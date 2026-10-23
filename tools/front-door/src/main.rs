//! The provenant front door: resolve the product checkout, pick the owner of
//! a verb, and replace this process with it. Owners keep their own logic.

#![forbid(unsafe_code)]
// Rewrite pending: remove when this crate is rewritten.
#![allow(
    clippy::collapsible_if,
    clippy::disallowed_methods,
    clippy::print_stderr,
    clippy::unwrap_used
)]
mod exec;
mod help;
mod worktree_cli;

use exec::{Launch, replace_process};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

/// Identity is computed by build.rs from the complete source inventory. No
/// environment variable can override it at build or runtime.
fn version_json() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let version = provenant_records::VersionInfo {
        source_digest: provenant_records::SourceDigest::parse(env!("PROVENANT_SOURCE_DIGEST"))?,
        asset_digest: provenant_records::SourceDigest::parse(env!("PROVENANT_ASSET_DIGEST"))?,
        target: env!("PROVENANT_TARGET_TRIPLE").into(),
        toolchain: env!("PROVENANT_TOOLCHAIN").into(),
        record_schema_version: 1,
        config_schema_version: provenant_config::CONFIG_SCHEMA_VERSION,
    };
    Ok(provenant_records::encode(&version)?)
}

const LEASE_VERBS: [&str; 5] = ["acquire", "release", "verify", "status", "push"];
const FABRIC_CLI: &str = "runtime/fabric/bin/fabric";
/// Owner paths by verb, relative to the selected root. Verbs listed in
/// `PREFIXED` also pass their own name as the first owner argument.
const OWNERS: &[(&str, &str)] = &[
    ("digest", "scripts/utility.py"),
    ("classify", "scripts/utility.py"),
    ("extract", "scripts/utility.py"),
    ("route", "scripts/model-route"),
    ("capabilities", "skills/orchestrate/scripts/capabilities.py"),
    ("clean", "scripts/clean.py"),
    ("reap", "scripts/reap.py"),
    ("pr", "scripts/provenant-pr"),
    ("lint", "scripts/provenant_lint.py"),
    ("check", "scripts/check-harness"),
    ("gate", "scripts/gate"),
    ("telemetry", "scripts/provenant-telemetry"),
    ("stats", "scripts/provenant-telemetry"),
    ("retro", "scripts/provenant-telemetry"),
    ("quota", "scripts/provenant-quota"),
    ("profile", "scripts/provenant-profile"),
    ("fabric", FABRIC_CLI),
    ("hosts", "scripts/fabric-hosts"),
    ("peer", "scripts/fabric-hosts"),
    ("dispatch", "skills/orchestrate/scripts/dispatch_run.py"),
    ("batch", "skills/orchestrate/scripts/batch_run.py"),
    ("run", "skills/orchestrate/scripts/run_controls.py"),
];
const PREFIXED: [&str; 7] = [
    "stats", "retro", "hosts", "peer", "digest", "classify", "extract",
];
/// What a command line asks for, decided without touching the filesystem.
#[derive(Debug, PartialEq)]
enum Route {
    Print {
        text: String,
        stderr: bool,
        status: u8,
    },
    Root,
    Mcp,
    Check(Vec<OsString>),
    Owner {
        relative: &'static str,
        args: Vec<OsString>,
    },
}

fn usage() -> Route {
    Route::Print {
        text: help::USAGE.to_string(),
        stderr: true,
        status: 2,
    }
}

