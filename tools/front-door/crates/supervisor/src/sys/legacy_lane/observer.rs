#![forbid(unsafe_code)]

use super::{
    files,
    group::ProviderGroup,
    platform,
    spec::Spec,
    state::{Event, ObserverReason, StopReason},
};
use crate::legacy_json::{self as json, Value};
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const QUEUE: usize = 256 * 1024;
const LINE: usize = 64 * 1024;
pub struct Observer {
    child: Option<Child>,
    input: Option<ChildStdin>,
    output: Option<ChildStdout>,
    queue: VecDeque<Vec<u8>>,
    offset: usize,
    bytes: usize,
    pending: Vec<u8>,
    pub lost: bool,
    pub dropped: u64,
    closing: bool,
}
impl Observer {
    pub fn start(spec: &Spec) -> Self {
        let mut o = Self {
            child: None,
            input: None,
            output: None,
            queue: VecDeque::new(),
            offset: 0,
            bytes: 0,
            pending: Vec::new(),
            lost: false,
            dropped: 0,
            closing: false,
        };
        if spec.observer.is_empty() {
            return o;
        }
        let mut command = Command::new(&spec.observer[0]);
        command
            .args(&spec.observer[1..])
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(&spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let spawned = platform::prepare_child(&mut command).and_then(|_| command.spawn());
        match spawned {
            Ok(mut child) => {
                o.input = child.stdin.take();
                o.output = child.stdout.take();
                let result = platform::nonblocking(o.input.as_ref().unwrap().as_raw_fd())
                    .and_then(|_| platform::nonblocking(o.output.as_ref().unwrap().as_raw_fd()));
                o.child = Some(child);
                if result.is_err() {
                    o.lose();
                }
            }
            Err(_) => o.lost = true,
        }
        o
    }
    pub fn pid(&self) -> Option<i32> {
        self.child.as_ref().map(|c| c.id() as i32)
    }
    fn lose(&mut self) {
        self.lost = true;
        self.input = None;
        self.output = None;
        self.queue.clear();
        self.bytes = 0;
    }
    pub fn enqueue(&mut self, stream: &str, line: &[u8]) {
        if self.child.is_none() || self.lost {
            return;
        }
        let data = format!(
            "{{\"s\":{},\"line\":{}}}\n",
            files::quote(stream),
            files::quote(&String::from_utf8_lossy(line))
        )
        .into_bytes();
        if self.bytes + data.len() > QUEUE {
            self.dropped += 1;
            self.lose();
            return;
        }
        self.bytes += data.len();
        self.queue.push_back(data);
    }
    pub fn drop_line(&mut self) {
        if self.child.is_some() {
            self.dropped += 1;
            self.lose();
        }
    }
    pub fn fds(&self) -> Vec<(RawFd, bool)> {
        let mut v = Vec::new();
        if !self.queue.is_empty() {
            if let Some(f) = &self.input {
                v.push((f.as_raw_fd(), true));
            }
        }
        if let Some(f) = &self.output {
            v.push((f.as_raw_fd(), false));
        }
        v
    }
    pub fn pump(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        // Bounded work, even if either peer produces continuously.
        for _ in 0..16 {
            let (Some(f), Some(front)) = (&mut self.input, self.queue.front()) else {
                break;
            };
            match f.write(&front[self.offset..]) {
                Ok(0) => {
                    self.lose();
                    break;
                }
                Ok(n) => {
                    self.offset += n;
                    self.bytes -= n;
                    if self.offset == front.len() {
                        self.queue.pop_front();
                        self.offset = 0;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.lose();
                    break;
                }
            }
        }
        if self.closing && self.queue.is_empty() {
            self.input = None;
        }
        let mut data = [0u8; 8192];
        for _ in 0..8 {
            let Some(f) = &mut self.output else {
                break;
            };
            match f.read(&mut data) {
                Ok(0) => {
                    self.output = None;
                    break;
                }
                Ok(n) => {
                    self.pending.extend_from_slice(&data[..n]);
                    while let Some(at) = self.pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<_> = self.pending.drain(..=at).collect();
                        if let Ok(v) = json::parse(&String::from_utf8_lossy(&line)) {
                            if v.get("terminal") == Some(&Value::Bool(true)) {
                                events.push(Event::Terminal);
                            }
                            if let Some(Value::Str(reason)) = v.get("stop") {
                                if let Some(reason) = ObserverReason::parse(reason) {
                                    events.push(Event::Stop(StopReason::Observer(reason)));
                                } else {
                                    self.lost = true;
                                }
                            }
                        } else {
                            self.lost = true;
                        }
                    }
                    if self.pending.len() > LINE {
                        self.lose();
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.lose();
                    break;
                }
            }
        }
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !self.closing || !status.success() || !self.queue.is_empty() {
                        self.lost = true;
                    }
                    self.child = None;
                    self.input = None;
                }
                Err(_) => self.lose(),
                _ => {}
            }
        }
        if self.lost || self.dropped > 0 {
            events.clear();
            events.push(Event::ObserverLost);
        }
        events
    }
    pub fn close(&mut self) {
        self.closing = true;
    }
    pub fn finished(&self) -> bool {
        self.child.is_none()
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.input = None;
        if let Some(mut child) = self.child.take() {
            ProviderGroup::reap_observer(&mut child);
        }
    }
}
/// Frames complete lines without letting a single provider line grow memory.
pub struct Lines {
    pending: Vec<u8>,
    discarding: bool,
}
impl Lines {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            discarding: false,
        }
    }
    pub fn write(&mut self, data: &[u8], stream: &str, o: &mut Observer) {
        for chunk in data.split_inclusive(|b| *b == b'\n') {
            let complete = chunk.last() == Some(&b'\n');
            if !self.discarding {
                if self.pending.len() + chunk.len() > LINE {
                    self.pending.clear();
                    self.discarding = true;
                    o.drop_line();
                } else {
                    self.pending.extend_from_slice(chunk);
                }
            }
            if complete {
                if !self.discarding {
                    o.enqueue(stream, &self.pending);
                }
                self.pending.clear();
                self.discarding = false;
            }
        }
    }
    pub fn finish(&mut self, stream: &str, o: &mut Observer) {
        if !self.pending.is_empty() {
            o.enqueue(stream, &self.pending);
            self.pending.clear();
        }
    }
}
