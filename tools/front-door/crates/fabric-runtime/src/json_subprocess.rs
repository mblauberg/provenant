//! Bounded JSON-helper protocol reducer. The supervisor applies kill effects
//! and reports close only after streams close and the helper has been reaped.
//! This module never launches or signals a process.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubprocessFailure { Exit, Timeout, Overflow, Malformed, Spawn }

#[derive(Debug, Error, PartialEq)]
#[error("{message}")]
pub struct SubprocessError {
    pub kind: SubprocessFailure,
    pub message: String,
    pub stderr: String,
}

#[derive(Debug, Error, PartialEq)]
pub enum JsonSubprocessError {
    #[error(transparent)]
    Subprocess(#[from] SubprocessError),
    /// AbortSignal reasons can be arbitrary values and must win over a timeout
    /// or overflow already recorded before close.
    #[error("JSON helper cancelled")]
    Cancelled { reason: Value },
}

pub enum Event<'a> {
    Stdout(&'a [u8]), Stderr(&'a [u8]), Timeout,
    Abort(Value), SpawnFailed(String), Closed(Option<i32>),
}

#[derive(Debug, PartialEq)]
pub enum Effect { None, Kill, Complete(Result<CapturedResponse, JsonSubprocessError>) }

/// Opaque output until the caller's response type is available. Keeping bytes
/// also avoids claiming JavaScript-number parity for generic serde_json values.
#[derive(Debug, PartialEq)]
pub struct CapturedResponse { pub stdout: Vec<u8>, pub stderr: String }
impl CapturedResponse {
    pub fn parse<T: serde::de::DeserializeOwned>(&self, unavailable: &str) -> Result<T, SubprocessError> {
        serde_json::from_str(&String::from_utf8_lossy(&self.stdout)).map_err(|_|SubprocessError {
            kind:SubprocessFailure::Malformed,message:unavailable.into(),stderr:self.stderr.clone(),
        })
    }
}