fn route(args: &[OsString], seat: bool) -> Route {
    let Some((first, rest)) = args.split_first() else {
        return if seat { Route::Mcp } else { usage() };
    };
    let Some(mut command) = first.to_str() else {
        return usage();
    };
    let mut rest = rest.to_vec();
    if matches!(command, "lanes" | "events" | "graph") {
        rest.insert(0, first.clone());
        command = "fabric";
    }
    if command == "lease" {
        let lease = help::detail("lease").unwrap_or_default();
        let verb = rest.first().and_then(|verb| verb.to_str());
        match verb {
            Some("-h" | "--help") => {
                return Route::Print {
                    text: lease.to_string(),
                    stderr: false,
                    status: 0,
                };
            }
            Some(verb) if LEASE_VERBS.contains(&verb) => {
                rest[0] = format!("landing-{verb}").into();
                command = "fabric";
            }
            _ => {
                return Route::Print {
                    text: help::first_line(lease).to_string(),
                    stderr: true,
                    status: 2,
                };
            }
        }
    }
    if command == "routes" {
        rest.insert(0, "routes".into());
        command = "route";
    } else if command == "help" && rest == ["routes"] {
        rest = vec!["routes".into(), "--health".into()];
        command = "route";
    } else if matches!(command, "help" | "-h" | "--help") {
        let text = match rest.as_slice() {
            [] => Some(help::GENERAL.to_string()),
            [topic] => topic.to_str().and_then(help::detail).map(str::to_string),
            _ => None,
        };
        return text.map_or_else(usage, |text| Route::Print {
            text,
            stderr: false,
            status: 0,
        });
    }
    if command == "mcp" {
        return if rest.is_empty() { Route::Mcp } else { usage() };
    }
    if command == "root" {
        return if rest.is_empty() {
            Route::Root
        } else {
            usage()
        };
    }
    if PREFIXED.contains(&command) {
        rest.insert(0, command.into());
    }
    match (command, OWNERS.iter().find(|(verb, _)| *verb == command)) {
        ("check", _) => Route::Check(rest),
        (_, Some((_, relative))) => Route::Owner {
            relative,
            args: rest,
        },
        _ => usage(),
    }
}

fn print(text: &str, stderr: bool) {
    // A closed pipe is the reader's business, not an error worth a status.
    let _ = if stderr {
        std::io::stderr().write_all(text.as_bytes())
    } else {
        std::io::stdout().write_all(text.as_bytes())
    };
}

fn fail(message: &str) -> u8 {
    eprintln!("provenant: {message}");
    3
}

const FABRIC_MCP: &str = "runtime/fabric/bin/fabric-mcp";
fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
}
/// Failures selecting the immutable release or an explicit build fixture.
#[derive(Debug)]
enum ProductRootError {
    CurrentExecutable(io::Error),
    MissingExecutableParent(PathBuf),
    MissingExplicitRoot,
    RelativeExplicitRoot(PathBuf),
    ExplicitRoot { path: PathBuf, source: io::Error },
    ExplicitRootNotDirectory(PathBuf),
}

