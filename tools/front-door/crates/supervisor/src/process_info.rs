//! Process records and pure decoders. OS discovery belongs to the observation adapter.

use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRow {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub started: String,
    pub command: String,
    pub zombie: bool,
    pub tty: String,
    pub status: u32,
}

impl ProcessRow {
    pub fn identity(&self) -> (i32, &str) {
        (self.pid, &self.started)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub lstart: String,
    pub elapsed: String,
    pub cpu: String,
    pub command: String,
    pub comm: String,
    pub stat: String,
    pub rss: i64,
    pub tty: String,
    pub uid: i64,
    pub start_epoch: Option<f64>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProcessParseError {
    #[error("malformed process field {0}")]
    Field(&'static str),
    #[error("invalid process duration")]
    Duration,
}

pub fn linux_process_row(pid: i32, stat: &[u8]) -> Result<Option<ProcessRow>, ProcessParseError> {
    let stat = String::from_utf8_lossy(stat);
    let Some(end) = stat.rfind(") ") else {
        return Ok(None);
    };
    let fields: Vec<_> = stat[end + 2..].split_whitespace().collect();
    if fields.len() < 20 {
        return Ok(None);
    }
    let begin = stat.find('(').map_or(0, |index| index + 1);
    Ok(Some(ProcessRow {
        pid,
        ppid: fields[1]
            .parse()
            .map_err(|_| ProcessParseError::Field("ppid"))?,
        pgid: fields[2]
            .parse()
            .map_err(|_| ProcessParseError::Field("pgid"))?,
        started: fields[19].into(),
        command: stat[begin..end].into(),
        zombie: fields[0] == "Z",
        tty: "??".into(),
        status: 0,
    }))
}

/// Python shlex.join: quote only arguments outside the ASCII shell-safe set.
pub fn shell_join<'a>(args: impl IntoIterator<Item = &'a str>) -> String {
    // UTF-8 input plus ASCII quoting always produces UTF-8 output.
    shell_join_os(args.into_iter().map(OsStr::new))
        .to_string_lossy()
        .into_owned()
}

/// Decode KERN_PROCARGS2 without including the following environment block.
/// Match Python fsdecode/fsencode round trips by retaining non-UTF-8 OS bytes.
pub fn darwin_argv(raw: &[u8]) -> OsString {
    let Some(count) = raw
        .get(..4)
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map(i32::from_ne_bytes)
    else {
        return OsString::new();
    };
    if !(1..65536).contains(&count) {
        return OsString::new();
    }
    let Some(executable_end) = raw[4..].iter().position(|&byte| byte == 0) else {
        return OsString::new();
    };
    let mut cursor = 4 + executable_end + 1;
    while raw.get(cursor) == Some(&0) {
        cursor += 1;
    }
    let mut args = Vec::new();
    for _ in 0..count {
        let Some(end) = raw[cursor..].iter().position(|&byte| byte == 0) else {
            break;
        };
        args.push(OsStr::from_bytes(&raw[cursor..cursor + end]));
        cursor += end + 1;
    }
    shell_join_os(args)
}

pub fn shell_join_os<'a>(args: impl IntoIterator<Item = &'a OsStr>) -> OsString {
    let mut output = Vec::new();
    for (index, arg) in args.into_iter().enumerate() {
        if index != 0 {
            output.push(b' ');
        }
        let arg = arg.as_bytes();
        if !arg.is_empty()
            && arg
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(c))
        {
            output.extend_from_slice(arg);
        } else {
            output.push(b'\'');
            for &byte in arg {
                if byte == b'\'' {
                    output.extend_from_slice(b"'\"'\"'");
                } else {
                    output.push(byte);
                }
            }
            output.push(b'\'');
        }
    }
    OsString::from_vec(output)
}

