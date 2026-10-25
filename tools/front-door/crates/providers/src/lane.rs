//! Provenant-only lane customisation; no user configuration is loaded.
use crate::launch::{exact, subtree};
use crate::{ProviderError, SetupStep};
use provenant_confinement::permissions::ProviderProfileSpec;
use provenant_core::{AttemptId, RUN_STATE_DIRECTORY_ENV};
use provenant_records::Harness;
use serde_json::Value;
use std::{ffi::OsString, path::Path};

/// Canonical runtime paths supplied by the composition edge.
pub struct LaneRuntime<'a> {
    pub executable: &'a Path,
    /// Canonical host-selected Git, also pinned in MCP and hook environments.
    pub git: &'a Path,
    pub state: &'a Path,
    /// Isolated Run store for this attempt's children; never its own custody.
    pub child_state: &'a Path,
    pub home: &'a Path,
    pub depth: u64,
    pub attempt: AttemptId,
    pub assets: &'a Path,
}

impl LaneRuntime<'_> {
    pub(crate) fn mcp(&self, harness: Harness) -> Result<Value, ProviderError> {
        if ![
            self.executable,
            self.git,
            self.state,
            self.assets,
            self.child_state,
            self.home,
        ]
        .iter()
        .all(|p| p.is_absolute())
        {
            return Err(ProviderError::InvalidPath);
        }
        let mut entry = provenant_config::mcp_entry(harness, self.executable, self.state)
            .map_err(|_| ProviderError::UnsupportedPolicy)?;
        entry["env"] = serde_json::json!({
            "PROVENANT_AGENT_LABEL": format!("lane-{}", self.attempt),
            "PROVENANT_LANE_DEPTH": self.depth.to_string(),
            "PROVENANT_PARENT_ATTEMPT_ID": self.attempt.to_string(),
            RUN_STATE_DIRECTORY_ENV: self.child_state,
            "HOME": self.home,
            "PROVENANT_ROOT": self.assets,
            "PROVENANT_GIT": self.git,
        });
        Ok(entry)
    }

    pub(crate) fn claude(
        &self,
        scratch: &Path,
        argv: &mut Vec<OsString>,
        allowed_tools: &str,
    ) -> Result<Vec<SetupStep>, ProviderError> {
        let mcp = serde_json::json!({"mcpServers": {"fabric": self.mcp(Harness::Claude)?}});
        let settings = provenant_config::claude_lane_settings(self.executable, self.state)
            .map_err(|_| ProviderError::UnsupportedPolicy)?;
        let mut steps = Vec::new();
        for (flag, name, value) in [
            ("--mcp-config", "provenant-mcp.json", mcp),
            ("--settings", "provenant-settings.json", settings),
        ] {
            let path = scratch.join(name);
            let bytes = serde_json::to_vec(&value).map_err(|_| ProviderError::Malformed)?;
            argv.extend([flag.into(), path.clone().into_os_string()]);
            steps.push(SetupStep::WriteFile { path, bytes });
        }
        // Settings are explicitly sourced from our generated document only.
        argv.extend([
            "--setting-sources".into(),
            "".into(),
            "--allowedTools".into(),
            allowed_tools.into(),
        ]);
        Ok(steps)
    }

    pub(crate) fn codex(&self, argv: &mut Vec<OsString>) -> Result<(), ProviderError> {
        let mcp = self.mcp(Harness::Codex)?;
        for key in ["command", "args", "env"] {
            // JSON values are TOML-compatible except object separators.
            let value = if key == "env" {
                let env = mcp[key].as_object().ok_or(ProviderError::Malformed)?;
                format!(
                    "{{{}}}",
                    env.iter()
                        .map(|(key, value)| { format!("{key}={value}") })
                        .collect::<Vec<_>>()
                        .join(",")
                )
            } else {
                mcp[key].to_string()
            };
            argv.extend([
                "-c".into(),
                format!("mcp_servers.fabric.{key}={value}").into(),
            ]);
        }
        Ok(())
    }

    /// `scrubbed_git_config` is for a provider whose shell subprocesses lose
    /// `GIT_CONFIG_GLOBAL`: `scratch/bin/git` then wraps the selected Git and
    /// sets the variable itself, so Git never opens the unreadable `~/.gitconfig`.
    pub(crate) fn environment(
        &self,
        scratch: &Path,
        env: &mut Vec<(OsString, OsString)>,
        setup: &mut Vec<SetupStep>,
        scrubbed_git_config: bool,
    ) -> Result<(), ProviderError> {
        // A private name also pins ordinary shell `git`, independently of
        // PATH's system shim and of the override executable's basename.
        let bin = scratch.join("bin");
        let (_, path) = env
            .iter_mut()
            .find(|(key, _)| key == "PATH")
            .ok_or(ProviderError::InvalidPath)?;
        *path =
            std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(path)))
                .map_err(|_| ProviderError::InvalidPath)?;
        setup.push(SetupStep::CreateDir(bin.clone()));
        setup.push(if scrubbed_git_config {
            SetupStep::WriteExecutable {
                path: bin.join("git"),
                bytes: self.git_wrapper()?,
            }
        } else {
            SetupStep::Symlink {
                link: bin.join("git"),
                target: self.git.to_owned(),
            }
        });
        env.extend([
            ("PROVENANT_ROOT".into(), self.assets.into()),
            ("PROVENANT_GIT".into(), self.git.into()),
            ("PROVENANT_LANE_DEPTH".into(), self.depth.to_string().into()),
            (
                "PROVENANT_PARENT_ATTEMPT_ID".into(),
                self.attempt.to_string().into(),
            ),
            (RUN_STATE_DIRECTORY_ENV.into(), self.child_state.into()),
        ]);
        Ok(())
    }

    fn git_wrapper(&self) -> Result<Vec<u8>, ProviderError> {
        let git = self.git.to_str().ok_or(ProviderError::InvalidPath)?;
        if git.contains(['\'', '\n']) {
            return Err(ProviderError::InvalidPath);
        }
        Ok(format!(
            "#!/bin/sh\nGIT_CONFIG_GLOBAL=/dev/null\nGIT_CONFIG_NOSYSTEM=1\n\
             export GIT_CONFIG_GLOBAL GIT_CONFIG_NOSYSTEM\nexec '{git}' \"$@\"\n"
        )
        .into_bytes())
    }

    /// Rust Fabric uses stdio and SQLite, so it needs no daemon socket grant.
    /// Keep configuration read-only and writable state confined to its owners.
    pub(crate) fn grant(
        &self,
        profile: &mut ProviderProfileSpec,
        setup: &mut Vec<SetupStep>,
        hooks: bool,
    ) {
        profile
            .process_executables
            .extend([exact(self.executable), exact(self.git)]);
        setup.push(SetupStep::CreateDir(self.state.join("routing")));
        profile.runtime_reads.extend([
            exact(self.executable),
            exact(self.git),
            exact(self.assets.join("release.json")),
            exact(self.state.join("config.toml")),
            exact(self.state.join("host-id")),
        ]);
        // Homebrew Git links pcre2 and gettext from sibling kegs.
        profile.runtime_reads.extend(
            crate::dylib::linked_libraries(self.git)
                .into_iter()
                .map(exact),
        );
        profile.runtime_reads.extend([
            subtree(self.state.join("projects")),
            subtree(self.state.join("routing")),
        ]);
        // The parent pre-creates anchors. Run databases and custody are private
        // to child_state. Shared SQLite still needs writes even for reads:
        // Store::open enables WAL and checks migrations. The lane MCP guard is
        // only a tool-surface guard, not a full boundary against direct writes.
        // TODO(lane-store-isolation): remove shared SQLite and lock write grants
        // after the read-only store follow-up in docs/specs/fabric.md lands.
        setup.push(SetupStep::CreateDir(self.child_state.to_owned()));
        profile.state_writes.push(subtree(self.child_state));
        let locks = self.state.join("fabric/locks");
        setup.push(SetupStep::CreateDir(locks.clone()));
        profile.state_writes.push(subtree(locks));
        for name in [
            "fabric/fabric.sqlite3",
            "fabric/fabric.sqlite3-wal",
            "fabric/fabric.sqlite3-shm",
            "fabric/fabric.sqlite3-journal",
            "host-binding.lock",
        ] {
            profile.state_writes.push(exact(self.state.join(name)));
        }
        if hooks {
            // Claude executes command hooks through its shell. This does not
            // enable the model's Bash tool.
            // macOS /bin/sh re-executes the selected system shell variant.
            profile
                .process_executables
                .extend(["/bin/sh", "/bin/bash", "/bin/zsh", "/bin/dash"].map(exact));
            let path = self.state.join("workflow");
            setup.push(SetupStep::CreateDir(path.clone()));
            profile.state_writes.push(subtree(path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_mcp_entries_have_attempt_unique_agent_labels() -> Result<(), ProviderError> {
        for harness in [Harness::Claude, Harness::Codex] {
            for byte in [1, 2] {
                let lane = LaneRuntime {
                    executable: Path::new("/fixture/provenant"),
                    git: Path::new("/fixture/git"),
                    state: Path::new("/fixture/state"),
                    child_state: Path::new("/fixture/state/children/attempt"),
                    home: Path::new("/fixture/home"),
                    depth: 1,
                    attempt: AttemptId::from_bytes([byte; 16]),
                    assets: Path::new("/fixture/assets"),
                };
                assert_eq!(
                    lane.mcp(harness)?["env"]["PROVENANT_AGENT_LABEL"],
                    format!("lane-{}", lane.attempt)
                );
            }
        }
        Ok(())
    }
}
