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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsOptions {
    /// Repeats stay in parse output, as in Python; the observation edge deduplicates.
    pub selected: Vec<i64>,
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
    #[error("PID exceeds the current decoder's integer range")]
    PidRange,
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
                let item = item.trim();
                // int() permits underscores only between digits.
                let digits = item.strip_prefix(['+', '-']).unwrap_or(item);
                if digits.is_empty()
                    || !digits.as_bytes()[0].is_ascii_digit()
                    || !digits.as_bytes()[digits.len() - 1].is_ascii_digit()
                    || digits.contains("__")
                    || !digits
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'_')
                {
                    return Err(PsParseError::NumericPids);
                }
                let pid = item
                    .replace('_', "")
                    .parse()
                    .map_err(|_| PsParseError::PidRange)?;
                result.selected.push(pid);
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
                    value.selected,
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
