//! Pure argument policy for the sandbox-safe ps subset.
//! Observation, locale rendering and unreadable-row synthesis are adapter work.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Field {
    Pid,
    Ppid,
    Pgid,
    Uid,
    User,
    Lstart,
    Etime,
    Time,
    Command,
    Args,
    Comm,
    Stat,
    Rss,
    Tty,
    Cmd,
}

impl Field {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "pid" => Self::Pid,
            "ppid" => Self::Ppid,
            "pgid" => Self::Pgid,
            "uid" => Self::Uid,
            "user" => Self::User,
            "lstart" => Self::Lstart,
            "etime" => Self::Etime,
            "time" => Self::Time,
            "command" => Self::Command,
            "args" => Self::Args,
            "comm" => Self::Comm,
            "stat" => Self::Stat,
            "rss" => Self::Rss,
            "tty" => Self::Tty,
            "cmd" => Self::Cmd,
            _ => return None,
        })
    }
    fn header(self) -> &'static str {
        match self {
            Self::Pid => "PID",
            Self::Ppid => "PPID",
            Self::Pgid => "PGID",
            Self::Uid => "UID",
            Self::User => "USER",
            Self::Lstart => "STARTED",
            Self::Etime => "ELAPSED",
            Self::Time => "TIME",
            Self::Command | Self::Args => "COMMAND",
            Self::Comm => "COMM",
            Self::Stat => "STAT",
            Self::Rss => "RSS",
            Self::Tty => "TTY",
            Self::Cmd => "CMD",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PsOptions {
    /// Repeats stay in parse output, as in Python; the observation edge deduplicates.
    pub selected: Vec<Pid>,
    pub fields: Vec<(Field, String)>,
    pub all_processes: bool,
    pub include_no_tty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PsParseError {
    #[error("unsupported argument {0}")]
    Argument(String),
    #[error("unsupported flag {0}")]
    Flag(String),
    #[error("-{0} needs a value")]
    MissingValue(String),
    #[error("-p needs numeric PIDs")]
    NumericPids,
    #[error("PID number out of range")]
    NumberOutOfRange,
    #[error("unsupported field {0}")]
    Field(String),
}

fn default_layout() -> Vec<(Field, String)> {
    layout(&[Field::Pid, Field::Tty, Field::Time, Field::Cmd])
}
fn full_layout() -> Vec<(Field, String)> {
    layout(&[
        Field::Uid,
        Field::Pid,
        Field::Ppid,
        Field::Tty,
        Field::Time,
        Field::Cmd,
    ])
}
fn user_layout() -> Vec<(Field, String)> {
    let mut fields = layout(&[
        Field::User,
        Field::Pid,
        Field::Rss,
        Field::Tty,
        Field::Stat,
        Field::Time,
        Field::Command,
    ]);
    fields[3].1 = "TT".into();
    fields
}
fn layout(fields: &[Field]) -> Vec<(Field, String)> {
    fields
        .iter()
        .map(|&field| (field, field.header().into()))
        .collect()
}
fn switches(text: &str) -> bool {
    text.bytes().all(|byte| b"aexAfuw".contains(&byte))
}
fn value_option(ch: char) -> bool {
    matches!(ch, 'o' | 'O' | 'p')
}

pub fn parse(argv: &[String]) -> Result<PsOptions, PsParseError> {
    let mut result = PsOptions {
        selected: Vec::new(),
        fields: Vec::new(),
        all_processes: false,
        include_no_tty: false,
    };
    let mut append_command = false;
    let mut layout = default_layout();
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        let last = arg.chars().next_back();
        let bsd = index == 0
            && !arg.starts_with('-')
            && !arg.is_empty()
            && switches(if last.is_some_and(value_option) {
                &arg[..arg.len() - 1]
            } else {
                arg
            });
        let body = if bsd {
            arg.as_str()
        } else {
            arg.strip_prefix('-')
                .ok_or_else(|| PsParseError::Argument(arg.clone()))?
        };
        let leading = if body.len() > 1 && body.chars().next_back().is_some_and(value_option) {
            body.trim_end_matches(value_option)
        } else {
            body
        };
        if leading != body && !switches(leading) {
            return Err(PsParseError::Flag(arg.clone()));
        }
        let (option, mut value) = if switches(leading) && !leading.is_empty() {
            result.all_processes |= leading.bytes().any(|byte| b"axeA".contains(&byte));
            result.include_no_tty |= leading.bytes().any(|byte| b"xeA".contains(&byte));
            if leading.contains('f') {
                layout = full_layout();
            }
            if leading.contains('u') {
                layout = user_layout();
            }
            if leading == body {
                index += 1;
                continue;
            }
            (&body[leading.len()..], None)
        } else {
            let end = body.chars().next().map_or(0, char::len_utf8);
            (
                &body[..end],
                (!body[end..].is_empty()).then_some(&body[end..]),
            )
        };
        if !matches!(option, "o" | "O" | "p") {
            return Err(PsParseError::Flag(arg.clone()));
        }
        if value.is_none() {
            index += 1;
            value = Some(
                argv.get(index)
                    .ok_or_else(|| PsParseError::MissingValue(option.into()))?,
            );
        }
        // Established above without an unwrap in library code.
        let value = value.ok_or_else(|| PsParseError::MissingValue(option.into()))?;
        if option == "p" {
            for item in value.split(',') {
                let value =
                    provenant_core::py::Integer::parse(item).ok_or(PsParseError::NumericPids)?;
                if value.as_i64().is_err() && value.as_u64().is_err() {
                    return Err(PsParseError::NumberOutOfRange);
                }
                result.selected.push(Pid(value));
            }
        } else {
            if option == "O" && result.fields.is_empty() {
                result.fields.extend_from_slice(&layout[..layout.len() - 1]);
                append_command = true;
            }
            for field in value.split(',') {
                let (name, header) = field
                    .split_once('=')
                    .map_or((field, None), |(name, header)| (name, Some(header)));
                let field = Field::parse(name).ok_or_else(|| PsParseError::Field(name.into()))?;
                result
                    .fields
                    .push((field, header.unwrap_or(field.header()).into()));
            }
        }
        index += 1;
    }
    if append_command {
        result.fields.push(layout[layout.len() - 1].clone());
    }
    if result.fields.is_empty() {
        result.fields = layout;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn familiar_forms_choose_the_source_layout_and_tty_policy() {
        let aux = parse(&args(&["aux"])).unwrap();
        assert!(aux.all_processes && aux.include_no_tty);
        assert_eq!(
            aux.fields
                .iter()
                .map(|(_, header)| header.as_str())
                .collect::<Vec<_>>(),
            ["USER", "PID", "RSS", "TT", "STAT", "TIME", "COMMAND"]
        );
        let full = parse(&args(&["-ef"])).unwrap();
        assert_eq!(full.fields, full_layout());
        let a = parse(&args(&["-a"])).unwrap();
        assert!(a.all_processes && !a.include_no_tty);
    }

    #[test]
    fn o_inserts_columns_and_headers_can_be_empty() {
        let options = parse(&args(&["-O", "ppid", "-p", "42,42,7"])).unwrap();
        assert_eq!(options.selected, [42, 42, 7]);
        assert_eq!(
            options
                .fields
                .iter()
                .map(|(field, _)| *field)
                .collect::<Vec<_>>(),
            [Field::Pid, Field::Tty, Field::Time, Field::Ppid, Field::Cmd]
        );
        assert_eq!(
            parse(&args(&["axo", "pid=,comm="])).unwrap().fields,
            [(Field::Pid, "".into()), (Field::Comm, "".into())]
        );
    }

    #[test]
    fn unsupported_inputs_have_source_diagnostics() {
        for (input, message) in [
            (vec!["-Z"], "unsupported flag -Z"),
            (vec!["-o"], "-o needs a value"),
            (vec!["-p", "x"], "-p needs numeric PIDs"),
            (vec!["-o", "bad"], "unsupported field bad"),
            (vec!["other"], "unsupported argument other"),
        ] {
            assert_eq!(parse(&args(&input)).unwrap_err().to_string(), message);
        }
    }

    #[test]
    fn differential_parser_matches_python_policy() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::python(
            &root,
            r#"
import json, runpy
p = runpy.run_path('skills/orchestrate/scripts/bin/ps')['parse']
cases = [[], ['aux'], ['-ef'], ['ax'], ['axo','pid,comm'], ['-axo','pid=,ppid='], ['-O','ppid','-p','42,42,7'], ['-u','-O','pid=','-f'], ['-o','pid=X,comm='], ['-p+1_000,-2'], ['-Z'], ['-o'], ['-p','x'], ['-o','bad'], ['other'], ['-'], ['--'], ['-axoo','pid']]
out=[]
for args in cases:
    try: out.append(p(args))
    except ValueError as e: out.append(str(e))
print(json.dumps(out))
"#,
        ) else {
            return;
        };
        let cases: &[&[&str]] = &[
            &[],
            &["aux"],
            &["-ef"],
            &["ax"],
            &["axo", "pid,comm"],
            &["-axo", "pid=,ppid="],
            &["-O", "ppid", "-p", "42,42,7"],
            &["-u", "-O", "pid=", "-f"],
            &["-o", "pid=X,comm="],
            &["-p+1_000,-2"],
            &["-Z"],
            &["-o"],
            &["-p", "x"],
            &["-o", "bad"],
            &["other"],
            &["-"],
            &["--"],
            &["-axoo", "pid"],
        ];
        let actual = cases
            .iter()
            .map(|case| match parse(&args(case)) {
                Ok(value) => serde_json::json!([
                    value
                        .selected
                        .iter()
                        .map(|pid| pid.0.as_i64().ok())
                        .collect::<Vec<_>>(),
                    value.fields,
                    value.all_processes,
                    value.include_no_tty
                ]),
                Err(error) => serde_json::json!(error.to_string()),
            })
            .collect::<Vec<_>>();
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }
}

/// Parsed PID tokens retain Python's integer domain until the OS boundary.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(provenant_core::py::Integer);
impl Pid {
    pub fn as_os_pid(&self) -> Option<i32> {
        self.0.as_i32().ok()
    }
}
impl std::fmt::Display for Pid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl PartialEq<i64> for Pid {
    fn eq(&self, other: &i64) -> bool {
        self.0.as_i64() == Ok(*other)
    }
}

use crate::process_info::ProcessInfo;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

#[derive(Clone, Debug, PartialEq)]
pub struct PublicFacts {
    pub ppid: i32,
    pub pgid: i32,
    pub uid: i64,
    pub start_epoch: f64,
    pub elapsed: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Readable(ProcessInfo),
    Unreadable {
        pid: i32,
        facts: Option<PublicFacts>,
    },
}

impl Row {
    pub fn pid(&self) -> i32 {
        match self {
            Self::Readable(info) => info.pid,
            Self::Unreadable { pid, .. } => *pid,
        }
    }
    fn tty(&self) -> &str {
        match self {
            Self::Readable(info) => &info.tty,
            Self::Unreadable { .. } => "?",
        }
    }
}

#[derive(Debug, Error)]
pub enum PsError {
    #[error(transparent)]
    Parse(#[from] PsParseError),
    #[error("Python int too large to convert to C long")]
    NumberOutOfRange,
    #[error(transparent)]
    Process(#[from] crate::process_info::ProcessError),
    #[error("could not format process start time")]
    Time(#[source] std::io::Error),
    #[error(transparent)]
    Account(#[from] crate::sys::SysError),
    #[error("could not write ps output")]
    Write(#[source] std::io::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: u8,
}

pub fn error_output(message: &str) -> Output {
    Output { stdout: String::new(), exit_code: 2,
        stderr: format!("ps: {message}; supported subset: -p PID[,PID], -a/-x/-e/-A, -f, BSD aux, -o/-O pid,ppid,pgid,uid,user,lstart,etime,time,command,args,comm,stat,rss,tty\n") }
}

/// Python's fsdecode/surrogateescape then unicode_escape for nonprintables.
/// Invalid OS bytes become \udcXX, rather than U+FFFD or lossy UTF-8 output.
pub fn visible(text: &OsStr) -> String {
    fn push_char(out: &mut String, ch: char) {
        if provenant_core::py::printable(ch) {
            out.push(ch);
            return;
        }
        match ch {
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            ch if (ch as u32) <= 0xff => out.push_str(&format!("\\x{:02x}", ch as u32)),
            ch if (ch as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push_str(&format!("\\U{:08x}", ch as u32)),
        }
    }
    let mut bytes = text.as_bytes();
    let mut output = String::new();
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                for ch in text.chars() {
                    push_char(&mut output, ch);
                }
                break;
            }
            Err(error) => {
                if let Ok(text) = std::str::from_utf8(&bytes[..error.valid_up_to()]) {
                    for ch in text.chars() {
                        push_char(&mut output, ch);
                    }
                }
                let length = error
                    .error_len()
                    .unwrap_or(bytes.len() - error.valid_up_to());
                for byte in &bytes[error.valid_up_to()..error.valid_up_to() + length] {
                    output.push_str(&format!("\\udc{byte:02x}"));
                }
                bytes = &bytes[error.valid_up_to() + length..];
            }
        }
    }
    output
}

fn user(uid: i64) -> Result<String, PsError> {
    let Ok(uid32) = u32::try_from(uid) else {
        return Ok(if uid < 0 { "?".into() } else { uid.to_string() });
    };
    Ok(crate::sys::account(crate::sys::Account::Uid(uid32))?
        .map_or_else(|| uid.to_string(), |account| visible(&account.name)))
}

fn field(row: &Row, field: Field, locale: &str, darwin: bool) -> Result<String, PsError> {
    let (uid, epoch, fallback_start) = match row {
        Row::Readable(info) => (info.uid, info.start_epoch, info.lstart.as_str()),
        Row::Unreadable { facts, .. } => (
            facts.as_ref().map_or(-1, |f| f.uid),
            facts.as_ref().map(|f| f.start_epoch),
            "?",
        ),
    };
    match field {
        Field::Pid => return Ok(row.pid().to_string()),
        Field::Uid => return Ok(uid.to_string()),
        Field::User => return user(uid),
        Field::Lstart => {
            return epoch.map_or_else(
                || Ok(fallback_start.into()),
                |epoch| {
                    crate::sys::process::locale_start(epoch, locale, darwin).map_err(PsError::Time)
                },
            )
        }
        _ => {}
    }
    Ok(match row {
        Row::Readable(info) => match field {
            Field::Ppid => info.ppid.to_string(),
            Field::Pgid => info.pgid.to_string(),
            Field::Etime => info.elapsed.clone(),
            Field::Time => info.cpu.clone(),
            Field::Command | Field::Cmd | Field::Args => visible(&info.command),
            Field::Comm => visible(&info.comm),
            Field::Stat => info.stat.clone(),
            Field::Rss => info.rss.to_string(),
            Field::Tty => info.tty.clone(),
            _ => String::new(),
        },
        Row::Unreadable { facts, .. } => match (field, facts) {
            (Field::Ppid, Some(facts)) => facts.ppid.to_string(),
            (Field::Pgid, Some(facts)) => facts.pgid.to_string(),
            (Field::Etime, Some(facts)) => facts.elapsed.clone(),
            _ => "?".into(),
        },
    })
}

/// Preserve per-field headers, row sort and the source's limited -a filtering.
/// Callers pass census rows or selected rows in request order; missing rows
/// should already have gone through the unreadable policy.
pub fn render(
    options: &PsOptions,
    mut rows: Vec<Row>,
    locale: &str,
    darwin: bool,
) -> Result<Output, PsError> {
    rows.sort_by_key(Row::pid);
    if options.all_processes && !options.include_no_tty && options.selected.is_empty() {
        rows.retain(|row| !matches!(row.tty(), "?" | "??"));
    }
    let mut stdout = String::new();
    let headers: Vec<_> = options
        .fields
        .iter()
        .filter_map(|(_, header)| (!header.is_empty()).then_some(header.as_str()))
        .collect();
    if !headers.is_empty() {
        stdout.push_str(&headers.join(" "));
        stdout.push('\n');
    }
    for row in &rows {
        let cells = options
            .fields
            .iter()
            .map(|(name, _)| field(row, *name, locale, darwin))
            .collect::<Result<Vec<_>, _>>()?;
        stdout.push_str(&cells.join(" "));
        stdout.push('\n');
    }
    Ok(Output {
        stdout,
        stderr: String::new(),
        exit_code: u8::from(rows.is_empty()),
    })
}

/// Unknown liveness is exclusion evidence. A second Darwin probe follows an
/// unavailable sysctl read so a vanished process is not shown as unreadable.
pub fn unreadable_row(
    pid: i32,
    first: crate::identity::Presence,
    facts: Option<PublicFacts>,
    second: crate::identity::Presence,
) -> (Option<Row>, Option<String>) {
    use crate::identity::Presence;
    if first == Presence::Gone || (facts.is_none() && second == Presence::Gone) {
        return (None, None);
    }
    let warning = facts
        .is_none()
        .then(|| format!("ps: {pid}: exists but cannot be read here; fields show ?\n"));
    (Some(Row::Unreadable { pid, facts }), warning)
}

pub fn run(argv: &[String], locale: &str) -> Result<Output, PsError> {
    let options = match parse(argv) {
        Ok(options) => options,
        Err(error) => return Ok(error_output(&error.to_string())),
    };
    let mut stderr = String::new();
    let rows = if options.selected.is_empty() {
        match crate::process_info::processes() {
            Ok(rows) => rows.into_iter().map(Row::Readable).collect(),
            Err(error) => {
                return Ok(Output {
                    stdout: String::new(),
                    stderr: format!("ps: cannot inspect processes: {error}\n"),
                    exit_code: 1,
                })
            }
        }
    } else {
        let mut seen = std::collections::BTreeSet::new();
        let mut rows = Vec::new();
        for token in &options.selected {
            if !seen.insert(token) {
                continue;
            }
            let pid = token.as_os_pid().ok_or(PsError::NumberOutOfRange)?;
            if let Some(info) = crate::process_info::process(pid)? {
                rows.push(Row::Readable(info));
                continue;
            }
            let first = crate::sys::process::presence(pid);
            #[cfg(target_os = "macos")]
            let facts = crate::sys::process::darwin_facts(pid).and_then(|facts| {
                Some(PublicFacts {
                    ppid: facts.ppid,
                    pgid: facts.pgid,
                    uid: facts.uid.into(),
                    start_epoch: facts.start_epoch,
                    elapsed: crate::process_info::duration(
                        crate::process_info::now_epoch() - facts.start_epoch,
                        true,
                        true,
                    )
                    .ok()?,
                })
            });
            #[cfg(not(target_os = "macos"))]
            let facts = None;
            let second = if cfg!(target_os = "macos") && facts.is_none() {
                crate::sys::process::presence(pid)
            } else {
                first
            };
            let (row, warning) = unreadable_row(pid, first, facts, second);
            rows.extend(row);
            if let Some(warning) = warning {
                stderr.push_str(&warning);
            }
        }
        rows
    };
    let mut output = render(&options, rows, locale, cfg!(target_os = "macos"))?;
    output.stderr = stderr;
    Ok(output)
}

/// Broken pipes are quiet at the future CLI edge; signal disposition remains
/// the binary's responsibility, since a library must not mutate global SIGPIPE.
pub fn write_output(
    output: &Output,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
) -> Result<u8, PsError> {
    stderr
        .write_all(output.stderr.as_bytes())
        .map_err(PsError::Write)?;
    match stdout.write_all(output.stdout.as_bytes()) {
        Ok(()) => Ok(output.exit_code),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(0),
        Err(error) => Err(PsError::Write(error)),
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::identity::Presence;
    use std::os::unix::ffi::OsStringExt;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }
    fn row(pid: i32, tty: &str) -> Row {
        Row::Readable(ProcessInfo {
            pid,
            ppid: 2,
            pgid: 3,
            uid: -1,
            lstart: "start".into(),
            elapsed: "01:02".into(),
            cpu: "0:01.25".into(),
            command: "tool two\nlines".into(),
            comm: "tool".into(),
            stat: "S".into(),
            rss: 123,
            tty: tty.into(),
            start_epoch: None,
        })
    }

    #[test]
    fn rendering_sorts_filters_and_suppresses_individual_headers() {
        let options = parse(&args(&["-a", "-o", "pid=,comm=NAME"])).unwrap();
        let output = render(
            &options,
            vec![row(42, "??"), row(43, "ttys001"), row(1, "ttys000")],
            "C",
            false,
        )
        .unwrap();
        assert_eq!(output.stdout, "NAME\n1 tool\n43 tool\n");
        assert_eq!(output.exit_code, 0);
        let options = parse(&args(&["-p", "42", "-o", "command="])).unwrap();
        assert_eq!(
            render(&options, vec![row(42, "??")], "C", false)
                .unwrap()
                .stdout,
            "tool two\\nlines\n"
        );
        let empty = render(&options, vec![], "C", false).unwrap();
        assert_eq!(empty.stdout, "");
        assert_eq!(empty.exit_code, 1);
    }

    #[test]
    fn unreadable_rows_keep_live_and_unknown_processes_but_omit_gone() {
        for presence in [Presence::Alive, Presence::Unknown] {
            let (row, warning) = unreadable_row(42, presence, None, presence);
            let options = parse(&args(&["-o", "pid=,ppid=,uid=,user=,stat=,rss=,tty="])).unwrap();
            assert_eq!(
                render(&options, vec![row.unwrap()], "C", false)
                    .unwrap()
                    .stdout,
                "42 ? -1 ? ? ? ?\n"
            );
            assert_eq!(
                warning.unwrap(),
                "ps: 42: exists but cannot be read here; fields show ?\n"
            );
        }
        assert!(unreadable_row(42, Presence::Gone, None, Presence::Alive)
            .0
            .is_none());
        assert!(unreadable_row(42, Presence::Alive, None, Presence::Gone)
            .0
            .is_none());
        let facts = PublicFacts {
            ppid: 2,
            pgid: 3,
            uid: 501,
            start_epoch: 1704067200.0,
            elapsed: "01:02".into(),
        };
        let (row, warning) = unreadable_row(42, Presence::Unknown, Some(facts), Presence::Unknown);
        assert!(warning.is_none());
        let options = parse(&args(&["-o", "pid=,ppid=,pgid=,uid=,etime=,stat="])).unwrap();
        assert_eq!(
            render(&options, vec![row.unwrap()], "C", true)
                .unwrap()
                .stdout,
            "42 2 3 501 01:02 ?\n"
        );
    }

    #[test]
    fn visible_text_preserves_unicode_and_escapes_controls_and_os_bytes() {
        let text = std::ffi::OsString::from_vec(b"two\nlines\t\r\x1b\xff".to_vec());
        assert_eq!(visible(&text), "two\\nlines\\t\\r\\x1b\\udcff");
        assert_eq!(
            visible(OsStr::new("λ\u{301}\u{200b}\u{00a0}")),
            "λ\u{301}\\u200b\\xa0"
        );
    }

    #[test]
    fn pid_integer_domain_matches_python_big_and_unicode_tokens() {
        let options = parse(&args(&["-p", "٩٢٢٣٣٧٢٠٣٦٨٥٤٧٧٥٨٠٨,+١_٢,-000,0004"])).unwrap();
        assert_eq!(
            options
                .selected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["9223372036854775808", "12", "0", "4"]
        );
        for value in ["18446744073709551616", "-9223372036854775809"] {
            assert_eq!(
                parse(&args(&["-p", value])).unwrap_err(),
                PsParseError::NumberOutOfRange
            );
        }
        for value in ["1__2", "1_", "_1", "²", "\u{1c}2", "1 2"] {
            assert_eq!(
                parse(&args(&["-p", value])).unwrap_err(),
                PsParseError::NumericPids
            );
        }
    }

    #[test]
    fn selected_processes_are_deduplicated_and_default_header_is_preserved() {
        let pid = std::process::id().to_string();
        let output = run(
            &args(&["-p", &format!("{pid},{pid}"), "-o", "pid=,stat="]),
            "C",
        )
        .unwrap();
        assert_eq!(output.stdout.lines().count(), 1);
        assert!(output.stdout.starts_with(&pid));
        let output = run(&args(&["-p", &pid]), "C").unwrap();
        assert_eq!(output.stdout.lines().next(), Some("PID TTY TIME CMD"));
        assert_eq!(output.exit_code, 0);
    }

    #[test]
    fn quiet_broken_pipe_is_an_edge_policy_and_other_writes_remain_errors() {
        struct Broken(std::io::ErrorKind);
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(self.0.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Output {
            stdout: "PID\n42\n".into(),
            stderr: String::new(),
            exit_code: 0,
        };
        assert_eq!(
            write_output(
                &output,
                &mut Broken(std::io::ErrorKind::BrokenPipe),
                &mut Vec::new()
            )
            .unwrap(),
            0
        );
        assert!(write_output(
            &output,
            &mut Broken(std::io::ErrorKind::StorageFull),
            &mut Vec::new()
        )
        .is_err());
    }

    #[test]
    fn locale_start_matches_python_with_padding_including_last_column() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::python(
            &root,
            r#"
import json, locale, runpy, sys
from types import SimpleNamespace
p = runpy.run_path('skills/orchestrate/scripts/bin/ps')
values = []
for name in ['C', 'en_AU.UTF-8', 'de_DE.UTF-8', 'ja_JP.UTF-8']:
    try: locale.setlocale(locale.LC_ALL, name)
    except locale.Error: values.append(None); continue
    values.append(p['_lstart'](SimpleNamespace(start_epoch=1704067200, lstart='unused')))
print(json.dumps(values))
"#,
        ) else {
            return;
        };
        for (index, locale) in ["C", "en_AU.UTF-8", "de_DE.UTF-8", "ja_JP.UTF-8"]
            .iter()
            .enumerate()
        {
            if expected[index].is_null() {
                continue;
            }
            assert_eq!(
                crate::sys::process::locale_start(1704067200.0, locale, cfg!(target_os = "macos"))
                    .unwrap(),
                expected[index]
            );
        }
    }

    #[test]
    fn differential_visible_text_and_parser_errors_match_python() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let Some(expected) = crate::test_support::python(
            &root,
            r#"
import json, os, runpy
p = runpy.run_path('skills/orchestrate/scripts/bin/ps')
cases = [''.join(chr(i) for i in range(256)), 'λ\u0301\u200b\u00a0\U0001f4a9', os.fsdecode(b'bad\xff\xf0\x80')]
values = [p['_visible'](text) for text in cases]
values.append([str(pid) for pid in p['parse'](['-p', '٩٢٢٣٣٧٢٠٣٦٨٥٤٧٧٥٨٠٨,+١_٢,-000,0004'])[0]])
print(json.dumps(values))
"#,
        ) else {
            return;
        };
        let text: String = (0..256).filter_map(char::from_u32).collect();
        let values = vec![
            serde_json::json!(visible(OsStr::new(&text))),
            serde_json::json!(visible(OsStr::new("λ\u{301}\u{200b}\u{00a0}💩"))),
            serde_json::json!(visible(&std::ffi::OsString::from_vec(
                b"bad\xff\xf0\x80".to_vec()
            ))),
            serde_json::json!(parse(&args(&["-p", "٩٢٢٣٣٧٢٠٣٦٨٥٤٧٧٥٨٠٨,+١_٢,-000,0004"]))
                .unwrap()
                .selected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()),
        ];
        assert_eq!(serde_json::to_value(values).unwrap(), expected);
    }
}