impl fmt::Display for ProductRootError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentExecutable(error) => {
                write!(formatter, "cannot resolve the current executable: {error}")
            }
            Self::MissingExecutableParent(path) => {
                write!(
                    formatter,
                    "current executable has no parent: {}",
                    path.display()
                )
            }
            Self::MissingExplicitRoot => {
                formatter.write_str("uninstalled binary requires an explicit PROVENANT_ROOT")
            }
            Self::RelativeExplicitRoot(path) => write!(
                formatter,
                "PROVENANT_ROOT must be an absolute directory: {}",
                path.display()
            ),
            Self::ExplicitRoot { path, source } => write!(
                formatter,
                "cannot resolve PROVENANT_ROOT {}: {source}",
                path.display()
            ),
            Self::ExplicitRootNotDirectory(path) => write!(
                formatter,
                "PROVENANT_ROOT is not a directory: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ProductRootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CurrentExecutable(error) => Some(error),
            Self::ExplicitRoot { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Installed executables live under `<release>/bin/provenant`. The manifest
/// next to that location identifies the root. Build-tree binaries have no
/// manifest and need an explicit fixture root.
fn product_root() -> Result<PathBuf, ProductRootError> {
    let executable = std::env::current_exe().map_err(ProductRootError::CurrentExecutable)?;
    let executable =
        std::fs::canonicalize(executable).map_err(ProductRootError::CurrentExecutable)?;
    let parent = executable
        .parent()
        .ok_or_else(|| ProductRootError::MissingExecutableParent(executable.clone()))?;
    if parent.file_name() == Some(OsStr::new("bin")) {
        let candidate = parent
            .parent()
            .ok_or_else(|| ProductRootError::MissingExecutableParent(executable.clone()))?;
        if is_file(&candidate.join("release.json")) {
            return Ok(candidate.to_path_buf());
        }
    }

    let Some(value) = std::env::var_os("PROVENANT_ROOT") else {
        return Err(ProductRootError::MissingExplicitRoot);
    };
    let root = PathBuf::from(value);
    if !root.is_absolute() {
        return Err(ProductRootError::RelativeExplicitRoot(root));
    }
    let root = std::fs::canonicalize(&root)
        .map_err(|source| ProductRootError::ExplicitRoot { path: root, source })?;
    if !root.is_dir() {
        return Err(ProductRootError::ExplicitRootNotDirectory(root));
    }
    Ok(root)
}

fn launch(program: &Path, args: &[OsString], scrub_git: bool) -> u8 {
    replace_process(&Launch {
        program,
        args,
        scrub_git,
    })
}

#[derive(Debug)]
enum OwnerAvailabilityError {
    PythonOwnerUnavailableInInstalledRelease,
}
impl fmt::Display for OwnerAvailabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PythonOwnerUnavailableInInstalledRelease => {
                formatter.write_str("owner unavailable in installed release")
            }
        }
    }
}

fn has_explicit_python() -> bool {
    std::env::var_os("HARNESS_PYTHON").is_some_and(|value| !value.is_empty())
}

fn has_git_metadata(root: &Path) -> bool {
    std::fs::symlink_metadata(root.join(".git")).is_ok()
}

fn python_owner_unavailable(root: &Path) -> bool {
    if !is_file(&root.join("release.json")) || has_explicit_python() {
        return false;
    }
    let venv_python = root.join(".venv/bin/python");
    !exec::is_executable(&venv_python) && !has_git_metadata(root)
}

fn run_owner(relative: &'static str, args: &[OsString]) -> u8 {
    let root = match product_root() {
        Ok(root) => root,
        Err(error) => return fail(&error.to_string()),
    };
    if python_owner_unavailable(&root) {
        return fail(&OwnerAvailabilityError::PythonOwnerUnavailableInInstalledRelease.to_string());
    }
    let program = root.join(relative);
    if relative.ends_with(".py") && is_file(&program) && exec::is_executable(&program) {
        let helper = root.join("scripts/lib/harness-python.sh");
        if !is_file(&helper) {
            return fail(&format!(
                "cannot execute {}: Python helper is missing at {}; {}",
                program.display(),
                helper.display(),
                exec::REPAIR
            ));
        }
        let mut forwarded = vec![helper.into_os_string(), program.into_os_string()];
        forwarded.extend_from_slice(args);
        launch(Path::new("/bin/bash"), &forwarded, false)
    } else {
        launch(&program, args, false)
    }
}

#[derive(Debug)]
enum CheckTargetError {
    CurrentDirectory,
    GitUnavailable,
    GitCommand,
    NotGitCheckout,
    InvalidGitOutput,
    InvalidCheckoutRoot,
    NotProvenantCheckout,
}
impl fmt::Display for CheckTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory => formatter.write_str("cannot resolve the current directory"),
            Self::GitUnavailable => formatter.write_str("Git is unavailable"),
            Self::GitCommand => formatter.write_str("cannot run Git checkout discovery"),
            Self::NotGitCheckout | Self::InvalidGitOutput | Self::InvalidCheckoutRoot => {
                formatter.write_str("check requires a Provenant Git checkout")
            }
            Self::NotProvenantCheckout => {
                formatter.write_str("check requires a Provenant Git checkout with HARNESS.md and an executable scripts/check-harness")
            }
        }
    }
}

