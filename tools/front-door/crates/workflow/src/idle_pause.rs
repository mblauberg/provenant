//! Idle-frontier pause validation. Markdown policy is independent of file access.
use provenant_core::py::python_strip as strip;
use regex::Regex;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

fn re(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(
        &pattern
            .replace(r"\s", r"[\s\x1c-\x1f]")
            .replace(r"\S", r"[^\s\x1c-\x1f]"),
    )
}
fn field(text: &str, name: &str, prefix: &str) -> Result<String, regex::Error> {
    let pattern = re(&format!(
        r"(?m)^- \*\*{}:\*\*\s*(.+?)\s*$",
        regex::escape(name)
    ))?;
    if let Some(c) = pattern.captures(text) {
        return Ok(strip(&c[1]).into());
    }
    let heading = re(r"(?m)^## Heartbeat\s*$\n")?;
    let Some(start) = heading.find(text) else {
        return Ok(String::new());
    };
    let tail = &text[start.end()..];
    let end = re(r"(?m)^## ")?
        .find(tail)
        .map_or(tail.len(), |m| m.start());
    let section = strip(&tail[..end]);
    for line in crate::pending::splitlines(section) {
        let value = strip(line);
        if value.is_empty() || value.starts_with("<!--") || value.starts_with("-->") {
            continue;
        }
        if prefix.is_empty() {
            return Ok(value.into());
        }
        if let Some(c) = re(&format!(r"(?i)\A{}:\s*(.+)\z", regex::escape(prefix)))?.captures(value)
        {
            return Ok(strip(&c[1]).into());
        }
    }
    Ok(String::new())
}

pub fn queue_has_active_rows(text: &str) -> Result<(bool, Option<String>), regex::Error> {
    if !re(r"(?im)^##\s+Tier\s+\S")?.is_match(text) {
        return Ok((
            false,
            Some("canonical queue lacks a '## Tier ...' section".into()),
        ));
    }
    let separator = re(r"\A:?-{3,}:?\z")?;
    for line in crate::pending::splitlines(text) {
        let value = strip(line);
        if !value.starts_with('|') || !value.ends_with('|') {
            continue;
        }
        // Python splits only pipes whose immediately preceding character is not '\'.
        let inner = value
            .strip_prefix('|')
            .and_then(|inner| inner.strip_suffix('|'))
            .unwrap_or("");
        let mut cells = Vec::new();
        let mut start = 0;
        for (i, c) in inner.char_indices() {
            if c == '|' && (i == 0 || inner.as_bytes().get(i - 1) != Some(&b'\\')) {
                cells.push(strip(&inner[start..i]));
                start = i + 1;
            }
        }
        cells.push(strip(&inner[start..]));
        let normalized: Vec<_> = cells.iter().map(|c| c.to_lowercase()).collect();
        if normalized
            == [
                "id",
                "status",
                "depends-on",
                "lease-owner",
                "lease-expiry",
                "notes",
            ]
            || cells.iter().all(|c| separator.is_match(c))
        {
            continue;
        }
        if cells.len() != 6 {
            return Ok((
                true,
                Some(format!("malformed queue row (expected 6 cells): {value}")),
            ));
        }
        let status = cells[1].trim_matches('`').to_uppercase();
        if ["PENDING", "LEASED"].contains(&status.as_str()) {
            return Ok((true, None));
        }
        if !["DONE", "BLOCKED", "DEFERRED"].contains(&status.as_str()) {
            return Ok((
                true,
                Some(format!(
                    "unrecognized queue status '{status}' — controlled vocabulary is PENDING, LEASED, DONE, BLOCKED, DEFERRED"
                )),
            ));
        }
    }
    Ok((false, None))
}

pub fn validate_text(text: &str, queue: &str) -> Result<Vec<String>, regex::Error> {
    let mut errors = state_errors(text)?;
    let (active, error) = queue_has_active_rows(queue)?;
    if let Some(error) = error {
        errors.push(error);
    } else if active {
        errors.push("canonical queue still has PENDING or LEASED rows".into());
    }
    Ok(errors)
}

