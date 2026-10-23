//! File publication has no process-group handle and cannot request a stop.
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
pub fn array(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(","))
}
pub fn nullable(s: Option<&str>) -> String {
    s.map(quote).unwrap_or_else(|| "null".into())
}
pub fn timestamp() -> Result<String, provenant_core::TimeError> {
    use provenant_core::Clock;
    crate::clock::SystemClock::new()
        .wall_now()
        .map(|time| time.to_string())
}
pub fn timestamp_epoch(sec: i64, millis: u32) -> Option<String> {
    let nanos = i32::try_from(millis.checked_mul(1_000_000)?).ok()?;
    jiff::Timestamp::new(sec, nanos)
        .ok()
        .map(|time| time.to_string())
}
/// Temporary S2 display contract for matching registered legacy owners.
/// The provider custody rewrite removes this string field with its importer.
pub fn canonical_start(epoch: u64) -> Option<String> {
    let seconds = i64::try_from(epoch).ok()?;
    let local = jiff::Timestamp::from_second(seconds)
        .ok()?
        .to_zoned(jiff::tz::TimeZone::system());
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let day = usize::try_from(local.weekday().to_sunday_zero_offset()).ok()?;
    let month = usize::try_from(local.month().checked_sub(1)?).ok()?;
    Some(format!(
        "{} {} {:2} {:02}:{:02}:{:02} {}",
        DAYS.get(day)?,
        MONTHS.get(month)?,
        local.day(),
        local.hour(),
        local.minute(),
        local.second(),
        local.year()
    ))
}
static TEMP: AtomicUsize = AtomicUsize::new(0);
#[derive(Debug)]
pub struct Attempt {
    dir: crate::sys::lane_files::LaneDirectory,
}
impl Attempt {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            dir: crate::sys::lane_files::LaneDirectory::open(path)?,
        })
    }
    pub fn create(&self, path: &Path) -> io::Result<File> {
        self.dir.create(path)
    }
    pub fn remove(&self, path: &str) {
        self.dir.remove(path);
    }
    pub fn exists(&self, path: &str) -> bool {
        self.dir.exists(path)
    }
    pub fn atomic(&self, name: &str, data: &[u8], no_replace: bool) -> io::Result<()> {
        fault(if no_replace { "exit" } else { "progress" })?;
        let temp = format!(
            ".supervisor-{}-{}",
            std::process::id(),
            TEMP.fetch_add(1, Ordering::Relaxed)
        );
        let result = (|| {
            let mut f = self.create(Path::new(&temp))?;
            f.write_all(data)?;
            f.sync_all()?;
            self.dir.publish_file(&temp, name, no_replace)?;
            if no_replace {
                // link is the publication boundary. A later durability error
                // must not retry link and misreport this visible record as lost.
                if let Err(error) = fault("exit-sync").and_then(|_| self.dir.sync()) {
                    eprintln!("provenant lane: exit record linked; directory sync: {error}");
                }
            } else {
                self.dir.sync()?;
            }
            Ok(())
        })();
        self.remove(&temp);
        result
    }
    pub fn reserve(&self) -> io::Result<()> {
        let mut f = self.create(Path::new("supervisor.reserve"))?;
        f.write_all(&[0u8; 65536])?;
        f.sync_all()
    }
    pub fn publish(&self, data: &[u8]) -> io::Result<()> {
        match self.atomic("supervisor.exit.json", data, true) {
            Err(e) if e.raw_os_error() == Some(libc::ENOSPC) => {
                self.remove("supervisor.reserve");
                self.atomic("supervisor.exit.json", data, true)
            }
            result => result,
        }
    }
}
#[cfg(feature = "fault-injection")]
pub fn fault(site: &str) -> io::Result<()> {
    // Test-only, scoped to this short-lived supervisor. No production knob.
    static ONCE: AtomicUsize = AtomicUsize::new(0);
    let selected = std::env::var("PROVENANT_LANE_FAULT").unwrap_or_default();
    if selected == site
        || (selected == format!("{site}-once") && ONCE.fetch_add(1, Ordering::Relaxed) == 0)
    {
        return Err(io::Error::from_raw_os_error(libc::ENOSPC));
    }
    Ok(())
}
#[cfg(not(feature = "fault-injection"))]
pub fn fault(_site: &str) -> io::Result<()> {
    Ok(())
}