pub struct JsonCapture {
    max_bytes: usize,
    unavailable: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    failure: Option<SubprocessFailure>,
    aborted: Option<Value>,
    settled: bool,
}
impl JsonCapture {
    pub fn new(max_bytes:usize,unavailable:impl Into<String>)->Self {
        Self { max_bytes,unavailable:unavailable.into(),stdout:Vec::new(),stderr:Vec::new(),failure:None,aborted:None,settled:false }
    }
    pub fn step(&mut self,event:Event<'_>)->Effect {
        if self.settled { return Effect::None; }
        match event {
            Event::Stdout(bytes)=> {
                if bytes.len()>self.max_bytes.saturating_sub(self.stdout.len()) || self.failure==Some(SubprocessFailure::Overflow) {
                    self.fail(SubprocessFailure::Overflow)
                } else { self.stdout.extend_from_slice(bytes); Effect::None }
            }
            Event::Stderr(bytes)=> {
                let count=bytes.len().min(self.max_bytes.saturating_sub(self.stderr.len()));
                self.stderr.extend_from_slice(&bytes[..count]); Effect::None
            }
            Event::Timeout=>self.fail(SubprocessFailure::Timeout),
            Event::Abort(reason)=>{ self.aborted=Some(reason); Effect::Kill },
            Event::SpawnFailed(message)=> {
                self.settled=true;
                Effect::Complete(Err(SubprocessError{kind:SubprocessFailure::Spawn,message,stderr:String::new()}.into()))
            }
            Event::Closed(code)=> {
                self.settled=true;
                if let Some(reason)=self.aborted.take() { return Effect::Complete(Err(JsonSubprocessError::Cancelled{reason})); }
                if let Some(kind)=self.failure { return Effect::Complete(Err(SubprocessError{kind,message:self.unavailable.clone(),stderr:String::new()}.into())); }
                let stderr=String::from_utf8_lossy(&self.stderr).into_owned();
                if code!=Some(0) { return Effect::Complete(Err(SubprocessError{kind:SubprocessFailure::Exit,message:self.unavailable.clone(),stderr}.into())); }
                Effect::Complete(Ok(CapturedResponse{stdout:std::mem::take(&mut self.stdout),stderr}))
            }
        }
    }
    fn fail(&mut self,kind:SubprocessFailure)->Effect { self.failure.get_or_insert(kind); Effect::Kill }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn finish(capture:&mut JsonCapture,code:Option<i32>)->Result<CapturedResponse,JsonSubprocessError> {
        match capture.step(Event::Closed(code)) { Effect::Complete(result)=>result,_=>panic!("no completion") }
    }
    #[test]
    fn split_utf8_and_json_round_trip() {
        let mut capture=JsonCapture::new(100,"unavailable");
        capture.step(Event::Stdout(b"{\"got\":\"caf\xc3")); capture.step(Event::Stdout(b"\xa9\"}"));
        assert_eq!(finish(&mut capture,Some(0)).unwrap().parse::<Value>("unavailable").unwrap(),json!({"got":"café"}));
    }
    #[test]
    fn early_stdin_close_is_not_an_event_and_exit_decides_result() {
        let mut capture=JsonCapture::new(100,"unavailable");
        capture.step(Event::Stderr(b"diagnostic"));
        assert_eq!(finish(&mut capture,Some(3)).unwrap_err(),SubprocessError{kind:SubprocessFailure::Exit,message:"unavailable".into(),stderr:"diagnostic".into()}.into());
        let mut capture=JsonCapture::new(100,"unavailable"); capture.step(Event::Stdout(b"{\"early\":true}"));
        assert_eq!(finish(&mut capture,Some(0)).unwrap().parse::<Value>("unavailable").unwrap(),json!({"early":true}));
    }
    #[test]
    fn malformed_and_empty_answers_keep_stderr_prefix() {
        for stdout in [b"not json".as_slice(),b""] {
            let mut capture=JsonCapture::new(10,"unavailable");
            capture.step(Event::Stderr(b"0123456789tail")); capture.step(Event::Stdout(stdout));
            let error=finish(&mut capture,Some(0)).unwrap().parse::<Value>("unavailable").unwrap_err();
            assert_eq!(error.kind,SubprocessFailure::Malformed); assert_eq!(error.stderr,"0123456789");
        }
    }
    #[test]
    fn exact_cap_is_allowed_but_overflow_requests_kill_and_waits_for_close() {
        let mut capture=JsonCapture::new(2,"unavailable");
        assert_eq!(capture.step(Event::Stdout(b"{}")),Effect::None);
        assert_eq!(capture.step(Event::Stdout(b"x")),Effect::Kill);
        assert!(matches!(finish(&mut capture,None),Err(JsonSubprocessError::Subprocess(SubprocessError{kind:SubprocessFailure::Overflow,..}))));
    }
    #[test]
    fn timeout_is_explicit_and_first_failure_wins() {
        let mut capture=JsonCapture::new(1,"unavailable");
        assert_eq!(capture.step(Event::Timeout),Effect::Kill);
        capture.step(Event::Stdout(b"too big"));
        assert!(matches!(finish(&mut capture,None),Err(JsonSubprocessError::Subprocess(SubprocessError{kind:SubprocessFailure::Timeout,..}))));
        assert_eq!(capture.step(Event::Timeout),Effect::None);
    }
    #[test]
    fn abort_reason_wins_over_failure_and_spawn_is_typed() {
        let mut capture=JsonCapture::new(1,"unavailable"); capture.step(Event::Timeout);
        capture.step(Event::Abort(json!({"message":"stop"})));
        assert_eq!(finish(&mut capture,None).unwrap_err(),JsonSubprocessError::Cancelled{reason:json!({"message":"stop"})});
        let mut capture=JsonCapture::new(1,"unavailable");
        assert_eq!(capture.step(Event::SpawnFailed("missing".into())),Effect::Complete(Err(SubprocessError{kind:SubprocessFailure::Spawn,message:"missing".into(),stderr:String::new()}.into())));
    }
}
