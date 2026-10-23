//! Pure validation and bounded command contract for the documented Herdr CLI.
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const HELP_GROUPS: &[(&str, &[&str])] = &[
    ("root", &[]),
    ("agent", &["agent"]),
    ("pane", &["pane"]),
    ("wait", &["wait"]),
    ("integration", &["integration"]),
    ("status", &["status"]),
    ("api", &["api"]),
];
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
pub const AGENT_WAIT_STATUSES: &[&str] = &["idle", "working", "blocked", "unknown"];
pub const PANE_WAIT_STATUSES: &[&str] = &["idle", "working", "blocked", "done", "unknown"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ContractError(pub String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ProcessFailure {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct HostFailure {
    pub message: String,
}

/// One successful or unsuccessful non-mutating command result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub return_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The owning bounded-process adapter supplies subprocess effects at this seam.
pub trait HerdrCommandRunner {
    fn run_bounded(
        &self,
        binary: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutput, ProcessFailure>;
}

/// Lookup and reference-file effects supplied by the permitted host owner.
pub trait HerdrCliHost: HerdrCommandRunner {
    fn which(&self, name: &str) -> Result<Option<PathBuf>, HostFailure>;
    fn is_file(&self, path: &Path) -> Result<bool, HostFailure>;
    fn read_reference(&self, path: &Path) -> Result<String, HostFailure>;
}

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error(transparent)]
    Contract(#[from] ContractError),
    #[error(transparent)]
    Process(#[from] ProcessFailure),
    #[error(transparent)]
    Host(#[from] HostFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArguments {
    pub binary: Option<String>,
    pub reference: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgumentParse {
    Help(String),
    Arguments(CliArguments),
}

fn usage(program: &str) -> String {
    format!("usage: {program} [-h] [--binary BINARY] [--reference REFERENCE]\n")
}

fn help_text(program: &str) -> String {
    format!(
        "{}\n\noptions:\n  -h, --help            show this help message and exit\n  --binary BINARY\n  --reference REFERENCE\n",
        usage(program).trim_end()
    )
}

fn option_name(argument: &str) -> Option<&'static str> {
    if argument == "-h"
        || argument == "--help"
        || (argument.len() > 2 && "--help".starts_with(argument) && argument.starts_with("--"))
    {
        return Some("help");
    }
    let binary = "--binary";
    let reference = "--reference";
    let candidates = [binary, reference]
        .into_iter()
        .filter(|option| option.starts_with(argument));
    let mut candidates = candidates;
    let first = candidates.next()?;
    if candidates.next().is_some() {
        None
    } else if first == binary {
        Some("binary")
    } else {
        Some("reference")
    }
}

fn argparse_negative_number(value: &str) -> bool {
    Regex::new(r"\A-\.?\d")
        .map(|pattern| pattern.is_match(value))
        .unwrap_or(false)
}

/// Parse the source script's two argparse options, including its default
/// long-option abbreviation and help response.
pub fn parse_arguments(
    argv: &[String],
    program: &str,
    default_reference: &Path,
) -> Result<ArgumentParse, CliResult> {
    let program = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    let mut binary = None;
    let mut reference = default_reference.to_path_buf();
    let mut unrecognised = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let argument = &argv[index];
        if argument == "--" {
            unrecognised.extend(argv[index + 1..].iter().cloned());
            break;
        }
        let (option, inline_value) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(option, value)| {
                (option, Some(value))
            });
        let Some(name) = option_name(option) else {
            if option == "--" && inline_value.is_some() {
                return Err(CliResult {
                    exit_code: 2,
                    stdout: String::new(),
                    stderr: format!(
                        "{}{}: error: ambiguous option: {argument} could match --help, --binary, --reference\n",
                        usage(program),
                        program
                    ),
                });
            }
            unrecognised.push(argument.clone());
            index += 1;
            continue;
        };
        if name == "help" {
            if let Some(value) = inline_value {
                return Err(CliResult {
                    exit_code: 2,
                    stdout: String::new(),
                    stderr: format!(
                        "{}{}: error: argument -h/--help: ignored explicit argument {}\n",
                        usage(program),
                        program,
                        provenant_core::py::python_repr(value)
                    ),
                });
            }
            return Ok(ArgumentParse::Help(help_text(program)));
        }
        let value = if let Some(value) = inline_value {
            value.to_owned()
        } else if let Some(value) = argv
            .get(index + 1)
            .filter(|value| !value.starts_with('-') || argparse_negative_number(value))
        {
            index += 1;
            value.clone()
        } else {
            let option = if name == "binary" {
                "--binary"
            } else {
                "--reference"
            };
            return Err(CliResult {
                exit_code: 2,
                stdout: String::new(),
                stderr: format!(
                    "{}{}: error: argument {option}: expected one argument\n",
                    usage(program),
                    program
                ),
            });
        };
        match name {
            "binary" => binary = Some(value),
            "reference" => reference = PathBuf::from(value),
            _ => {
                return Err(CliResult {
                    exit_code: 2,
                    stdout: String::new(),
                    stderr: format!(
                        "{}{}: error: unrecognized arguments: {}\n",
                        usage(program),
                        program,
                        argument
                    ),
                });
            }
        }
        index += 1;
    }
    if !unrecognised.is_empty() {
        return Err(CliResult {
            exit_code: 2,
            stdout: String::new(),
            stderr: format!(
                "{}{}: error: unrecognized arguments: {}\n",
                usage(program),
                program,
                unrecognised.join(" ")
            ),
        });
    }
    Ok(ArgumentParse::Arguments(CliArguments { binary, reference }))
}

/// Resolve an explicit path as a file, or find a bare command on PATH.
pub fn resolve_binary(
    host: &impl HerdrCliHost,
    value: Option<&str>,
) -> Result<Option<PathBuf>, HostFailure> {
    let Some(value) = value else {
        return host.which("herdr");
    };
    let path = PathBuf::from(value);
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if path.is_absolute() || parent.is_some_and(|parent| parent != Path::new(".")) {
        return host.is_file(&path).map(|exists| exists.then_some(path));
    }
    host.which(value)
}

/// Source default: repository root is four parents above this crate manifest.
pub fn default_reference_path() -> PathBuf {
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..4 {
        let Some(parent) = root.parent() else {
            break;
        };
        root = parent.to_path_buf();
    }
    root.join("skills/orchestrate/references/herdr-panes.md")
}

/// Render the stable status line once binary lookup and reference reading have
/// been supplied by the edge adapter.
pub fn render_cli_result(result: Result<Option<String>, CheckError>) -> CliResult {
    match result {
        Ok(None) => CliResult {
            exit_code: 0,
            stdout: "HERDR CLI CHECK: SKIP - binary not found\n".into(),
            stderr: String::new(),
        },
        Ok(Some(version)) => CliResult {
            exit_code: 0,
            stdout: format!(
                "HERDR CLI CHECK: PASS - observed version {version}; required command surfaces available\n"
            ),
            stderr: String::new(),
        },
        Err(error) => CliResult {
            exit_code: 1,
            stdout: format!("HERDR CLI CHECK: FAIL - {error}\n"),
            stderr: String::new(),
        },
    }
}

fn regex(pattern: &str) -> Result<Regex, ContractError> {
    Regex::new(pattern).map_err(|error| ContractError(format!("invalid built-in pattern: {error}")))
}

fn inline_code_spans(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        if bytes[start] != b'`'
            || (start > 0 && bytes[start - 1] == b'`')
            || (start + 1 < bytes.len() && bytes[start + 1] == b'`')
        {
            start += 1;
            continue;
        }
        let content_start = start + 1;
        let mut end = content_start;
        while end < bytes.len() {
            if bytes[end] == b'`'
                && (end == 0 || bytes[end - 1] != b'`')
                && (end + 1 == bytes.len() || bytes[end + 1] != b'`')
            {
                spans.push(&text[content_start..end]);
                start = end + 1;
                break;
            }
            end += 1;
        }
        if end == bytes.len() {
            start += 1;
        }
    }
    spans
}

fn python_normalise_whitespace(text: &str) -> String {
    let mut words = Vec::new();
    let mut word = String::new();
    for character in text.chars() {
        if provenant_core::py::python_whitespace(character) {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(character);
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words.join(" ")
}

/// Documented commands are sorted to preserve Python's set iteration order at
/// each error boundary and to make the result deterministic for callers.
pub fn documented_commands(reference: &str) -> Result<Vec<String>, ContractError> {
    let command_pattern = regex(r"\bherdr(?: --help|(?: [a-z][a-z0-9-]*){1,2})")?;
    let integration_pattern = regex(r"\Aherdr integration install [a-z0-9-]+\z")?;
    let mut commands = BTreeSet::new();
    commands.extend(
        command_pattern
            .find_iter(reference)
            .map(|capture| capture.as_str().to_owned()),
    );
    for span in inline_code_spans(reference) {
        let normalised = python_normalise_whitespace(span);
        if normalised == "pane run" {
            commands.insert("herdr pane run".to_owned());
        }
        if integration_pattern.is_match(&normalised) {
            commands.insert(normalised);
        }
    }
    Ok(commands.into_iter().collect())
}

pub fn has_command(surface: &str, command: &str) -> Result<bool, ContractError> {
    let pattern = regex(&format!(r"(?m)^\s*{}(?:\s|$)", regex::escape(command)))?;
    Ok(pattern.is_match(surface))
}

fn sorted_list_repr(values: &BTreeSet<String>) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn status_set(surface: &str, command: &str) -> Result<BTreeSet<String>, ContractError> {
    let pattern = regex(&format!(
        r"(?m)^\s*{}\b[^\n]*?--status <([^>]+)>",
        regex::escape(command)
    ))?;
    let Some(captures) = pattern.captures(surface) else {
        return Err(ContractError(format!(
            "status surface is absent from help: {command}"
        )));
    };
    let statuses = captures
        .get(1)
        .ok_or_else(|| ContractError(format!("status surface is absent from help: {command}")))?;
    Ok(statuses.as_str().split('|').map(str::to_owned).collect())
}

fn get_surface<'a>(
    surfaces: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, ContractError> {
    surfaces
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| ContractError(format!("help surface is absent: {name}")))
}

pub fn validate_documented_commands(
    reference: &str,
    surfaces: &BTreeMap<String, String>,
) -> Result<(), ContractError> {
    for command in documented_commands(reference)? {
        let parts: Vec<_> = command.split_whitespace().collect();
        let present = if command == "herdr --help" {
            get_surface(surfaces, "root")?.contains("--help")
        } else if parts.len() == 2 {
            has_command(get_surface(surfaces, "root")?, &command)?
        } else {
            let surface = surfaces.get(parts[1]).map(String::as_str).unwrap_or("");
            has_command(surface, &command)?
        };
        if !present {
            return Err(ContractError(format!(
                "documented command is absent from help: {command}"
            )));
        }
    }
    Ok(())
}

pub fn validate_statuses(
    reference: &str,
    surfaces: &BTreeMap<String, String>,
) -> Result<(), ContractError> {
    let agent = status_set(get_surface(surfaces, "agent")?, "herdr agent wait")?;
    let pane = status_set(get_surface(surfaces, "wait")?, "herdr wait agent-status")?;
    let expected_agent: BTreeSet<String> =
        AGENT_WAIT_STATUSES.iter().map(|s| (*s).into()).collect();
    let expected_pane: BTreeSet<String> = PANE_WAIT_STATUSES.iter().map(|s| (*s).into()).collect();
    if agent != expected_agent {
        return Err(ContractError(format!(
            "agent wait statuses {} do not match {}",
            sorted_list_repr(&agent),
            sorted_list_repr(&expected_agent)
        )));
    }
    if pane != expected_pane {
        return Err(ContractError(format!(
            "pane wait statuses {} do not match {}",
            sorted_list_repr(&pane),
            sorted_list_repr(&expected_pane)
        )));
    }

    let status_paragraphs: Vec<_> = reference
        .split("\n\n")
        .filter(|paragraph| {
            paragraph.contains("`idle`")
                && paragraph.contains("`done`")
                && paragraph.contains("agent")
        })
        .collect();
    if status_paragraphs.len() != 1 {
        return Err(ContractError(
            "cannot identify the documented agent-status paragraph".into(),
        ));
    }
    let token_pattern = regex(r"`([a-z][a-z-]+)`")?;
    let documented: BTreeSet<String> = token_pattern
        .captures_iter(status_paragraphs[0])
        .filter_map(|captures| captures.get(1).map(|capture| capture.as_str().to_owned()))
        .collect();
    let unsupported: BTreeSet<_> = documented.difference(&pane).cloned().collect();
    if !unsupported.is_empty() {
        return Err(ContractError(format!(
            "documented status tokens are absent from help: {}",
            sorted_list_repr(&unsupported)
        )));
    }

    let example_pattern = regex(r"herdr agent wait[^\n]*?--status ([a-z-]+)")?;
    let examples: BTreeSet<String> = example_pattern
        .captures_iter(reference)
        .filter_map(|captures| captures.get(1).map(|capture| capture.as_str().to_owned()))
        .collect();
    let invalid: BTreeSet<_> = examples.difference(&agent).cloned().collect();
    if !invalid.is_empty() {
        return Err(ContractError(format!(
            "documented agent wait statuses are invalid: {}",
            sorted_list_repr(&invalid)
        )));
    }
    Ok(())
}

fn invoke(
    runner: &impl HerdrCommandRunner,
    binary: &str,
    args: &[&str],
) -> Result<String, CheckError> {
    let output = runner.run_bounded(binary, args, COMMAND_TIMEOUT)?;
    if output.return_code != 0 {
        let command = std::iter::once("herdr")
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        let detail = if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        };
        return Err(ContractError(format!(
            "non-mutating command failed: {command}: {}",
            provenant_core::py::python_strip(detail)
        ))
        .into());
    }
    Ok(format!("{}{}", output.stdout, output.stderr))
}

fn surfaces(
    runner: &impl HerdrCommandRunner,
    binary: &str,
) -> Result<BTreeMap<String, String>, CheckError> {
    let mut values = BTreeMap::new();
    for (key, group) in HELP_GROUPS {
        let mut args = group.to_vec();
        args.push("--help");
        values.insert((*key).to_owned(), invoke(runner, binary, &args)?);
    }
    Ok(values)
}

/// Run the version and non-mutating help contract through a bounded host seam.
pub fn check(
    binary: &str,
    reference: &str,
    runner: &impl HerdrCommandRunner,
) -> Result<String, CheckError> {
    let version_output = invoke(runner, binary, &["--version"])?;
    let version_output = provenant_core::py::python_strip(&version_output);
    let version_pattern = regex(r"\Aherdr\s+([^\s]+)\z")?;
    let Some(version) = version_pattern
        .captures(version_output)
        .and_then(|captures| captures.get(1).map(|capture| capture.as_str().to_owned()))
    else {
        return Err(ContractError(format!(
            "cannot parse Herdr version output: {}",
            provenant_core::py::python_repr(version_output)
        ))
        .into());
    };
    let surfaces = surfaces(runner, binary)?;
    validate_documented_commands(reference, &surfaces)?;
    validate_statuses(reference, &surfaces)?;
    Ok(version)
}

/// Read the source reference before invoking Herdr, as `Path.read_text` does.
pub fn check_reference(
    binary: &Path,
    reference: &Path,
    host: &impl HerdrCliHost,
) -> Result<String, CheckError> {
    let reference_text = host.read_reference(reference)?;
    let binary = binary.to_string_lossy();
    check(&binary, &reference_text, host)
}

/// Complete script policy through the host's path, file, reference and bounded
/// process seams. `argv` excludes the executable name, like argparse input.
pub fn run_main(argv: &[String], program: &str, host: &impl HerdrCliHost) -> CliResult {
    let arguments = match parse_arguments(argv, program, &default_reference_path()) {
        Ok(ArgumentParse::Help(help)) => {
            return CliResult {
                exit_code: 0,
                stdout: help,
                stderr: String::new(),
            };
        }
        Ok(ArgumentParse::Arguments(arguments)) => arguments,
        Err(result) => return result,
    };
    let binary = match resolve_binary(host, arguments.binary.as_deref()) {
        Ok(binary) => binary,
        Err(error) => return render_cli_result(Err(error.into())),
    };
    let Some(binary) = binary else {
        return render_cli_result(Ok(None));
    };
    render_cli_result(check_reference(&binary, &arguments.reference, host).map(Some))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::cell::RefCell;
    use std::collections::VecDeque;

    const REFERENCE: &str = "herdr pane run\n\nThe agent states are `idle`, `working`, `blocked`, `unknown`, and `done`.\n";

    fn valid_surfaces() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("root".into(), "herdr pane <subcommand>\n  --help\n".into()),
            (
                "agent".into(),
                "herdr agent wait <target> --status <idle|working|blocked|unknown>\n".into(),
            ),
            ("pane".into(), "herdr pane run <pane> <command>\n".into()),
            (
                "wait".into(),
                "herdr wait agent-status <pane> --status <idle|working|blocked|done|unknown>\n"
                    .into(),
            ),
            ("integration".into(), String::new()),
            ("status".into(), String::new()),
            ("api".into(), String::new()),
        ])
    }

    fn case_result(case: &Value) -> Value {
        let surfaces: BTreeMap<String, String> =
            serde_json::from_value(case["surfaces"].clone()).unwrap_or_default();
        let result = match case["op"].as_str() {
            Some("commands") => documented_commands(case["reference"].as_str().unwrap_or(""))
                .map(|values| json!(values)),
            Some("has_command") => has_command(
                case["surface"].as_str().unwrap_or(""),
                case["command"].as_str().unwrap_or(""),
            )
            .map(|value| json!(value)),
            Some("documented") => {
                validate_documented_commands(case["reference"].as_str().unwrap_or(""), &surfaces)
                    .map(|()| Value::Null)
            }
            Some("statuses") => {
                validate_statuses(case["reference"].as_str().unwrap_or(""), &surfaces)
                    .map(|()| Value::Null)
            }
            Some("version") => {
                let runner = FixtureRunner::from_case(case);
                check("herdr", case["reference"].as_str().unwrap_or(""), &runner)
                    .map(|version| json!(version))
                    .map_err(|error| ContractError(error.to_string()))
            }
            _ => Err(ContractError("unknown Herdr test operation".into())),
        };
        match result {
            Ok(value) => json!({"ok":value}),
            Err(error) => json!({"error":error.to_string()}),
        }
    }

    struct FixtureRunner {
        outputs: Vec<CommandOutput>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl FixtureRunner {
        fn from_case(case: &Value) -> Self {
            let outputs = case["outputs"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|row| CommandOutput {
                    return_code: row["return_code"].as_i64().unwrap_or(0) as i32,
                    stdout: row["stdout"].as_str().unwrap_or("").into(),
                    stderr: row["stderr"].as_str().unwrap_or("").into(),
                })
                .collect();
            Self {
                outputs,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl HerdrCommandRunner for FixtureRunner {
        fn run_bounded(
            &self,
            _binary: &str,
            args: &[&str],
            timeout: Duration,
        ) -> Result<CommandOutput, ProcessFailure> {
            assert_eq!(timeout, COMMAND_TIMEOUT);
            self.calls
                .borrow_mut()
                .push(args.iter().map(|arg| (*arg).into()).collect());
            let index = self.calls.borrow().len() - 1;
            self.outputs
                .get(index)
                .cloned()
                .ok_or_else(|| ProcessFailure {
                    message: "fixture output missing".into(),
                })
        }
    }

    struct FixtureCliHost {
        is_file: bool,
        reference: Result<String, HostFailure>,
        outputs: RefCell<VecDeque<CommandOutput>>,
        calls: RefCell<Vec<String>>,
    }

    impl FixtureCliHost {
        fn with_outputs(outputs: impl IntoIterator<Item = CommandOutput>) -> Self {
            Self {
                is_file: true,
                reference: Ok(REFERENCE.into()),
                outputs: RefCell::new(outputs.into_iter().collect()),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn missing_binary() -> Self {
            Self {
                is_file: false,
                reference: Ok(REFERENCE.into()),
                outputs: RefCell::new(VecDeque::new()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl HerdrCommandRunner for FixtureCliHost {
        fn run_bounded(
            &self,
            binary: &str,
            args: &[&str],
            timeout: Duration,
        ) -> Result<CommandOutput, ProcessFailure> {
            assert_eq!(timeout, COMMAND_TIMEOUT);
            self.calls
                .borrow_mut()
                .push(format!("run:{binary}:{}", args.join(" ")));
            self.outputs
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| ProcessFailure {
                    message: "fixture output missing".into(),
                })
        }
    }

    impl HerdrCliHost for FixtureCliHost {
        fn which(&self, name: &str) -> Result<Option<PathBuf>, HostFailure> {
            self.calls.borrow_mut().push(format!("which:{name}"));
            Ok(None)
        }

        fn is_file(&self, path: &Path) -> Result<bool, HostFailure> {
            self.calls
                .borrow_mut()
                .push(format!("is_file:{}", path.display()));
            Ok(self.is_file)
        }

        fn read_reference(&self, path: &Path) -> Result<String, HostFailure> {
            self.calls
                .borrow_mut()
                .push(format!("read:{}", path.display()));
            self.reference.clone()
        }
    }

    #[test]
    fn source_policy_and_error_text_match_python() {
        let good = valid_surfaces();
        let mut cases = vec![
            json!({"op":"commands","reference":"herdr --help\nherdr agent wait <target> --status idle\n`herdr integration install pi`\n`pane run`"}),
            json!({"op":"documented","reference":REFERENCE,"surfaces":good}),
        ];
        for changed in [
            "herdr agent wait <target> --status <idle|working|blocked|done|unknown>\n",
            "herdr wait agent-status <pane> --status <idle|working|blocked|unknown>\n",
        ] {
            let mut surfaces = valid_surfaces();
            let target = if changed.starts_with("herdr agent") {
                "agent"
            } else {
                "wait"
            };
            surfaces.insert(target.into(), changed.to_owned());
            cases.push(json!({"op":"statuses","reference":REFERENCE,"surfaces":surfaces}));
        }
        let mut missing = valid_surfaces();
        missing.insert("pane".into(), String::new());
        cases.push(json!({"op":"documented","reference":REFERENCE,"surfaces":missing}));
        let version_case = json!({"op":"version","reference":REFERENCE,"outputs":[
            {"return_code":0,"stdout":"herdr future-version\n","stderr":""},
            {"return_code":0,"stdout":valid_surfaces()["root"],"stderr":""},
            {"return_code":0,"stdout":valid_surfaces()["agent"],"stderr":""},
            {"return_code":0,"stdout":valid_surfaces()["pane"],"stderr":""},
            {"return_code":0,"stdout":valid_surfaces()["wait"],"stderr":""},
            {"return_code":0,"stdout":"","stderr":""},
            {"return_code":0,"stdout":"","stderr":""},
            {"return_code":0,"stdout":"","stderr":""}
        ]});
        cases.push(version_case.clone());
        let failed_command = json!({"op":"version","reference":REFERENCE,"outputs":[
            {"return_code":2,"stdout":"fallback output","stderr":"  command failed  \n"}
        ]});
        cases.push(failed_command);
        let actual: Vec<_> = cases.iter().map(case_result).collect();
        if let Some(expected) =
            crate::test_support::oracle_script("oracle_writer.py", "herdr_policy", &json!(cases))
        {
            assert_eq!(json!(actual), expected);
        }

        let runner = FixtureRunner::from_case(&version_case);
        assert_eq!(
            check("herdr", REFERENCE, &runner).unwrap(),
            "future-version"
        );
        assert_eq!(
            runner.calls.into_inner(),
            vec![
                vec!["--version"],
                vec!["--help"],
                vec!["agent", "--help"],
                vec!["pane", "--help"],
                vec!["wait", "--help"],
                vec!["integration", "--help"],
                vec!["status", "--help"],
                vec!["api", "--help"],
            ]
        );
    }

    #[test]
    fn cli_status_lines_keep_the_source_exit_codes() {
        assert_eq!(
            render_cli_result(Ok(None)),
            CliResult {
                exit_code: 0,
                stdout: "HERDR CLI CHECK: SKIP - binary not found\n".into(),
                stderr: String::new(),
            }
        );
        assert_eq!(
            render_cli_result(Ok(Some("future-version".into()))),
            CliResult {
                exit_code: 0,
                stdout: "HERDR CLI CHECK: PASS - observed version future-version; required command surfaces available\n".into(),
                stderr: String::new(),
            }
        );
        assert_eq!(
            render_cli_result(Err(ContractError("surface drift".into()).into())),
            CliResult {
                exit_code: 1,
                stdout: "HERDR CLI CHECK: FAIL - surface drift\n".into(),
                stderr: String::new(),
            }
        );
    }

    #[test]
    fn argparse_main_outputs_match_python_for_help_and_failures() {
        let cases = vec![
            json!({"argv":["-h"]}),
            json!({"argv":["--bin","/path/to/missing-herdr"]}),
            json!({"argv":["--reference","-1","--binary","/path/to/missing-herdr"]}),
            json!({"argv":["--binary","-1name"]}),
            json!({"argv":["--reference","-.2suffix","--binary","/path/to/missing-herdr"]}),
            json!({"argv":["--binary"]}),
            json!({"argv":["--unknown"]}),
            json!({"argv":["--help=detail"]}),
            json!({"argv":["--=value"]}),
        ];
        let actual: Vec<_> = cases
            .iter()
            .map(|case| {
                let argv: Vec<String> = case["argv"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let result = run_main(
                    &argv,
                    "/tmp/check_herdr_cli.py",
                    &FixtureCliHost::missing_binary(),
                );
                json!({"exit_code":result.exit_code,"stdout":result.stdout,"stderr":result.stderr})
            })
            .collect();
        if let Some(expected) =
            crate::test_support::oracle_script("oracle_writer.py", "herdr_args", &json!(cases))
        {
            assert_eq!(json!(actual), expected);
        }
    }

    #[test]
    fn main_reads_reference_before_bounded_commands_and_returns_source_status() {
        let surfaces = valid_surfaces();
        let mut outputs = vec![CommandOutput {
            return_code: 0,
            stdout: "herdr future-version\n".into(),
            stderr: String::new(),
        }];
        outputs.extend(HELP_GROUPS.iter().map(|(key, _)| CommandOutput {
            return_code: 0,
            stdout: surfaces[*key].clone(),
            stderr: String::new(),
        }));
        let host = FixtureCliHost::with_outputs(outputs);
        let result = run_main(
            &[
                "--binary".into(),
                "/fixture/herdr".into(),
                "--reference".into(),
                "/fixture/herdr-panes.md".into(),
            ],
            "check_herdr_cli.py",
            &host,
        );
        assert_eq!(
            result,
            CliResult {
                exit_code: 0,
                stdout: "HERDR CLI CHECK: PASS - observed version future-version; required command surfaces available\n".into(),
                stderr: String::new(),
            }
        );
        let calls = host.calls.borrow();
        assert_eq!(calls[0], "is_file:/fixture/herdr");
        assert_eq!(calls[1], "read:/fixture/herdr-panes.md");
        assert_eq!(calls[2], "run:/fixture/herdr:--version");
        assert_eq!(calls.len(), 10);

        let mut unreadable = FixtureCliHost::with_outputs([]);
        unreadable.reference = Err(HostFailure {
            message: "reference is unreadable".into(),
        });
        let result = run_main(
            &[
                "--binary".into(),
                "/fixture/herdr".into(),
                "--reference".into(),
                "/fixture/missing.md".into(),
            ],
            "check_herdr_cli.py",
            &unreadable,
        );
        assert_eq!(result.exit_code, 1);
        assert_eq!(
            result.stdout,
            "HERDR CLI CHECK: FAIL - reference is unreadable\n"
        );
        assert_eq!(
            unreadable.calls.borrow().last().map(String::as_str),
            Some("read:/fixture/missing.md")
        );
    }
}
