//! Provenant-only lane customisation; no user configuration is loaded.
use crate::launch::{exact, subtree};
use crate::{ProviderError, SetupStep};
use provenant_confinement::permissions::ProviderProfileSpec;
use provenant_records::Harness;
use serde_json::Value;
use std::{ffi::OsString, path::Path};

/// Canonical runtime paths supplied by the composition edge.
pub struct LaneRuntime<'a> {
    pub executable: &'a Path,
    pub state: &'a Path,
    pub assets: &'a Path,
}

impl LaneRuntime<'_> {
    pub(crate) fn mcp(&self, harness: Harness) -> Result<Value, ProviderError> {
        if ![self.executable, self.state, self.assets]
            .iter()
            .all(|p| p.is_absolute())
        {
            return Err(ProviderError::InvalidPath);
        }
        provenant_config::mcp_entry(harness, self.executable, self.state)
            .map_err(|_| ProviderError::UnsupportedPolicy)
    }

    pub(crate) fn claude(
        &self,
        scratch: &Path,
        argv: &mut Vec<OsString>,
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
            "mcp__fabric__*".into(),
        ]);
        Ok(steps)
    }

    pub(crate) fn codex(&self, argv: &mut Vec<OsString>) -> Result<(), ProviderError> {
        let mcp = self.mcp(Harness::Codex)?;
        for key in ["command", "args", "env"] {
            // JSON string/array spellings are valid TOML for these values;
            // the generator's empty env is an empty inline table in TOML.
            let value = if key == "env" {
                "{}".to_owned()
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

    pub(crate) fn environment(&self, env: &mut Vec<(OsString, OsString)>) {
        env.push(("PROVENANT_ROOT".into(), self.assets.into()));
    }

    /// Rust Fabric uses stdio and SQLite, so it needs no daemon socket grant.
    /// Keep configuration read-only and writable state confined to its owners.
    pub(crate) fn grant(
        &self,
        profile: &mut ProviderProfileSpec,
        setup: &mut Vec<SetupStep>,
        hooks: bool,
    ) {
        profile.process_executables.push(exact(self.executable));
        profile.runtime_reads.extend([
            exact(self.executable),
            exact(self.assets.join("release.json")),
            exact(self.state.join("config.toml")),
            exact(self.state.join("host-id")),
        ]);
        for name in ["fabric", "projects", "runs", "tmp/git-home", "tmp/results"] {
            let path = self.state.join(name);
            setup.push(SetupStep::CreateDir(path.clone()));
            profile.state_writes.push(subtree(path));
        }
        for name in [
            "run.sqlite3",
            "run.sqlite3-wal",
            "run.sqlite3-shm",
            "run.sqlite3-journal",
            "host-binding.lock",
        ] {
            profile.state_writes.push(exact(self.state.join(name)));
        }
        if hooks {
            // Claude executes command hooks through its shell. This does not
            // enable the model's Bash tool.
            profile
                .process_executables
                .extend([exact("/bin/sh"), exact("/usr/bin/git")]);
            let path = self.state.join("workflow");
            setup.push(SetupStep::CreateDir(path.clone()));
            profile.state_writes.push(subtree(path));
        }
    }
}
