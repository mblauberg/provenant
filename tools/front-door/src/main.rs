//! The provenant front door: resolve the product checkout, pick the owner of
//! a verb, and replace this process with it. Owners keep their own logic.

mod check;
mod exec;
mod help;
mod json;
mod resolve;

use exec::{replace_process, Launch};
use help::Caps;
use resolve::{is_file, Settings, FABRIC_MCP};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Identity block read by the installer to tell a managed binary from a
/// foreign file. It is the ADR 0019 marker line plus the source digest, each
/// on its own line, so a byte search finds it.
#[used]
static IDENTITY: [u8; identity::LEN] = identity::BYTES;

mod identity {
    const HEAD: &str = "\n# Managed by scripts/install-harness; local edits to an installed copy are not preserved.\nprovenant-front-door source-sha256=";
    const SHA: &str = match option_env!("PROVENANT_FD_SOURCE_SHA256") {
        Some(sha) => sha,
        None => "unknown",
    };
    pub const LEN: usize = HEAD.len() + SHA.len() + 1;
    pub const BYTES: [u8; LEN] = {
        let mut out = [b'\n'; LEN];
        let (head, sha) = (HEAD.as_bytes(), SHA.as_bytes());
        let mut at = 0;
        while at < head.len() {
            out[at] = head[at];
            at += 1;
        }
        let mut i = 0;
        while i < sha.len() {
            out[head.len() + i] = sha[i];
            i += 1;
        }
        out
    };
}

const LEASE_VERBS: [&str; 5] = ["acquire", "release", "verify", "status", "push"];
const FABRIC_CLI: &str = "runtime/fabric/bin/fabric";
/// Presence of these marks #1094 and #1095 in a tree.
const LINT_OWNER: &str = "scripts/provenant_lint.py";
const LEASE_OWNER: &str = "runtime/fabric/src/landing-cli.ts";

