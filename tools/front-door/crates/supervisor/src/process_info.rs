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
    #[serde(with = "os_text")]
    pub command: OsString,
    #[serde(with = "os_text")]
    pub comm: OsString,
    pub stat: String,
    pub rss: i64,
    pub tty: String,
    pub uid: i64,
    pub start_epoch: Option<f64>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProcessParseError {
    #[error("Linux boot time unavailable")]
    BootTimeUnavailable,
    #[error("process number out of range: {0}")]
    NumberOutOfRange(&'static str),
    #[error("malformed process field {0}")]
    Field(&'static str),
    #[error("invalid process duration")]
    Duration,
}

fn integer_field(text: &str, field: &'static str) -> Result<i64, ProcessParseError> {
    let integer = crate::pending::Integer::parse(text).ok_or(ProcessParseError::Field(field))?;
    integer
        .as_i64()
        .ok_or(ProcessParseError::NumberOutOfRange(field))
}

fn pid_field(text: &str, field: &'static str) -> Result<i32, ProcessParseError> {
    i32::try_from(integer_field(text, field)?)
        .map_err(|_| ProcessParseError::NumberOutOfRange(field))
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
        ppid: pid_field(fields[1], "ppid")?,
        pgid: pid_field(fields[2], "pgid")?,
        started: fields[19].into(),
        command: stat.get(begin..end).unwrap_or("").into(),
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
    use crate::pending::{divmod_decimal, float_whole};
    let seconds = seconds.max(0.0);
    if !seconds.is_finite() {
        return Err(ProcessParseError::Duration);
    }
    let whole = float_whole(seconds);
    let (total_minutes, secs) = divmod_decimal(&whole, 60);
    let (total_hours, minutes) = divmod_decimal(&total_minutes, 60);
    let (days, hours) = divmod_decimal(&total_hours, 24);
    if !elapsed {
        let hundredths = (((seconds - seconds.trunc()) * 100.0 + 0.5) as u32).min(99);
        return Ok(if darwin {
            format!("{total_minutes}:{secs:02}.{hundredths:02}")
        } else if total_hours != "0" {
            format!("{total_hours}:{minutes:02}:{secs:02}.{hundredths:02}")
        } else {
            format!("{minutes}:{secs:02}.{hundredths:02}")
        });
    }
    Ok(if days != "0" {
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
    fn pid_fields_accept_python_tokens_and_refuse_range_overflow() {
        let raw = "42 (tool) S +٢_٠ 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1234";
        assert_eq!(
            linux_process_row(42, raw.as_bytes()).unwrap().unwrap().ppid,
            20
        );
        let raw = raw.replace("+٢_٠", "2147483648");
        assert!(matches!(
            linux_process_row(42, raw.as_bytes()),
            Err(ProcessParseError::NumberOutOfRange("ppid"))
        ));
    }

    #[test]
    fn reversed_stat_command_delimiters_produce_an_empty_command() {
        let raw = b"42 ) S 2 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 (";
        let row = linux_process_row(42, raw).unwrap().unwrap();
        assert_eq!(row.command, "");
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
        assert_eq!(duration(f64::NAN, true, false).unwrap(), "00:00");
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
    let text = stdout.trim_matches(provenant_core::py::python_whitespace);
    if text.is_empty() {
        return Ok(None);
    }
    let mut rest = text;
    let mut values = Vec::new();
    for _ in 0..7 {
        let Some(end) = rest.find(provenant_core::py::python_whitespace) else {
            return Ok(None);
        };
        values.push(&rest[..end]);
        rest = rest[end..].trim_start_matches(provenant_core::py::python_whitespace);
    }
    if rest.is_empty() {
        return Ok(None);
    }
    let Some(lstart) = lstart.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let first = rest
        .split(provenant_core::py::python_whitespace)
        .find(|value| !value.is_empty())
        .ok_or(ProcessParseError::Field("command"))?;
    // pathlib preserves a final '..'; std::path::Path::file_name excludes it.
    let comm = first
        .split('/')
        .rev()
        .find(|part| !part.is_empty() && *part != ".")
        .unwrap_or("");
    let numeric = (|| {
        Ok((
            pid_field(values[1], "ppid")?,
            pid_field(values[2], "pgid")?,
            integer_field(values[6], "rss")?,
        ))
    })();
    let (ppid, pgid, rss) = match numeric {
        Ok(values) => values,
        Err(ProcessParseError::Field(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(ProcessInfo {
        pid,
        ppid,
        pgid,
        lstart: lstart.into(),
        elapsed: values[3].into(),
        cpu: values[4].into(),
        stat: values[5].into(),
        rss,
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
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            ps_process_output(
                42,
                "42 2147483648 3 00:10 0:00 S 123 command",
                Some("start"),
                None
            ),
            Err(ProcessParseError::NumberOutOfRange("ppid"))
        ));
        for output in [
            "42 2 bad 00:10 0:00 S 123 command",
            "42 2 3 00:10 0:00 S bad command",
        ] {
            assert!(ps_process_output(42, output, Some("start"), None)
                .unwrap()
                .is_none());
        }
        let row = ps_process_output(42, "42 2 3 00:10 0:00 S 123 ../", Some("start"), None)
            .unwrap()
            .unwrap();
        assert_eq!(row.comm, "..");
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
         b'42 (bad\xffname) S 2 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 5678', b'bad stat', b'42 (short) S 2 3', b'42 ) S 2 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 (']
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
            b"42 ) S 2 3 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 (",
        ];
        let actual = cases
            .iter()
            .map(|raw| linux_process_row(42, raw).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("{operation}: {path}")]
    Io {
        operation: &'static str,
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Parse(#[from] ProcessParseError),
    #[error("{0}")]
    Census(String),
    #[error(transparent)]
    Bounded(#[from] crate::bounded::BoundedError),
}

fn read_error(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> ProcessError + '_ {
    |source| ProcessError::Io {
        operation: "read process observation",
        path: path.into(),
        source,
    }
}

pub fn linux_read_row(
    pid: i32,
    proc_root: &std::path::Path,
) -> Result<Option<ProcessRow>, ProcessError> {
    let path = proc_root.join(pid.to_string()).join("stat");
    linux_process_row(pid, &std::fs::read(&path).map_err(read_error(&path))?).map_err(Into::into)
}

/// A missing task/children interface requests a full census. A process that
/// disappears during traversal is simply omitted. Include previously seen
/// children so a reparented descendant remains observable.
pub fn linux_tree_snapshot(
    root_pid: i32,
    known_pids: &[i32],
    proc_root: &std::path::Path,
) -> Result<Option<std::collections::BTreeMap<i32, ProcessRow>>, ProcessError> {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let mut queue = VecDeque::from([root_pid]);
    queue.extend(known_pids);
    let mut visited = BTreeSet::new();
    let mut rows = BTreeMap::new();
    while let Some(pid) = queue.pop_front() {
        if !visited.insert(pid) {
            continue;
        }
        let row = match linux_read_row(pid, proc_root) {
            Ok(Some(row)) => row,
            Ok(None) | Err(ProcessError::Io { .. }) => continue,
            Err(error) => return Err(error),
        };
        rows.insert(pid, row);
        let task = proc_root.join(pid.to_string()).join("task");
        let Ok(threads) = std::fs::read_dir(task) else {
            return Ok(None);
        };
        for thread in threads {
            let Ok(thread) = thread else {
                return Ok(None);
            };
            let Ok(children) = std::fs::read_to_string(thread.path().join("children")) else {
                return Ok(None);
            };
            for child in children.split_whitespace() {
                queue.push_back(pid_field(child, "child pid")?);
            }
        }
    }
    Ok(Some(rows))
}

pub fn tty_name(device: u32, dev_root: &std::path::Path) -> OsString {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    if device == 0 || device == u32::MAX {
        return "??".into();
    }
    if let Ok(entries) = std::fs::read_dir(dev_root) {
        for entry in entries.flatten() {
            if entry.path().metadata().is_ok_and(|metadata| {
                metadata.file_type().is_char_device() && metadata.rdev() as u32 == device
            }) {
                return entry.file_name();
            }
        }
    }
    device.to_string().into()
}

pub fn linux_boot_time(proc_root: &std::path::Path) -> Result<i64, ProcessError> {
    let path = proc_root.join("stat");
    let text = std::fs::read_to_string(&path).map_err(read_error(&path))?;
    for line in text.lines() {
        if line.starts_with("btime ") {
            let value = line
                .split_whitespace()
                .nth(1)
                .ok_or(ProcessParseError::Field("boot time"))?;
            return integer_field(value, "boot time").map_err(Into::into);
        }
    }
    Err(ProcessParseError::BootTimeUnavailable.into())
}

#[cfg(target_os = "macos")]
pub fn darwin_process_row(pid: i32) -> Option<ProcessRow> {
    let info = crate::sys::process::darwin_bsd(pid)?;
    Some(ProcessRow {
        pid,
        ppid: info.ppid,
        pgid: info.pgid,
        started: format!("{}.{:06}", info.start_sec, info.start_usec),
        command: info.command,
        zombie: info.status == 5,
        tty: tty_name(info.device, std::path::Path::new("/dev"))
            .to_string_lossy()
            .into_owned(),
        status: info.status,
    })
}

pub fn probe_process_row(pid: i32) -> Result<Option<ProcessRow>, ProcessError> {
    #[cfg(target_os = "macos")]
    {
        Ok(darwin_process_row(pid))
    }
    #[cfg(target_os = "linux")]
    {
        readable_observation(linux_read_row(pid, std::path::Path::new("/proc")))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        Ok(None)
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use std::path::Path;

    fn fixture_row(root: &Path, pid: i32, children: &str) {
        let directory = root.join(pid.to_string());
        std::fs::create_dir_all(directory.join("task").join(pid.to_string())).unwrap();
        std::fs::write(
            directory.join("stat"),
            format!("{pid} (tool) S 1 2 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1234"),
        )
        .unwrap();
        std::fs::write(
            directory
                .join("task")
                .join(pid.to_string())
                .join("children"),
            children,
        )
        .unwrap();
    }

    #[test]
    fn task_children_include_known_reparented_nodes_and_break_cycles() {
        let root = tempfile::tempdir().unwrap();
        fixture_row(root.path(), 42, "43 44");
        fixture_row(root.path(), 43, "42");
        fixture_row(root.path(), 45, "");
        let rows = linux_tree_snapshot(42, &[45, 45, 999], root.path())
            .unwrap()
            .unwrap();
        assert_eq!(rows.keys().copied().collect::<Vec<_>>(), [42, 43, 45]);
        std::fs::remove_file(root.path().join("43/task/43/children")).unwrap();
        assert!(linux_tree_snapshot(42, &[], root.path()).unwrap().is_none());
    }

    #[test]
    fn boot_time_and_tty_unknowns_preserve_source_errors_and_values() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("stat"), "cpu 1 2 3\nbtime 1704067200\n").unwrap();
        assert_eq!(linux_boot_time(root.path()).unwrap(), 1704067200);
        std::fs::write(root.path().join("stat"), "cpu 1 2 3\n").unwrap();
        assert!(matches!(
            linux_boot_time(root.path()),
            Err(ProcessError::Parse(ProcessParseError::BootTimeUnavailable))
        ));
        assert_eq!(tty_name(0, root.path()), "??");
        assert_eq!(tty_name(u32::MAX, root.path()), "??");
        assert_eq!(tty_name(12345, root.path()), "12345");
    }

    #[test]
    fn canonical_local_dates_match_python_with_explicit_epochs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::python(
            &root,
            r#"
import json
from skills.orchestrate.scripts.process_info import _lstart
print(json.dumps([_lstart(t) for t in [0, 1704067200, 1735689599, -1, -0.0000001, 1704067199.9999998]]))
"#,
        ) else {
            return;
        };
        let actual: Vec<_> = [
            0.0,
            1704067200.0,
            1735689599.0,
            -1.0,
            -0.0000001,
            1704067199.9999998,
        ]
        .into_iter()
        .map(|epoch| crate::sys::process::lstart(epoch).unwrap())
        .collect();
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sdk_facts_layout_and_mach_timebase_are_read_without_global_state() {
        use crate::sys::process::{darwin_facts_bytes, mach_tick_seconds};
        let mut raw = vec![0_u8; 648];
        raw[..8].copy_from_slice(&1704067200_i64.to_ne_bytes());
        raw[8..16].copy_from_slice(&123456_i64.to_ne_bytes());
        for (offset, value) in [(40, 42_i32), (420, 501), (560, 2), (564, 3)] {
            raw[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
        }
        let facts = darwin_facts_bytes(42, &raw).unwrap();
        assert_eq!(
            (facts.pid, facts.ppid, facts.pgid, facts.uid),
            (42, 2, 3, 501)
        );
        assert_eq!(facts.start_epoch.to_bits(), 1704067200.123456_f64.to_bits());
        assert!(darwin_facts_bytes(43, &raw).is_none());
        assert!(darwin_facts_bytes(42, &raw[..647]).is_none());
        assert!(mach_tick_seconds() > 0.0);
    }
}

// OS text stays lossless until an explicitly UTF-8 record edge. Refuse invalid
// UTF-8 at that edge rather than rewriting its bytes with replacement characters.
mod os_text {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::ffi::{OsStr, OsString};
    pub fn serialize<S: Serializer>(text: &OsStr, serializer: S) -> Result<S::Ok, S::Error> {
        let text = text
            .to_str()
            .ok_or_else(|| serde::ser::Error::custom("process text is not UTF-8"))?;
        serializer.serialize_str(text)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<OsString, D::Error> {
        String::deserialize(deserializer).map(Into::into)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LinuxClock {
    pub boot_epoch: i64,
    pub ticks: i64,
    pub page_size: i64,
    pub now_epoch: f64,
}

/// Match Python's unreadable/malformed observation policy, but preserve v4's
/// explicit integer-range refusal through lookup and census callers.
fn readable_observation<T>(
    result: Result<Option<T>, ProcessError>,
) -> Result<Option<T>, ProcessError> {
    match result {
        Err(
            ProcessError::Io { .. }
            | ProcessError::Parse(
                ProcessParseError::Field(_)
                | ProcessParseError::Duration
                | ProcessParseError::BootTimeUnavailable,
            ),
        ) => Ok(None),
        result => result,
    }
}

/// The filesystem and clock are values so the Linux path can be exercised on
/// macOS too. Unreadable processes are absent; integer overflow stays typed.
pub fn linux_process_at(
    pid: i32,
    proc_root: &std::path::Path,
    clock: LinuxClock,
) -> Result<Option<ProcessInfo>, ProcessError> {
    readable_observation((|| {
        use std::os::unix::fs::MetadataExt;
        let Some(row) = linux_read_row(pid, proc_root)? else {
            return Ok(None);
        };
        let directory = proc_root.join(pid.to_string());
        let path = directory.join("stat");
        let raw = std::fs::read(&path).map_err(read_error(&path))?;
        let text = String::from_utf8_lossy(&raw);
        let Some(end) = text.rfind(") ") else {
            return Ok(None);
        };
        let fields: Vec<_> = text[end + 2..].split_whitespace().collect();
        if clock.ticks <= 0 || clock.page_size <= 0 {
            return Ok(None);
        }
        let number = |index: usize| {
            integer_field(
                fields.get(index).ok_or(ProcessParseError::Field("stat"))?,
                "stat integer",
            )
        };
        let epoch = clock.boot_epoch as f64
            + integer_field(&row.started, "start ticks")? as f64 / clock.ticks as f64;
        let cpu = number(11)?
            .checked_add(number(12)?)
            .ok_or(ProcessParseError::NumberOutOfRange("CPU ticks"))? as f64
            / clock.ticks as f64;
        let rss = number(21)?
            .checked_mul(clock.page_size)
            .ok_or(ProcessParseError::NumberOutOfRange("rss"))?
            .div_euclid(1024);
        let command = std::fs::read(directory.join("cmdline"))
            .ok()
            .and_then(|bytes| {
                let start = bytes.iter().position(|&byte| byte != 0)?;
                let end = bytes.iter().rposition(|&byte| byte != 0)? + 1;
                Some(shell_join_os(
                    bytes[start..end]
                        .split(|&byte| byte == 0)
                        .map(OsStr::from_bytes),
                ))
            })
            .unwrap_or_else(|| row.command.clone().into());
        Ok((|| {
            Some(ProcessInfo {
                pid,
                ppid: row.ppid,
                pgid: row.pgid,
                lstart: crate::sys::process::lstart(epoch).ok()?,
                elapsed: duration(clock.now_epoch - epoch, true, false).ok()?,
                cpu: duration(cpu, false, false).ok()?,
                command,
                comm: row.command.into(),
                stat: fields.first()?.to_string(),
                rss,
                tty: if *fields.get(4)? == "0" {
                    "?".into()
                } else {
                    fields[4].into()
                },
                uid: directory.metadata().ok()?.uid().into(),
                start_epoch: Some(epoch),
            })
        })())
    })())
}

pub fn now_epoch() -> f64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(time) => time.as_secs_f64(),
        Err(error) => -error.duration().as_secs_f64(),
    }
}

pub fn process(pid: i32) -> Result<Option<ProcessInfo>, ProcessError> {
    process_at(pid, now_epoch())
}

pub fn process_at(pid: i32, now: f64) -> Result<Option<ProcessInfo>, ProcessError> {
    #[cfg(target_os = "macos")]
    {
        let Some(row) = darwin_process_row(pid) else {
            return Ok(None);
        };
        let task = crate::sys::process::darwin_task(pid)?;
        Ok((|| {
            let epoch = row.started.parse::<f64>().ok()?;
            let cpu = task.as_ref().map_or(0.0, |task| task.cpu_seconds);
            let stat = match row.status {
                4 => "T",
                5 => "Z",
                _ => {
                    if task.as_ref().is_some_and(|task| task.running) {
                        "R"
                    } else {
                        "S"
                    }
                }
            };
            let uid = crate::sys::process::darwin_bsd(pid).map_or(-1, |info| info.uid);
            let command = crate::sys::process::darwin_command(pid);
            Some(ProcessInfo {
                pid,
                ppid: row.ppid,
                pgid: row.pgid,
                lstart: crate::sys::process::lstart(epoch).ok()?,
                elapsed: duration(now - epoch, true, true).ok()?,
                cpu: duration(cpu, false, true).ok()?,
                command: if command.is_empty() {
                    row.command.clone().into()
                } else {
                    command
                },
                comm: row.command.into(),
                stat: stat.into(),
                rss: task.map_or(0, |task| task.rss_kib),
                tty: row.tty,
                uid,
                start_epoch: Some(epoch),
            })
        })())
    }
    #[cfg(target_os = "linux")]
    {
        let root = std::path::Path::new("/proc");
        if !root.is_dir() {
            return ps_process(pid);
        }
        let Some(boot_epoch) = readable_observation(linux_boot_time(root).map(Some))? else {
            return Ok(None);
        };
        let Some(clock) = (|| {
            Some(LinuxClock {
                boot_epoch,
                ticks: crate::sys::process::clock_ticks().ok()?,
                page_size: crate::sys::process::page_size().ok()?,
                now_epoch: now,
            })
        })() else {
            return Ok(None);
        };
        linux_process_at(pid, root, clock)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = now;
        ps_process(pid)
    }
}

fn ps_command(
    args: &[&str],
    timeout: f64,
    canonical: bool,
) -> Result<crate::capture::BoundedProcessResult, ProcessError> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    if canonical {
        for name in ["LC_ALL", "LANG"] {
            if let Some((_, value)) = env.iter_mut().find(|(key, _)| key == name) {
                *value = "C".into();
            } else {
                env.push((name.into(), "C".into()));
            }
        }
    }
    let mut options = crate::bounded::Options::new(std::path::Path::new("."), timeout);
    options.env = Some(&env);
    options.merge_stderr = false;
    let command: Vec<_> = std::iter::once("/bin/ps")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    let result = crate::bounded::run_bounded_text(&command, options)?;
    if result.timed_out {
        return Err(ProcessError::Census(
            "ps census failed: process timed out".into(),
        ));
    }
    if result.output_truncated || result.stderr_truncated == Some(true) {
        return Err(ProcessError::Census(
            "ps census output exceeds observation bound".into(),
        ));
    }
    Ok(result)
}

pub fn ps_start_time(pid: i32, canonical: bool) -> Option<String> {
    let pid = pid.to_string();
    let result = ps_command(&["-o", "lstart=", "-p", &pid], 2.0, canonical).ok()?;
    (result.returncode == 0).then(|| provenant_core::py::python_strip(&result.output).to_string())
}

pub fn ps_process(pid: i32) -> Result<Option<ProcessInfo>, ProcessError> {
    let pid_arg = pid.to_string();
    let result = match ps_command(
        &[
            "-o",
            "pid=,ppid=,pgid=,etime=,time=,stat=,rss=,command=",
            "-p",
            &pid_arg,
        ],
        2.0,
        true,
    ) {
        Ok(result) => result,
        Err(_) => return Ok(None),
    };
    if result.returncode != 0 {
        return Ok(None);
    }
    let Some(lstart) = ps_start_time(pid, true) else {
        return Ok(None);
    };
    #[cfg(target_os = "macos")]
    let epoch = crate::sys::process::darwin_facts(pid).map(|facts| facts.start_epoch);
    #[cfg(not(target_os = "macos"))]
    let epoch = None;
    ps_process_output(pid, &result.output, Some(&lstart), epoch).map_err(Into::into)
}

pub fn start_time(pid: i32) -> Result<Option<String>, ProcessError> {
    Ok(process(pid)?
        .map(|info| info.lstart)
        .or_else(|| ps_start_time(pid, true)))
}

/// Decode observed /proc directory names, preserving iteration errors.
pub fn linux_census_pids(
    entries: impl IntoIterator<Item = std::io::Result<OsString>>,
) -> Result<Vec<i32>, ProcessError> {
    let mut pids = Vec::new();
    for entry in entries {
        let name = entry.map_err(read_error(std::path::Path::new("/proc")))?;
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.is_empty()
            && name
                .chars()
                .all(|ch| crate::pending::decimal_digit(ch).is_some())
        {
            pids.push(pid_field(name, "pid")?);
        }
    }
    Ok(pids)
}

pub fn processes() -> Result<Vec<ProcessInfo>, ProcessError> {
    #[cfg(target_os = "macos")]
    let candidates = crate::sys::process::darwin_pids()
        .ok()
        .map(Ok::<_, ProcessError>);
    #[cfg(target_os = "linux")]
    let candidates = std::fs::read_dir("/proc").ok().map(|entries| {
        linux_census_pids(entries.map(|entry| entry.map(|entry| entry.file_name())))
    });
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let candidates: Option<Result<Vec<i32>, ProcessError>> = None;
    match candidates {
        Some(pids) => {
            let mut rows = Vec::new();
            for pid in pids?.into_iter().filter(|&pid| pid > 0) {
                if let Some(row) = process(pid)? {
                    rows.push(row);
                }
            }
            Ok(rows)
        }
        None => ps_processes(),
    }
}

pub fn ps_processes() -> Result<Vec<ProcessInfo>, ProcessError> {
    let result = ps_command(&["-axo", "pid="], 5.0, false)?;
    if result.returncode != 0 {
        return Err(ProcessError::Census(
            result
                .stderr
                .as_deref()
                .map(provenant_core::py::python_strip)
                .filter(|text| !text.is_empty())
                .unwrap_or("ps census failed")
                .into(),
        ));
    }
    let pids: Vec<i32> = result
        .output
        .split(provenant_core::py::python_whitespace)
        .filter(|value| {
            !value.is_empty()
                && value
                    .chars()
                    .all(|ch| crate::pending::decimal_digit(ch).is_some())
        })
        .map(|value| pid_field(value, "pid"))
        .collect::<Result<_, _>>()?;
    if pids.is_empty() {
        return Err(ProcessError::Census("ps census returned no PIDs".into()));
    }
    let mut rows = Vec::new();
    for pid in pids {
        if let Some(row) = ps_process(pid)? {
            rows.push(row);
        }
    }
    if rows.is_empty() {
        return Err(ProcessError::Census(
            "ps census could not inspect any PID".into(),
        ));
    }
    Ok(rows)
}

#[cfg(test)]
mod process_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn linux_census_preserves_directory_iteration_failure() {
        let result = linux_census_pids([
            Ok("42".into()),
            Err(std::io::Error::from_raw_os_error(crate::sys::errno::EACCES)),
        ]);
        assert!(
            result.is_err(),
            "an incomplete census must not look successful"
        );
    }

    #[test]
    fn linux_fixture_computes_cpu_rss_arguments_and_clock_fields() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("42");
        std::fs::create_dir(&directory).unwrap();
        let mut fields = vec!["0".to_string(); 22];
        for (index, value) in [
            (0, "S"),
            (1, "2"),
            (2, "3"),
            (4, "0"),
            (11, "120"),
            (12, "5"),
            (19, "100"),
            (21, "4"),
        ] {
            fields[index] = value.into();
        }
        std::fs::write(
            directory.join("stat"),
            format!("42 (tool) {}", fields.join(" ")),
        )
        .unwrap();
        std::fs::write(directory.join("cmdline"), b"tool\0two words\0").unwrap();
        let clock = LinuxClock {
            boot_epoch: 1704067200,
            ticks: 100,
            page_size: 4096,
            now_epoch: 1704067262.0,
        };
        let row = linux_process_at(42, root.path(), clock).unwrap().unwrap();
        assert_eq!((row.ppid, row.pgid, row.rss), (2, 3, 16));
        assert_eq!(
            row.start_epoch.map(f64::to_bits),
            Some(1704067201.0_f64.to_bits())
        );
        assert_eq!(row.elapsed, "01:01");
        assert_eq!(row.cpu, "0:01.25");
        assert_eq!(row.command, "tool 'two words'");
        assert_eq!(row.stat, "S");
        std::fs::write(directory.join("cmdline"), b"").unwrap();
        assert_eq!(
            linux_process_at(42, root.path(), clock)
                .unwrap()
                .unwrap()
                .command,
            "tool"
        );
        std::fs::write(directory.join("cmdline"), b"tool\0bad\xff\0").unwrap();
        let row = linux_process_at(42, root.path(), clock).unwrap().unwrap();
        assert_eq!(row.command.as_bytes(), b"tool 'bad\xff'");
        assert!(serde_json::to_value(row).is_err());
    }

    #[test]
    fn lookup_preserves_typed_pid_range_errors_instead_of_omitting_the_process() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("42");
        std::fs::create_dir(&directory).unwrap();
        let clock = LinuxClock {
            boot_epoch: 0,
            ticks: 100,
            page_size: 4096,
            now_epoch: 0.0,
        };
        for (ppid, pgid, field) in [("2147483648", "3", "ppid"), ("2", "2147483648", "pgid")] {
            let stat = format!("42 (tool) S {ppid} {pgid} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1234");
            std::fs::write(directory.join("stat"), stat).unwrap();
            assert!(
                matches!(linux_process_at(42, root.path(), clock), Err(ProcessError::Parse(ProcessParseError::NumberOutOfRange(name))) if name == field)
            );
        }
        std::fs::remove_file(directory.join("stat")).unwrap();
        assert!(linux_process_at(42, root.path(), clock).unwrap().is_none());
    }

    #[test]
    fn native_self_identity_matches_python_at_the_same_pid() {
        let pid = std::process::id() as i32;
        let Some(row) = process_at(pid, 2000000000.0).unwrap() else {
            panic!("native self observation unavailable");
        };
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let script = format!(
            r#"
import json, struct
from skills.orchestrate.scripts import process_info as p
row = p.process({pid})
assert row is not None
print(json.dumps([row.pid, row.ppid, row.pgid, row.lstart, row.command, row.comm, row.tty, row.uid, struct.pack('>d', row.start_epoch).hex() if row.start_epoch is not None else None]))
"#
        );
        let Some(expected) = crate::test_support::python(&root, &script) else {
            return;
        };
        assert_eq!(
            serde_json::json!([
                row.pid,
                row.ppid,
                row.pgid,
                row.lstart,
                row.command.to_str().unwrap(),
                row.comm.to_str().unwrap(),
                row.tty,
                row.uid,
                row.start_epoch
                    .map(|epoch| format!("{:016x}", epoch.to_bits()))
            ]),
            expected
        );
    }

    #[test]
    fn native_census_contains_self_and_missing_pid_is_absent() {
        assert!(process(2_000_000_000).unwrap().is_none());
        let rows = processes().unwrap();
        assert!(rows.iter().any(|row| row.pid == std::process::id() as i32));
    }
}

#[cfg(test)]
mod numeric_tests {
    use super::*;

    #[test]
    fn differential_durations_retain_the_finite_python_float_domain() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::python(
            &root,
            r#"
import json
from skills.orchestrate.scripts import process_info as p
values = [-float('inf'), float('nan'), 1e20, 1e100, float.fromhex('0x1.fffffffffffffp+1023')]
out = []
for platform in ['darwin', 'linux']:
    p.sys.platform = platform
    for elapsed in [False, True]:
        out.extend(p._duration(value, elapsed=elapsed) for value in values)
print(json.dumps(out))
"#,
        ) else {
            return;
        };
        let mut actual = Vec::new();
        for darwin in [true, false] {
            for elapsed in [false, true] {
                for value in [f64::NEG_INFINITY, f64::NAN, 1e20, 1e100, f64::MAX] {
                    actual.push(duration(value, elapsed, darwin).unwrap());
                }
            }
        }
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }
}