fn state_errors(text: &str) -> Result<Vec<String>, regex::Error> {
    let status = field(text, "Run status", "")?;
    let lease = field(text, "Conductor lease", "CONDUCTOR LEASE")?;
    let in_flight = field(text, "In flight", "")?;
    let next_up = field(text, "Next up", "")?;
    let resume = field(text, "Resume protocol", "RESUME PROTOCOL")?;
    let mut errors = vec![];
    if !re(r"(?i)\APAUSED\s+(?:—|-)\s+reason:\s*idle-frontier\z")?.is_match(&status) {
        errors.push("run status must be exactly PAUSED — reason: idle-frontier".into());
    }
    if !["released", "release-on-driver-exit"].contains(&lease.to_lowercase().as_str()) {
        errors.push("idle pause must record lease release-on-driver-exit or released".into());
    }
    if !["none", "(none)"].contains(&in_flight.to_lowercase().as_str()) {
        errors.push("idle pause requires an empty in-flight ledger".into());
    }
    if !re(r"(?i)\A\(?none\s+(?:—|-)\s+dry(?:\s+after\s+bounded\s+re-enumeration)?\)?\z")?
        .is_match(&next_up)
    {
        errors.push("idle pause requires an empty dry next-up frontier sentinel".into());
    }
    let resume = resume
        .split(provenant_core::py::python_whitespace)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let resume_re = re(r"\Arestart-on:\s*([a-z-]+(?:\s*,\s*[a-z-]+)*)\z")?;
    let triggers = resume_re
        .captures(&resume)
        .map(|c| {
            c[1].split(',')
                .map(|s| strip(s).to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if triggers.is_empty()
        || triggers.iter().collect::<BTreeSet<_>>().len() != triggers.len()
        || triggers.iter().any(|s| {
            ![
                "human-directive",
                "gate-answer",
                "external-completion",
                "material-change",
                "explicit-restart",
            ]
            .contains(&s.as_str())
        })
    {
        errors.push("idle pause requires a structured external resume trigger".into());
    }
    Ok(errors)
}

#[derive(Debug, thiserror::Error)]
pub enum IdlePauseError {
    #[error(transparent)]
    Pattern(#[from] regex::Error),
    /// Python raises UnicodeDecodeError outside its OSError diagnostic path.
    #[error("invalid UTF-8 in {path}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
}

fn python_os_error(path: &Path, error: &std::io::Error) -> String {
    let description = error.to_string();
    if let Some(errno) = error.raw_os_error() {
        let suffix = format!(" (os error {errno})");
        let message = description.strip_suffix(&suffix).unwrap_or(&description);
        format!(
            "[Errno {errno}] {message}: {}",
            provenant_core::py::python_repr(&path.to_string_lossy())
        )
    } else {
        format!(
            "[Errno None] {description}: {}",
            provenant_core::py::python_repr(&path.to_string_lossy())
        )
    }
}

/// Match `validate`: read errors are returned as policy diagnostics, in order.
pub fn validate(path: &Path, queue: Option<&Path>) -> Result<Vec<String>, IdlePauseError> {
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|source| IdlePauseError::Decode {
            path: path.to_path_buf(),
            source,
        })?,
        Err(source) => {
            return Ok(vec![format!(
                "cannot read state: {}",
                python_os_error(path, &source)
            )]);
        }
    };
    let mut errors = state_errors(&text.replace("\r\n", "\n").replace('\r', "\n"))?;
    let default = path.parent().unwrap_or(Path::new(".")).join("QUEUE.md");
    let queue_path = queue.unwrap_or(&default);
    let queue = match std::fs::read(queue_path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|source| IdlePauseError::Decode {
            path: queue_path.to_path_buf(),
            source,
        })?,
        Err(source) => {
            errors.push(format!(
                "cannot read canonical queue: {}",
                python_os_error(queue_path, &source)
            ));
            return Ok(errors);
        }
    };
    let (active, error) = queue_has_active_rows(&queue.replace("\r\n", "\n").replace('\r', "\n"))?;
    if let Some(error) = error {
        errors.push(error);
    } else if active {
        errors.push("canonical queue still has PENDING or LEASED rows".into());
    }
    Ok(errors)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdlePauseCliResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

const CLI_USAGE: &str = "usage: validate_idle_pause.py [-h] [--queue QUEUE] [--quiet] state\n";
const CLI_HELP: &str = "usage: validate_idle_pause.py [-h] [--queue QUEUE] [--quiet] state\n\nValidate that a PAUSED mission STATE is a real, resumable idle-frontier pause.\n\npositional arguments:\n  state\n\noptions:\n  -h, --help     show this help message and exit\n  --queue QUEUE\n  --quiet\n";

fn argument_path(value: &str) -> PathBuf {
    if value.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(value)
    }
}

/// Parse the source entry point's small argparse surface and render its output.
pub fn run_cli_args(argv: &[String]) -> Result<IdlePauseCliResult, IdlePauseError> {
    let mut queue = None;
    let mut quiet = false;
    let mut positionals = Vec::new();
    let mut unknown = Vec::new();
    let mut after_separator = false;
    let mut index = 0;
    while let Some(argument) = argv.get(index) {
        if !after_separator && (argument == "-h" || argument == "--help") {
            return Ok(IdlePauseCliResult {
                exit_code: 0,
                stdout: CLI_HELP.into(),
                stderr: String::new(),
            });
        }
        if !after_separator && argument == "--" {
            after_separator = true;
        } else if !after_separator && argument.starts_with("--") {
            let (option, attached_value) = argument
                .split_once('=')
                .map_or((argument.as_str(), None), |(option, value)| {
                    (option, Some(value))
                });
            let queue_match = "--queue".starts_with(option) && option.len() > 2;
            let quiet_match = "--quiet".starts_with(option) && option.len() > 2;
            match (queue_match, quiet_match) {
                (true, true) => {
                    return Ok(cli_parse_error(&format!(
                        "ambiguous option: {option} could match --queue, --quiet"
                    )));
                }
                (true, false) => {
                    let value = if let Some(value) = attached_value {
                        value
                    } else {
                        index += 1;
                        let Some(value) = argv.get(index) else {
                            return Ok(cli_parse_error("argument --queue: expected one argument"));
                        };
                        if value.starts_with('-') && value != "-" {
                            return Ok(cli_parse_error("argument --queue: expected one argument"));
                        }
                        value
                    };
                    queue = Some(argument_path(value));
                }
                (false, true) => {
                    if let Some(value) = attached_value {
                        return Ok(cli_parse_error(&format!(
                            "argument --quiet: ignored explicit argument '{value}'"
                        )));
                    }
                    quiet = true;
                }
                (false, false) => unknown.push(argument.clone()),
            }
        } else if !after_separator && argument.starts_with('-') && argument != "-" {
            unknown.push(argument.clone());
        } else {
            positionals.push(argument.clone());
        }
        index += 1;
    }
    if positionals.is_empty() {
        return Ok(cli_parse_error(
            "the following arguments are required: state",
        ));
    }
    if !unknown.is_empty() {
        return Ok(cli_parse_error(&format!(
            "unrecognized arguments: {}",
            unknown.join(" ")
        )));
    }
    if positionals.len() > 1 {
        return Ok(cli_parse_error(&format!(
            "unrecognized arguments: {}",
            positionals[1..].join(" ")
        )));
    }
    let Some(state_value) = positionals.first() else {
        return Ok(cli_parse_error(
            "the following arguments are required: state",
        ));
    };
    let state = argument_path(state_value);
    run_cli(&state, queue.as_deref(), quiet)
}

fn cli_parse_error(message: &str) -> IdlePauseCliResult {
    IdlePauseCliResult {
        exit_code: 2,
        stdout: String::new(),
        stderr: format!("{CLI_USAGE}validate_idle_pause.py: error: {message}\n"),
    }
}

/// Render the Python entry point's validation output and status.
pub fn run_cli(
    path: &Path,
    queue: Option<&Path>,
    quiet: bool,
) -> Result<IdlePauseCliResult, IdlePauseError> {
    match validate(path, queue) {
        Ok(errors) if errors.is_empty() => Ok(IdlePauseCliResult {
            exit_code: 0,
            stdout: if quiet {
                String::new()
            } else {
                format!("PASS: valid idle pause {}\n", path.display())
            },
            stderr: String::new(),
        }),
        Ok(errors) => Ok(IdlePauseCliResult {
            exit_code: 1,
            stdout: if quiet {
                String::new()
            } else {
                errors
                    .iter()
                    .map(|error| format!("ERROR: {error}\n"))
                    .collect()
            },
            stderr: String::new(),
        }),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn state() -> String {
        "# STATE\n- **Run status:** PAUSED — reason: idle-frontier\n- **Conductor lease:** released\n- **In flight:** (none)\n- **Next up:** (none — dry after bounded re-enumeration)\n- **Resume protocol:** restart-on: material-change, explicit-restart\n".into()
    }
    #[test]
    fn idle_pause_rejects_active_unknown_and_malformed_queue_rows() {
        assert!(validate_text(&state(), "## Tier 0\n").unwrap().is_empty());
        for status in ["PENDING", "LEASED", "ACTIVE"] {
            assert!(!validate_text(
                &state(),
                &format!("## Tier 0\n| A | {status} | - | - | - | note |\n")
            )
            .unwrap()
            .is_empty());
        }
        assert!(validate_text(
            &state(),
            "## Tier 0\n| A | DEFERRED | - | - | - | A \\| B |\n"
        )
        .unwrap()
        .is_empty());
        assert!(!validate_text(&state(), "## Tier 0\n| A | DONE |\n")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn validation_cli_preserves_stdout_status_and_quiet_behavior() {
        let base = state();
        let cases = vec![
            json!({"op":"cli","text":base,"queue":"## Tier 0\n","quiet":true}),
            json!({"op":"cli","text":state().replace("PAUSED", "NOT PAUSED"),"queue":"## Tier 0\n","quiet":false}),
            json!({"op":"cli","text":state(),"queue":"## Tier 0\n| A | PENDING | - | - | - | note |\n","quiet":true}),
        ];
        let temp = tempfile::tempdir().unwrap();
        let mut actual = Vec::new();
        for (index, case) in cases.iter().enumerate() {
            let path = temp.path().join(format!("STATE-{index}.md"));
            let queue = temp.path().join(format!("QUEUE-{index}.md"));
            std::fs::write(&path, case["text"].as_str().unwrap()).unwrap();
            std::fs::write(&queue, case["queue"].as_str().unwrap()).unwrap();
            let result = run_cli(&path, Some(&queue), case["quiet"].as_bool().unwrap()).unwrap();
            actual.push(
                json!({"exit":result.exit_code,"stdout":result.stdout,"stderr":result.stderr}),
            );
        }
        let expected =
            crate::test_support::oracle_script("oracle_portable.py", "idle_pause", &json!(cases))
                .expect("required Python differential");
        assert_eq!(json!(actual), expected);

        let path = temp.path().join("PASS.md");
        let queue = temp.path().join("PASS-QUEUE.md");
        std::fs::write(&path, state()).unwrap();
        std::fs::write(&queue, "## Tier 0\n").unwrap();
        assert_eq!(
            run_cli(&path, Some(&queue), false).unwrap().stdout,
            format!("PASS: valid idle pause {}\n", path.display())
        );
    }

    #[test]
    fn validation_argparse_errors_and_help_match_python() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("STATE.md");
        let queue = temp.path().join("QUEUE.md");
        std::fs::write(&path, state()).unwrap();
        std::fs::write(&queue, "## Tier 0\n").unwrap();
        let args = [
            vec![],
            vec!["--queue".into()],
            vec!["--unknown".into()],
            vec!["--q".into()],
            vec!["--quiet=value".into(), path.to_string_lossy().into_owned()],
            vec![path.to_string_lossy().into_owned(), "extra".into()],
            vec!["--help".into()],
            vec![
                "--que".into(),
                queue.to_string_lossy().into_owned(),
                "--qui".into(),
                path.to_string_lossy().into_owned(),
            ],
            vec![
                "--quiet".into(),
                path.to_string_lossy().into_owned(),
                "--queue".into(),
                queue.to_string_lossy().into_owned(),
            ],
        ];
        let cases: Vec<_> = args
            .iter()
            .map(|argv| json!({"op":"cli_args","argv":argv}))
            .collect();
        let actual: Vec<_> = args
            .iter()
            .map(|argv| {
                let result = run_cli_args(argv).unwrap();
                json!({"exit":result.exit_code,"stdout":result.stdout,"stderr":result.stderr})
            })
            .collect();
        let expected =
            crate::test_support::oracle_script("oracle_portable.py", "idle_pause", &json!(cases))
                .expect("required Python differential");
        assert_eq!(json!(actual), expected);
    }

    #[test]
    fn read_failures_preserve_python_diagnostics_and_validation_order() {
        let root = ["/private/tmp", "/tmp"]
            .iter()
            .find_map(|directory| {
                tempfile::Builder::new()
                    .prefix("workflow-idle-pause-")
                    .tempdir_in(directory)
                    .ok()
            })
            .expect("isolated temp directory");
        let state_missing = root.path().join("missing-state.md");
        let queue_missing = root.path().join("missing-queue.md");
        let good_state = root.path().join("good-state.md");
        let invalid_state = root.path().join("invalid-state.md");
        std::fs::write(&good_state, state()).unwrap();
        std::fs::write(&invalid_state, "- **Run status:** RUNNING\n").unwrap();
        let cases = json!([
            {"op":"paths","state":state_missing,"queue":queue_missing},
            {"op":"paths","state":good_state,"text":state(),"queue":queue_missing},
            {"op":"paths","state":invalid_state,"text":"- **Run status:** RUNNING\n","queue":queue_missing},
        ]);
        let actual = [
            validate(&state_missing, Some(&queue_missing)).unwrap(),
            validate(&good_state, Some(&queue_missing)).unwrap(),
            validate(&invalid_state, Some(&queue_missing)).unwrap(),
        ];
        let expected =
            crate::test_support::oracle_script("oracle_portable.py", "idle_pause", &cases)
                .expect("required Python differential");
        assert_eq!(json!(actual), expected);
    }

    #[test]
    fn invalid_utf8_propagates_as_a_decode_error_for_state_and_queue() {
        let root = ["/private/tmp", "/tmp"]
            .iter()
            .find_map(|directory| {
                tempfile::Builder::new()
                    .prefix("workflow-idle-pause-utf8-")
                    .tempdir_in(directory)
                    .ok()
            })
            .expect("isolated temp directory");
        let state_path = root.path().join("STATE.md");
        let queue_path = root.path().join("QUEUE.md");
        let cases = json!([
            {"op":"invalid_utf8", "where":"state"},
            {"op":"invalid_utf8", "where":"queue", "state":state()},
        ]);
        let mut actual = Vec::new();
        for where_ in ["state", "queue"] {
            if where_ == "state" {
                std::fs::write(&state_path, [0xff]).unwrap();
                std::fs::write(&queue_path, "## Tier 0\n").unwrap();
            } else {
                std::fs::write(&state_path, state()).unwrap();
                std::fs::write(&queue_path, [0xff]).unwrap();
            }
            let error = validate(&state_path, Some(&queue_path)).unwrap_err();
            assert!(matches!(error, IdlePauseError::Decode { .. }));
            assert!(matches!(
                run_cli(&state_path, Some(&queue_path), false),
                Err(IdlePauseError::Decode { .. })
            ));
            actual.push(json!({"error_type":"UnicodeDecodeError"}));
        }
        let expected =
            crate::test_support::oracle_script("oracle_portable.py", "idle_pause", &cases)
                .expect("required Python differential");
        assert_eq!(json!(actual), expected);
    }

    #[test]
    fn pause_sentinels_and_fallback_fields_match_python() {
        let mut cases = vec![];
        for text in [
            state(),
            state().replace("released", "not released"),
            state().replace("(none)", "run-17"),
            state().replace("PAUSED", "NOT PAUSED"),
            state().replace("dry after bounded re-enumeration", "not dry"),
            state().replace(
                "material-change, explicit-restart",
                "human-directive, human-directive",
            ),
            state().replace("material-change, explicit-restart", "human-directive."),
            state().replace("material-change, explicit-restart", "timer-expiry"),
            state().replace("material-change, explicit-restart", "wait and see"),
            state()
                .replace(
                    "- **Conductor lease:** released",
                    "CONDUCTOR LEASE: released",
                )
                .replace("# STATE", "## Heartbeat"),
            state().replace(' ', "\u{1c}"),
            String::new(),
        ] {
            for queue in [
                "## Tier 0\n",
                "# Queue\n",
                "## Tier A\n|\n",
                "## Tier 0\n| A | DONE | - | - | - | A \\| B |\n",
                "## Tier 0\n| A | ACTIVE | - | - | - | note |\n",
            ] {
                cases.push(json!({"text":text,"queue":queue}));
            }
        }
        let actual: Vec<_> = cases
            .iter()
            .map(|c| {
                json!(
                    validate_text(c["text"].as_str().unwrap(), c["queue"].as_str().unwrap())
                        .unwrap()
                )
            })
            .collect();
        if let Some(expected) =
            crate::test_support::oracle_script("oracle_portable.py", "idle_pause", &json!(cases))
        {
            assert_eq!(json!(actual), expected);
        }
    }
}