/// Owner paths by verb, relative to the selected root. Verbs listed in
/// `PREFIXED` also pass their own name as the first owner argument.
const OWNERS: &[(&str, &str)] = &[
    ("route", "scripts/model-route"),
    ("capabilities", "skills/orchestrate/scripts/capabilities.py"),
    ("worktree", "scripts/worktree"),
    ("clean", "scripts/clean.py"),
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
const PREFIXED: [&str; 4] = ["stats", "retro", "hosts", "peer"];

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

fn usage(caps: Caps) -> Route {
    Route::Print {
        text: help::usage(caps),
        stderr: true,
        status: 2,
    }
}

fn route(args: &[OsString], seat: bool, caps: Caps) -> Route {
    let Some((first, rest)) = args.split_first() else {
        return if seat { Route::Mcp } else { usage(caps) };
    };
    let Some(mut command) = first.to_str() else {
        return usage(caps);
    };
    let mut rest = rest.to_vec();
    if matches!(command, "lanes" | "events") {
        rest.insert(0, first.clone());
        command = "fabric";
    }
    if !caps.has(command) {
        return usage(caps);
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
                }
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
                }
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
            [] => Some(help::general(caps)),
            [topic] => topic
                .to_str()
                .filter(|topic| caps.has(topic))
                .and_then(help::detail)
                .map(str::to_string),
            _ => None,
        };
        return text.map_or_else(
            || usage(caps),
            |text| Route::Print {
                text,
                stderr: false,
                status: 0,
            },
        );
    }
    if command == "root" {
        return if rest.is_empty() {
            Route::Root
        } else {
            usage(caps)
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
        _ => usage(caps),
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

/// Prefer the checkout this binary lives in, then the resolved product root.
/// Only the resolved case rewrites the environment.
fn target(relative: &str, settings: &Settings) -> Result<(PathBuf, Option<PathBuf>), String> {
    if let Some(local) = resolve::local_root() {
        if is_file(&local.join(FABRIC_MCP)) && is_file(&local.join(relative)) {
            return Ok((local.join(relative), None));
        }
    }
    let root = resolve::product_root(settings)?;
    Ok((root.join(relative), Some(root)))
}

fn launch(program: &Path, args: &[OsString], root: Option<&Path>) -> u8 {
    replace_process(&Launch {
        program,
        args,
        root,
        scrub_git: false,
        stale_checked: false,
    })
}

fn run(args: &[OsString]) -> u8 {
    let settings = Settings::from_process();
    if std::env::var_os("PROVENANT_FRONT_DOOR").is_some_and(|mode| mode == "python") {
        return run_python(args, &settings);
    }
    let caps = capabilities(&settings);
    match route(args, std::env::var_os("AGENT_FABRIC_SEAT").is_some(), caps) {
        Route::Print {
            text,
            stderr,
            status,
        } => {
            print(&text, stderr);
            status
        }
        Route::Root => match resolve::product_root(&settings) {
            Ok(root) => {
                let mut line = root.as_os_str().as_bytes().to_vec();
                line.push(b'\n');
                let _ = std::io::stdout().write_all(&line);
                0
            }
            Err(message) => fail(&message),
        },
        Route::Mcp => match resolve::product_root(&settings) {
            Ok(root) => launch(&root.join(FABRIC_MCP), &[], Some(&root)),
            Err(message) => fail(&message),
        },
        Route::Check(rest) => match check::select(&settings) {
            Ok(selection) => replace_process(&Launch {
                program: &selection.checker,
                args: &rest,
                root: Some(&selection.root),
                scrub_git: true,
                stale_checked: true,
            }),
            Err(message) => message.map_or(3, |message| fail(&message)),
        },
        Route::Owner { relative, args } => match target(relative, &settings) {
            Ok((program, root)) => launch(&program, &args, root.as_deref()),
            Err(message) => fail(&message),
        },
    }
}

/// Which optional commands the tree this run would dispatch in provides:
/// the checkout the binary lives in, else the resolved product root, else
/// none (help still works without a root, and must not advertise guesses).
fn capabilities(settings: &Settings) -> Caps {
    let provides = |root: &Path| Caps {
        lint: is_file(&root.join(LINT_OWNER)),
        lease: is_file(&root.join(LEASE_OWNER)),
    };
    if let Some(local) = resolve::local_root() {
        if is_file(&local.join(FABRIC_MCP)) {
            return provides(&local);
        }
    }
    resolve::product_root(settings).map_or(Caps::NONE, |root| provides(&root))
}

/// Rollback switch: run the preserved Python dispatcher directly. The
/// stable template would exec `scripts/provenant`, which is this binary when
/// Rust is installed there, and loop back here.
fn run_python(args: &[OsString], settings: &Settings) -> u8 {
    let root = match resolve::product_root(settings) {
        Ok(root) => root,
        Err(message) => return fail(&message),
    };
    let preserved = root.join("scripts/provenant.py");
    let dispatcher = if is_file(&preserved) {
        preserved
    } else {
        root.join("scripts/provenant")
    };
    if !starts_with_shebang(&dispatcher) {
        return fail(&format!(
            "no Python dispatcher at {}; {}",
            dispatcher.display(),
            exec::REPAIR
        ));
    }
    let mut forwarded = vec![dispatcher.into_os_string()];
    forwarded.extend_from_slice(args);
    launch(
        Path::new("/usr/bin/env"),
        &[&[OsString::from("python3")][..], &forwarded].concat(),
        Some(&root),
    )
}

fn starts_with_shebang(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok()
        && &head == b"#!"
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
        assert_eq!(route(&[], true, Caps::ALL), Route::Mcp);
        assert_eq!(route(&[], false, Caps::ALL), usage(Caps::ALL));
        assert_eq!(route(&args(&["x"]), true, Caps::ALL), usage(Caps::ALL));
    }

    #[test]
    fn verbs_pass_arguments_unchanged_or_with_their_prefix() {
        assert_eq!(
            route(&args(&["route", "a", ""]), false, Caps::ALL),
            owner("scripts/model-route", &["a", ""])
        );
        assert_eq!(
            route(&args(&["routes", "--all"]), false, Caps::ALL),
            owner("scripts/model-route", &["routes", "--all"])
        );
        assert_eq!(
            route(&args(&["lanes", "--json"]), false, Caps::ALL),
            owner(FABRIC_CLI, &["lanes", "--json"])
        );
        assert_eq!(
            route(&args(&["events"]), false, Caps::ALL),
            owner(FABRIC_CLI, &["events"])
        );
        assert_eq!(
            route(&args(&["stats", "-x"]), false, Caps::ALL),
            owner("scripts/provenant-telemetry", &["stats", "-x"])
        );
        assert_eq!(
            route(&args(&["retro"]), false, Caps::ALL),
            owner("scripts/provenant-telemetry", &["retro"])
        );
        assert_eq!(
            route(&args(&["telemetry", "ingest"]), false, Caps::ALL),
            owner("scripts/provenant-telemetry", &["ingest"])
        );
        assert_eq!(
            route(&args(&["peer"]), false, Caps::ALL),
            owner("scripts/fabric-hosts", &["peer"])
        );
        assert_eq!(
            route(&args(&["hosts", "list"]), false, Caps::ALL),
            owner("scripts/fabric-hosts", &["hosts", "list"])
        );
        assert_eq!(
            route(&args(&["lint", "commit-msg", "f"]), false, Caps::ALL),
            owner("scripts/provenant_lint.py", &["commit-msg", "f"])
        );
        assert_eq!(
            route(&args(&["check", "--fast"]), false, Caps::ALL),
            Route::Check(args(&["--fast"]))
        );
    }

    #[test]
    fn every_help_verb_has_an_owner_or_special_route() {
        for verb in [
            "route",
            "capabilities",
            "worktree",
            "clean",
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
                    route(&args(&[verb]), false, Caps::ALL),
                    Route::Owner { .. } | Route::Check(_)
                ),
                "{verb}"
            );
        }
    }

    #[test]
    fn help_routes_executes_and_dash_h_routes_prints() {
        assert_eq!(
            route(&args(&["help", "routes"]), false, Caps::ALL),
            owner("scripts/model-route", &["routes", "--health"])
        );
        assert_eq!(
            route(&args(&["help", "routes", "x"]), false, Caps::ALL),
            usage(Caps::ALL)
        );
        for flag in ["-h", "--help"] {
            let Route::Print {
                text,
                status: 0,
                stderr: false,
            } = route(&args(&[flag, "routes"]), false, Caps::ALL)
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
                route(&args(&[word]), false, Caps::ALL),
                Route::Print {
                    text: help::general(Caps::ALL),
                    stderr: false,
                    status: 0
                }
            );
        }
        assert_eq!(
            route(&args(&["help", "bogus"]), false, Caps::ALL),
            usage(Caps::ALL)
        );
        assert_eq!(
            route(&args(&["help", "a", "b"]), false, Caps::ALL),
            usage(Caps::ALL)
        );
        assert_eq!(
            route(&args(&["-h", "bogus"]), false, Caps::ALL),
            usage(Caps::ALL)
        );
        for topic in ["root", "lint", "lease", "help", "check"] {
            let Route::Print { text, .. } = route(&args(&["help", topic]), false, Caps::ALL) else {
                panic!()
            };
            assert_eq!(Some(text.as_str()), help::detail(topic));
        }
    }

    #[test]
    fn unknown_commands_are_usage_errors() {
        for word in ["bogus", "", "--version", "-v", "ROUTE", "Route", "--json"] {
            assert_eq!(
                route(&args(&[word]), false, Caps::ALL),
                usage(Caps::ALL),
                "{word:?}"
            );
        }
    }

    #[test]
    fn root_takes_no_arguments() {
        assert_eq!(route(&args(&["root"]), false, Caps::ALL), Route::Root);
        assert_eq!(
            route(&args(&["root", "x"]), false, Caps::ALL),
            usage(Caps::ALL)
        );
    }

    #[test]
    fn lease_rewrites_to_fabric_landing_verbs() {
        for verb in LEASE_VERBS {
            let landing = format!("landing-{verb}");
            assert_eq!(
                route(&args(&["lease", verb, "s", "1"]), false, Caps::ALL),
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
                route(&args(words), false, Caps::ALL),
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
                route(&args(words), false, Caps::ALL),
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
            route(&[OsString::from("fabric"), raw.clone()], false, Caps::ALL),
            Route::Owner {
                relative: FABRIC_CLI,
                args: vec![raw.clone()]
            }
        );
        assert_eq!(route(&[raw], false, Caps::ALL), usage(Caps::ALL));
    }

    #[test]
    fn identity_block_has_its_own_marker_lines() {
        let text = String::from_utf8(IDENTITY.to_vec()).unwrap();
        assert!(text.contains("\n# Managed by scripts/install-harness; local edits to an installed copy are not preserved.\n"));
        assert!(text.contains("\nprovenant-front-door source-sha256="));
        assert!(text.ends_with('\n'));
    }
}