pub fn duration(seconds: f64, elapsed: bool, darwin: bool) -> Result<String, ProcessParseError> {
    if !seconds.is_finite() || seconds >= i64::MAX as f64 {
        return Err(ProcessParseError::Duration);
    }
    let seconds = seconds.max(0.0);
    let whole = seconds as i64;
    let days = whole / 86400;
    let hours = whole / 3600 % 24;
    let minutes = whole / 60 % 60;
    let secs = whole % 60;
    if !elapsed {
        let hundredths = (((seconds - whole as f64) * 100.0 + 0.5) as i64).min(99);
        let total_hours = whole / 3600;
        return Ok(if darwin {
            format!("{}:{secs:02}.{hundredths:02}", whole / 60)
        } else if total_hours > 0 {
            format!("{total_hours}:{minutes:02}:{secs:02}.{hundredths:02}")
        } else {
            format!("{minutes}:{secs:02}.{hundredths:02}")
        });
    }
    Ok(if days > 0 {
        format!("{days}-{hours:02}:{minutes:02}:{secs:02}")
    } else if hours > 0 {
        format!("{hours:02}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_command_parentheses_do_not_shift_identity_fields() {
        let raw = b"42 (name ) with (parens)) Z 2 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1234";
        let row = linux_process_row(42, raw).unwrap().unwrap();
        assert_eq!(row.identity(), (42, "1234"));
        assert_eq!((row.ppid, row.pgid), (2, 3));
        assert_eq!(row.command, "name ) with (parens)");
        assert!(row.zombie);
        assert!(linux_process_row(42, b"bad stat").unwrap().is_none());
    }

    #[test]
    fn argv_does_not_leak_the_environment() {
        let mut raw = 3_i32.to_ne_bytes().to_vec();
        raw.extend(b"/bin/tool\0\0tool\0two words\0a'b\0SECRET=value\0");
        assert_eq!(darwin_argv(&raw), "tool 'two words' 'a'\"'\"'b'");
        assert_eq!(darwin_argv(&[0; 4]), "");
        let mut raw = 1_i32.to_ne_bytes().to_vec();
        raw.extend(b"/bin/tool\0\0bad\xffname\0SECRET=value\0");
        assert_eq!(darwin_argv(&raw).as_bytes(), b"'bad\xffname'");
        assert_eq!(shell_join(["", "λ", "a\nb"]), "'' 'λ' 'a\nb'");
    }

    #[test]
    fn duration_uses_unbounded_macos_minutes_and_clamps_negative_time() {
        assert_eq!(duration(4185.86, false, true).unwrap(), "69:45.86");
        assert_eq!(duration(4185.86, false, false).unwrap(), "1:09:45.86");
        assert_eq!(duration(90061.0, true, false).unwrap(), "1-01:01:01");
        assert_eq!(duration(-1.0, true, false).unwrap(), "00:00");
        assert_eq!(
            duration(f64::NAN, true, false),
            Err(ProcessParseError::Duration)
        );
    }

    #[test]
    fn differential_duration_and_shell_quoting_match_python() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::python(
            &root,
            r#"
import json, shlex
from skills.orchestrate.scripts import process_info as p
out = []
for platform in ['darwin', 'linux']:
    p.sys.platform = platform
    for elapsed in [False, True]:
        for value in [-1, 0, 0.9999, 59.995, 60, 3599.999, 3600, 4185.86, 90061]:
            out.append(p._duration(value, elapsed=elapsed))
out.append(shlex.join(['', 'a b', "a'b", 'λ', 'a\nb', '/ok', 'a@b']))
print(json.dumps(out))
"#,
        ) else {
            return;
        };
        let mut actual = Vec::new();
        for darwin in [true, false] {
            for elapsed in [false, true] {
                for value in [
                    -1.0, 0.0, 0.9999, 59.995, 60.0, 3599.999, 3600.0, 4185.86, 90061.0,
                ] {
                    actual.push(duration(value, elapsed, darwin).unwrap());
                }
            }
        }
        actual.push(shell_join(["", "a b", "a'b", "λ", "a\nb", "/ok", "a@b"]));
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }
}

/// Decode /bin/ps's eight-column fallback after the adapter checks exit status.
/// A start time is mandatory; unavailable identity is never fabricated.
pub fn ps_process_output(
    pid: i32,
    stdout: &str,
    lstart: Option<&str>,
    start_epoch: Option<f64>,
) -> Result<Option<ProcessInfo>, ProcessParseError> {
    let text = stdout.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let mut rest = text;
    let mut values = Vec::new();
    for _ in 0..7 {
        let Some(end) = rest.find(char::is_whitespace) else {
            return Ok(None);
        };
        values.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    if rest.is_empty() {
        return Ok(None);
    }
    let Some(lstart) = lstart.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let first = rest
        .split_whitespace()
        .next()
        .ok_or(ProcessParseError::Field("command"))?;
    let comm = std::path::Path::new(first)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    Ok(Some(ProcessInfo {
        pid,
        ppid: values[1]
            .parse()
            .map_err(|_| ProcessParseError::Field("ppid"))?,
        pgid: values[2]
            .parse()
            .map_err(|_| ProcessParseError::Field("pgid"))?,
        lstart: lstart.into(),
        elapsed: values[3].into(),
        cpu: values[4].into(),
        stat: values[5].into(),
        rss: values[6]
            .parse()
            .map_err(|_| ProcessParseError::Field("rss"))?,
        command: rest.into(),
        comm: comm.into(),
        tty: "??".into(),
        uid: -1,
        start_epoch,
    }))
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    #[test]
    fn fallback_keeps_command_spacing_and_requires_start_identity() {
        let output = " 42  2 3  00:10 0:00.01 S 123 /bin/tool  two words  \n";
        let row = ps_process_output(42, output, Some("Mon Jan  1 00:00:00 2024"), None)
            .unwrap()
            .unwrap();
        assert_eq!(row.command, "/bin/tool  two words");
        assert_eq!(row.comm, "tool");
        assert_eq!((row.ppid, row.pgid, row.rss, row.uid), (2, 3, 123, -1));
        assert_eq!(row.tty, "??");
        assert!(ps_process_output(42, output, None, None).unwrap().is_none());
        assert!(ps_process_output(42, "42 2", Some("start"), None)
            .unwrap()
            .is_none());
        assert!(
            ps_process_output(42, "42 bad 3 00:10 0:00 S 123 command", Some("start"), None)
                .is_err()
        );
    }

    #[test]
    fn process_record_roundtrips_all_fields_including_null_epoch() {
        let row = ps_process_output(42, "42 2 3 00:10 0:00 S 123 /bin/tool", Some("start"), None)
            .unwrap()
            .unwrap();
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 13);
        assert_eq!(json["start_epoch"], serde_json::Value::Null);
        assert_eq!(serde_json::from_value::<ProcessInfo>(json).unwrap(), row);
    }
}

#[cfg(test)]
mod linux_fixture_tests {
    use super::*;

    #[test]
    fn differential_stat_rows_match_python_on_fixture_proc_tree() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::python(
            &root,
            r#"
import json, tempfile
from dataclasses import asdict
from pathlib import Path
from skills.orchestrate.scripts.process_info import _linux_process_row
cases = [b'42 (name ) with (parens)) Z 2 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1234',
         b'42 (bad\xffname) S 2 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 5678', b'bad stat', b'42 (short) S 2 3']
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    (root / '42').mkdir()
    rows = []
    for raw in cases:
        (root / '42' / 'stat').write_bytes(raw)
        row = _linux_process_row(42, root)
        rows.append(asdict(row) if row is not None else None)
    print(json.dumps(rows))
"#,
        ) else {
            return;
        };
        let cases: &[&[u8]] = &[
            b"42 (name ) with (parens)) Z 2 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1234",
            b"42 (bad\xffname) S 2 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 5678",
            b"bad stat",
            b"42 (short) S 2 3",
        ];
        let actual = cases
            .iter()
            .map(|raw| linux_process_row(42, raw).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }
}