fn trusted_git() -> Option<&'static Path> {
    for candidate in [
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
        "/opt/local/bin/git",
        "/usr/bin/git",
        "/bin/git",
    ] {
        let path = Path::new(candidate);
        if exec::is_executable(path) {
            return Some(path);
        }
    }
    None
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn check_target() -> Result<PathBuf, CheckTargetError> {
    let cwd = std::env::current_dir().map_err(|_| CheckTargetError::CurrentDirectory)?;
    let git = trusted_git().ok_or(CheckTargetError::GitUnavailable)?;
    let mut command = Command::new(git);
    command
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    for variable in exec::GIT_REDIRECTS {
        command.env_remove(variable);
    }
    for (name, _) in std::env::vars_os() {
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("GIT_CONFIG_KEY_")
            || name.starts_with("GIT_CONFIG_VALUE_")
            || matches!(
                name,
                "GIT_CONFIG_COUNT"
                    | "GIT_CONFIG_PARAMETERS"
                    | "GIT_CONFIG_GLOBAL"
                    | "GIT_CONFIG_SYSTEM"
                    | "GIT_CONFIG_NOSYSTEM"
            )
        {
            command.env_remove(name);
        }
    }
    let output = command.output().map_err(|_| CheckTargetError::GitCommand)?;
    if !output.status.success() {
        return Err(CheckTargetError::NotGitCheckout);
    }
    let mut bytes = output.stdout;
    if bytes.last() != Some(&b'\n') {
        return Err(CheckTargetError::InvalidGitOutput);
    }
    bytes.pop();
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if bytes.is_empty() || bytes.contains(&b'\n') || bytes.contains(&0) {
        return Err(CheckTargetError::InvalidGitOutput);
    }
    let top = PathBuf::from(OsString::from_vec(bytes));
    if !top.is_absolute() {
        return Err(CheckTargetError::InvalidGitOutput);
    }
    let root = std::fs::canonicalize(top).map_err(|_| CheckTargetError::InvalidCheckoutRoot)?;
    let checker = root.join("scripts/check-harness");
    if !is_regular_file(&root.join("HARNESS.md"))
        || !is_regular_file(&checker)
        || !exec::is_executable(&checker)
    {
        return Err(CheckTargetError::NotProvenantCheckout);
    }
    let resolved_checker =
        std::fs::canonicalize(&checker).map_err(|_| CheckTargetError::NotProvenantCheckout)?;
    if !resolved_checker.starts_with(&root) {
        return Err(CheckTargetError::NotProvenantCheckout);
    }
    Ok(checker)
}

fn run_check(args: &[OsString]) -> u8 {
    let checker = match check_target() {
        Ok(checker) => checker,
        Err(error) => {
            eprintln!("provenant: usage error: {error}");
            return 2;
        }
    };
    launch(&checker, args, true)
}