pub struct Capture {
    file: Option<File>,
    cap: usize,
    tail: VecDeque<u8>,
    pub total: u64,
    pub error: Option<String>,
}
impl Capture {
    pub fn new(file: io::Result<File>, cap: usize) -> Self {
        let (file, error) = match file {
            Ok(f) => (Some(f), None),
            Err(e) => (None, Some(e.to_string())),
        };
        Self {
            file,
            cap,
            tail: VecDeque::new(),
            total: 0,
            error,
        }
    }
    fn fail(&mut self, error: io::Error) {
        if self.error.is_none() {
            self.error = Some(error.to_string());
        }
        self.file = None;
    }
    pub fn write(&mut self, data: &[u8]) {
        let room = self
            .cap
            .saturating_sub(self.total.min(usize::MAX as u64) as usize);
        if let Some(f) = &mut self.file {
            let result = fault("capture").and_then(|_| {
                if room > 0 {
                    f.write_all(&data[..room.min(data.len())])
                } else {
                    crate::sys::lane_files::touch(f)?;
                    Ok(())
                }
            });
            if let Err(e) = result {
                self.fail(e);
            }
        }
        self.total = self.total.saturating_add(data.len() as u64);
        let half = self.cap - self.cap / 2;
        let keep = &data[data.len().saturating_sub(half)..];
        let excess = (self.tail.len() + keep.len()).saturating_sub(half);
        self.tail.drain(..excess);
        self.tail.extend(keep);
    }
    pub fn finish(&mut self) {
        if self.total > self.cap as u64 {
            if let Some(f) = &mut self.file {
                let result = (|| {
                    f.seek(SeekFrom::Start((self.cap / 2) as u64))?;
                    f.write_all(self.tail.make_contiguous())?;
                    f.set_len(self.cap as u64)
                })();
                if let Err(e) = result {
                    self.fail(e);
                }
            }
        }
    }
    pub fn json(&self) -> String {
        format!(
            "{{\"bytes\":{},\"truncated\":{}}}",
            self.total,
            self.total > self.cap as u64
        )
    }
}
#[derive(Default)]
pub struct Notes {
    pub items: Vec<String>,
    pub omitted: usize,
}
impl Notes {
    pub fn add(&mut self, s: impl Into<String>) {
        if self.items.len() < 32 {
            let mut s = s.into();
            if s.len() > 256 {
                let mut n = 256;
                while !s.is_char_boundary(n) {
                    n -= 1;
                }
                s.truncate(n);
            }
            self.items.push(s);
        } else {
            self.omitted += 1;
        }
    }
    pub fn result<T>(&mut self, label: &str, r: io::Result<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.add(format!("{label}: {e}"));
                None
            }
        }
    }
}
pub struct ExitRecord {
    pub pid: Option<i32>,
    pub started: String,
    pub ended: String,
    pub progress: String,
    pub exit: Option<i32>,
    pub signal: Option<i32>,
    pub forced: Option<String>,
    pub terminal: bool,
    pub reaped: Vec<String>,
    pub spared: Vec<String>,
    pub warnings: Notes,
    pub io: Notes,
    pub out: String,
    pub err: String,
    pub observer_lost: bool,
    pub observer_dropped: u64,
}
impl ExitRecord {
    pub fn encode(&self) -> String {
        let mut sizes = [
            self.reaped.len(),
            self.spared.len(),
            self.warnings.items.len(),
            self.io.items.len(),
        ];
        loop {
            let list = |v: &[String], n| array(v[..n].iter().cloned());
            let s = format!(
                concat!(
                    "{{\"schema\":\"provenant.lane.exit.v1\",\"pid\":{pid},\"pgid\":{pid},",
                    "\"started_at\":{start},\"ended_at\":{end},\"last_progress_at\":{progress},\"exit\":{exit},\"signal\":{signal},",
                    "\"forced\":{forced},\"terminal_event_seen\":{terminal},\"reaped\":{reaped},\"spared\":{spared},",
                    "\"census_warnings\":{warnings},\"io_errors\":{io},\"capture\":{{\"events\":{out},\"stderr\":{err}}},",
                    "\"observer_lost\":{lost},\"observer_dropped\":{dropped},",
                    "\"reaped_truncated\":{rt},\"spared_truncated\":{st},\"census_warnings_truncated\":{wt},\"io_errors_truncated\":{it}}}\n"
                ),
                pid = self
                    .pid
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "null".into()),
                start = quote(&self.started),
                end = quote(&self.ended),
                progress = quote(&self.progress),
                exit = self
                    .exit
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "null".into()),
                signal = self
                    .signal
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "null".into()),
                forced = nullable(self.forced.as_deref()),
                terminal = self.terminal,
                reaped = list(&self.reaped, sizes[0]),
                spared = list(&self.spared, sizes[1]),
                warnings = array(self.warnings.items[..sizes[2]].iter().map(|s| quote(s))),
                io = array(self.io.items[..sizes[3]].iter().map(|s| quote(s))),
                out = self.out,
                err = self.err,
                lost = self.observer_lost,
                dropped = self.observer_dropped,
                rt = self.reaped.len() - sizes[0],
                st = self.spared.len() - sizes[1],
                wt = self.warnings.items.len() - sizes[2] + self.warnings.omitted,
                it = self.io.items.len() - sizes[3] + self.io.omitted
            );
            if s.len() <= 16 * 1024 - 64 {
                return s;
            }
            if let Some((i, _)) = sizes
                .iter()
                .enumerate()
                .filter(|(_, n)| **n > 0)
                .max_by_key(|(_, n)| **n)
            {
                sizes[i] /= 2;
            } else {
                unreachable!("scalar fields are bounded");
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_link_never_replaces_an_existing_record() {
        let path = std::env::temp_dir().join(format!("lane-link-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        let a = Attempt::open(&path).unwrap();
        a.atomic("supervisor.exit.json", b"first", true).unwrap();
        assert_eq!(
            a.atomic("supervisor.exit.json", b"second", true)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EEXIST)
        );
        assert_eq!(
            std::fs::read(path.join("supervisor.exit.json")).unwrap(),
            b"first"
        );
        drop(a);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn exit_record_is_valid_and_bounded_with_omission_counts() {
        let r = ExitRecord {
            pid: Some(1),
            started: timestamp().unwrap(),
            ended: timestamp().unwrap(),
            progress: timestamp().unwrap(),
            exit: Some(0),
            signal: None,
            forced: None,
            terminal: false,
            reaped: vec![format!("{{\"pid\":1,\"command\":{}}}", quote(&"x".repeat(80))); 10000],
            spared: vec![],
            warnings: Notes::default(),
            io: Notes::default(),
            out: "{}".into(),
            err: "{}".into(),
            observer_lost: false,
            observer_dropped: 0,
        };
        let text = r.encode();
        assert!(text.len() <= 16384);
        let v = crate::legacy_json::parse(&text).unwrap();
        assert!(
            matches!(v.get("reaped_truncated"),Some(crate::legacy_json::Value::Int(n)) if *n>0)
        );
    }
}
