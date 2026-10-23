//! Installation-check interface. Config and binary inspection remain with their owners.
use crate::CommandOutput;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(name = "check-provenant-install", args_override_self = true)]
pub struct Arguments {
    /// Fail on routing or provider warnings.
    #[arg(long)]
    pub strict: bool,
}

pub const PROVIDERS: [&str; 6] = ["claude", "codex", "opencode", "agy", "cursor", "kiro"];

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub present: bool,
    pub skills: bool,
    pub registered: bool,
}

/// Ordered validation failure, selected by the installation owner before rendering.
#[derive(Debug, Error)]
pub enum Failure {
    #[error("Agent Fabric instance root must be absolute")]
    RelativeInstance,
    #[error("product-root pointer is invalid or stale at {0}")]
    InvalidPointer(PathBuf),
    #[error("PROVENANT_BIN_DIR must be absolute")]
    RelativeBin,
    #[error("{0} must be a regular managed copy; re-run install-harness")]
    Unmanaged(PathBuf),
    #[error("{0} is not executable; re-run install-harness")]
    NotExecutable(PathBuf),
    #[error("installed front door has no Rust source identity; re-run install-harness")]
    MissingIdentity,
    #[error("installed front door was built from different sources than tools/front-door (embeds {stamped}, checkout is {current}); rebuild: re-run install-harness")]
    StaleIdentity { stamped: String, current: String },
}

#[derive(Debug)]
pub enum Status {
    NotOwned,
    Failed(Failure),
    Installed {
        command: PathBuf,
        providers: [ProviderStatus; 6],
        routing: Result<Vec<String>, String>,
    },
}

pub fn render(status: &Status, strict: bool, advisories: &[String]) -> CommandOutput {
    let mut out = CommandOutput::default();
    for line in advisories {
        out.stderr.push_str(line);
        out.stderr.push('\n');
    }
    match status {
        Status::NotOwned => out
            .stdout
            .push_str("provenant installed stub=not-owned-by-this-checkout\n"),
        Status::Failed(error) => {
            out.stderr.push_str(&format!("FAIL: {error}\n"));
            out.exit_code = 1;
        }
        Status::Installed {
            command,
            providers,
            routing,
        } => {
            out.stdout.push_str(&format!(
                "provenant installed stub=ok path={}\nfront-door=rust\n",
                command.display()
            ));
            let mut missing = false;
            for (name, state) in PROVIDERS.iter().zip(providers) {
                if !state.present {
                    out.stdout
                        .push_str(&format!("provider {name} present=no\n"));
                    continue;
                }
                let complete = state.skills && state.registered;
                missing |= !complete;
                let skills = if state.skills { "ok" } else { "missing" };
                let mcp = if state.registered { "ok" } else { "missing" };
                let repair = if complete {
                    ""
                } else {
                    " repair=install-harness --platform all"
                };
                out.stdout.push_str(&format!(
                    "provider {name} present=yes skills={skills} mcp={mcp}{repair}\n"
                ));
            }
            let unreadable = vec!["unreadable".to_owned()];
            let differences = match routing {
                Ok(keys) => keys,
                Err(error) => {
                    out.stderr.push_str(&format!("warning: routing catalogue {error}; repair=install-harness --platform all --refresh-routing\n"));
                    &unreadable
                }
            };
            for key in differences {
                out.stderr.push_str(&format!(
                    "routing drift={key} repair=install-harness --platform all --refresh-routing\n"
                ));
            }
            out.exit_code = u8::from(strict && (missing || !differences.is_empty()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_and_failures_have_stable_exit_and_streams() {
        assert_eq!(
            render(&Status::NotOwned, true, &[]),
            CommandOutput {
                stdout: "provenant installed stub=not-owned-by-this-checkout\n".into(),
                ..Default::default()
            }
        );
        let out = render(
            &Status::Failed(Failure::InvalidPointer("/p".into())),
            false,
            &[],
        );
        assert_eq!(out.exit_code, 1);
        assert_eq!(
            out.stderr,
            "FAIL: product-root pointer is invalid or stale at /p\n"
        );
    }

    #[test]
    fn strict_only_fails_present_incomplete_providers_or_routing_drift() {
        let mut providers = [ProviderStatus::default(); 6];
        providers[3].registered = true;
        let status = |providers, routing| Status::Installed {
            command: "/bin/provenant".into(),
            providers,
            routing,
        };
        assert_eq!(
            render(&status(providers, Ok(vec![])), true, &[]).exit_code,
            0
        );
        providers[2].present = true;
        let value = status(providers, Ok(vec![]));
        assert_eq!(render(&value, false, &[]).exit_code, 0);
        let out = render(&value, true, &[]);
        assert_eq!(out.exit_code, 1);
        assert!(out.stdout.contains("provider opencode present=yes skills=missing mcp=missing repair=install-harness --platform all\n"));
        assert!(out.stdout.contains("provider agy present=no\n"));
        let out = render(
            &status([ProviderStatus::default(); 6], Err("bad data".into())),
            true,
            &[],
        );
        assert_eq!(out.exit_code, 1);
        assert_eq!(out.stderr, "warning: routing catalogue bad data; repair=install-harness --platform all --refresh-routing\nrouting drift=unreadable repair=install-harness --platform all --refresh-routing\n");
    }

    #[test]
    fn flags_are_structural() {
        assert!(
            Arguments::try_parse_from(["check", "--strict"])
                .unwrap()
                .strict
        );
        assert!(Arguments::try_parse_from(["check", "--unknown"]).is_err());
        assert!(
            Arguments::try_parse_from(["check", "--strict", "--strict"])
                .unwrap()
                .strict
        );
    }
}