fn run(args: &[OsString]) -> u8 {
    if args == ["--version", "--json"] {
        return match version_json() {
            Ok(mut bytes) => {
                bytes.push(b'\n');
                if std::io::stdout().write_all(&bytes).is_ok() {
                    0
                } else {
                    3
                }
            }
            Err(error) => fail(&error.to_string()),
        };
    }
    if args == ["--version"] {
        print(
            concat!("provenant ", env!("PROVENANT_SOURCE_DIGEST"), "\n"),
            false,
        );
        return 0;
    }

    if args.first().is_some_and(|v| v == "internal")
        && args.get(1).is_some_and(|v| v == "supervise")
    {
        return match provenant_supervisor::supervise(&args[2..], Default::default()) {
            Ok(()) => 0,
            Err(error) => fail(&error.to_string()),
        };
    }
    let worktree_args: Vec<OsString> = std::iter::once(OsString::from("provenant"))
        .chain(args.iter().cloned())
        .collect();
    if worktree_cli::is_worktree_invocation(&worktree_args) {
        return worktree_cli::run(&worktree_args);
    }
    match route(args, std::env::var_os("AGENT_FABRIC_SEAT").is_some()) {
        Route::Print {
            text,
            stderr,
            status,
        } => {
            print(&text, stderr);
            status
        }
        Route::Root => match product_root() {
            Ok(root) => {
                let mut line = root.as_os_str().as_bytes().to_vec();
                line.push(b'\n');
                let _ = std::io::stdout().write_all(&line);
                0
            }
            Err(message) => fail(&message.to_string()),
        },
        Route::Mcp => run_owner(FABRIC_MCP, &[]),
        Route::Check(rest) => run_check(&rest),
        Route::Owner { relative, args } => run_owner(relative, &args),
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    ExitCode::from(run(&args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn owner(relative: &'static str, words: &[&str]) -> Route {
        Route::Owner {
            relative,
            args: args(words),
        }
    }

    #[test]
    fn empty_argv_is_mcp_with_a_seat_and_usage_without() {
        assert_eq!(route(&[], true), Route::Mcp);
        assert_eq!(route(&[], false), usage());
        assert_eq!(route(&args(&["x"]), true), usage());
    }

    #[test]
    fn verbs_pass_arguments_unchanged_or_with_their_prefix() {
        for verb in ["digest", "classify", "extract"] {
            assert_eq!(
                route(&args(&[verb, "file", "arg"]), false),
                owner("scripts/utility.py", &[verb, "file", "arg"])
            );
        }
        assert_eq!(
            route(&args(&["route", "a", ""]), false),
            owner("scripts/model-route", &["a", ""])
        );
        assert_eq!(
            route(&args(&["routes", "--all"]), false),
            owner("scripts/model-route", &["routes", "--all"])
        );
        assert_eq!(
            route(&args(&["lanes", "--json"]), false),
            owner(FABRIC_CLI, &["lanes", "--json"])
        );
        assert_eq!(
            route(&args(&["events"]), false),
            owner(FABRIC_CLI, &["events"])
        );
        assert_eq!(
            route(&args(&["stats", "-x"]), false),
            owner("scripts/provenant-telemetry", &["stats", "-x"])
        );
        assert_eq!(
            route(&args(&["retro"]), false),
            owner("scripts/provenant-telemetry", &["retro"])
        );
        assert_eq!(
            route(&args(&["telemetry", "ingest"]), false),
            owner("scripts/provenant-telemetry", &["ingest"])
        );
        assert_eq!(
            route(&args(&["peer"]), false),
            owner("scripts/fabric-hosts", &["peer"])
        );
        assert_eq!(
            route(&args(&["hosts", "list"]), false),
            owner("scripts/fabric-hosts", &["hosts", "list"])
        );
        assert_eq!(
            route(&args(&["lint", "commit-msg", "f"]), false),
            owner("scripts/provenant_lint.py", &["commit-msg", "f"])
        );
        assert_eq!(
            route(&args(&["check", "--fast"]), false),
            Route::Check(args(&["--fast"]))
        );
    }

    #[test]
    fn native_worktree_entry_receives_program_and_frozen_arguments() {
        assert_eq!(run(&args(&["worktree", "--help"])), 0);
        assert_eq!(run(&args(&["worktree", "list", "--unsupported"])), 2);
    }

    #[test]
    fn every_help_verb_has_an_owner_or_special_route() {
        for verb in [
            "route",
            "digest",
            "classify",
            "extract",
            "capabilities",
            "worktree",
            "clean",
            "reap",
            "pr",
            "lint",
            "check",
            "gate",
            "telemetry",
            "stats",
            "retro",
            "quota",
            "profile",
            "fabric",
            "hosts",
            "peer",
            "dispatch",
            "batch",
            "run",
        ] {
            assert!(
                matches!(
                    route(&args(&[verb]), false),
                    Route::Owner { .. } | Route::Check(_)
                ) || worktree_cli::is_worktree_invocation(&args(&["provenant", verb])),
                "{verb}"
            );
        }
    }

    #[test]
    fn help_routes_executes_and_dash_h_routes_prints() {
        assert_eq!(
            route(&args(&["help", "routes"]), false),
            owner("scripts/model-route", &["routes", "--health"])
        );
        assert_eq!(route(&args(&["help", "routes", "x"]), false), usage());
        for flag in ["-h", "--help"] {
            let Route::Print {
                text,
                status: 0,
                stderr: false,
            } = route(&args(&[flag, "routes"]), false)
            else {
                panic!("{flag}");
            };
            assert_eq!(Some(text.as_str()), help::detail("routes"));
        }
    }

    #[test]
    fn help_prints_general_detail_or_usage() {
        for word in ["help", "-h", "--help"] {
            assert_eq!(
                route(&args(&[word]), false),
                Route::Print {
                    text: help::GENERAL.to_string(),
                    stderr: false,
                    status: 0
                }
            );
        }
        assert_eq!(route(&args(&["help", "bogus"]), false), usage());
        assert_eq!(route(&args(&["help", "a", "b"]), false), usage());
        assert_eq!(route(&args(&["-h", "bogus"]), false), usage());
        for topic in [
            "root", "lint", "lease", "help", "check", "digest", "classify", "extract",
        ] {
            let Route::Print { text, .. } = route(&args(&["help", topic]), false) else {
                panic!()
            };
            assert_eq!(Some(text.as_str()), help::detail(topic));
        }
    }

    #[test]
    fn unknown_commands_are_usage_errors() {
        for word in ["bogus", "", "-v", "ROUTE", "Route", "--json"] {
            assert_eq!(route(&args(&[word]), false), usage(), "{word:?}");
        }
    }

    #[test]
    fn root_takes_no_arguments() {
        assert_eq!(route(&args(&["root"]), false), Route::Root);
        assert_eq!(route(&args(&["root", "x"]), false), usage());
    }

    #[test]
    fn lease_rewrites_to_fabric_landing_verbs() {
        for verb in LEASE_VERBS {
            let landing = format!("landing-{verb}");
            assert_eq!(
                route(&args(&["lease", verb, "s", "1"]), false),
                owner(FABRIC_CLI, &[&landing, "s", "1"])
            );
        }
    }

    #[test]
    fn lease_help_and_errors() {
        let lease = help::detail("lease").unwrap();
        for words in [
            &["lease", "-h"][..],
            &["lease", "--help", "acquire"],
            &["lease", "-h", "junk"],
        ] {
            assert_eq!(
                route(&args(words), false),
                Route::Print {
                    text: lease.to_string(),
                    stderr: false,
                    status: 0
                }
            );
        }
        for words in [
            &["lease"][..],
            &["lease", "wait"],
            &["lease", "--json"],
            &["lease", ""],
        ] {
            assert_eq!(
                route(&args(words), false),
                Route::Print {
                    text: help::first_line(lease).to_string(),
                    stderr: true,
                    status: 2
                }
            );
        }
    }

    #[test]
    fn non_utf8_arguments_pass_through_untouched() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![b'a', 0xff, b'b']);
        assert_eq!(
            route(&[OsString::from("fabric"), raw.clone()], false),
            Route::Owner {
                relative: FABRIC_CLI,
                args: vec![raw.clone()]
            }
        );
        assert_eq!(route(&[raw], false), usage());
    }

    #[test]
    fn version_reports_typed_build_identity() {
        let value: serde_json::Value = serde_json::from_slice(&version_json().unwrap()).unwrap();
        assert_eq!(value["source_digest"], env!("PROVENANT_SOURCE_DIGEST"));
        assert_eq!(value["config_schema_version"], 1);
    }
}
