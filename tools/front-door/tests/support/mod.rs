//! Isolated process fixtures; no ambient config, credentials, or product state.
#![allow(dead_code)]
use provenant_supervisor::exec::{
    ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor,
};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    tempdir_in(&std::env::temp_dir())
}

fn tempdir_in(parent: &Path) -> std::io::Result<tempfile::TempDir> {
    // Canonical roots avoid macOS temp aliases while honouring the lane's writable TMPDIR.
    tempfile::tempdir_in(fs::canonicalize(parent)?)
}

pub fn register_account(
    rig: &TestRig,
    adapter: &str,
    label: &str,
    key: &str,
    directory: &Path,
) -> Result {
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    let config = rig.state.join("config.toml");
    let mut doc: toml_edit::DocumentMut = if config.exists() {
        fs::read_to_string(&config)?.parse()?
    } else {
        "schema_version = 1\n".parse()?
    };
    let domains = doc["domains"]
        .or_insert(toml_edit::table())
        .as_table_mut()
        .ok_or("domains")?;
    let accounts = domains
        .entry("accounts")
        .or_insert(toml_edit::table())
        .as_table_mut()
        .ok_or("accounts")?;
    let entries = accounts
        .entry("entries")
        .or_insert(toml_edit::table())
        .as_table_mut()
        .ok_or("entries")?;
    let mut row = toml_edit::Table::new();
    row["adapter"] = toml_edit::value(adapter);
    row["label"] = toml_edit::value(label);
    row["key"] = toml_edit::value(key);
    row["directory"] = toml_edit::value(directory.to_str().ok_or("directory")?);
    entries.insert(&format!("{adapter}-{label}"), toml_edit::Item::Table(row));
    fs::write(config, doc.to_string())?;
    Ok(())
}

pub fn clear_accounts(rig: &TestRig) -> Result {
    let config = rig.state.join("config.toml");
    let mut doc: toml_edit::DocumentMut = fs::read_to_string(&config)?.parse()?;
    doc["domains"]["accounts"]["entries"] = toml_edit::table();
    fs::write(config, doc.to_string())?;
    Ok(())
}

pub struct TestRig {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub project: PathBuf,
    pub state: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    executor: SystemExecutor,
}
impl TestRig {
    pub fn new() -> Result<Self> {
        // Outside the checkout: a rig beneath it lets cargo in a confined lane
        // probe find (and fail to read) the workspace manifest and config above.
        Self::in_directory(&std::env::temp_dir())
    }
    pub fn in_directory(parent: &Path) -> Result<Self> {
        Self::with_object_format(parent, "sha1")
    }
    pub fn with_object_format(parent: &Path, format: &str) -> Result<Self> {
        let temp = tempdir_in(parent)?;
        let root = fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let project = root.join("project");
        let state = root.join("state");
        for path in [
            &home,
            &project,
            &root.join("config"),
            &root.join("cache"),
            &root.join("claude"),
            &root.join("codex"),
        ] {
            fs::create_dir_all(path)?;
        }
        let env = [
            ("HOME", home.clone()),
            ("XDG_CONFIG_HOME", root.join("config")),
            ("XDG_CACHE_HOME", root.join("cache")),
            ("XDG_STATE_HOME", root.join("xdg-state")),
            ("CLAUDE_CONFIG_DIR", root.join("claude")),
            ("CODEX_HOME", root.join("codex")),
            ("GIT_CONFIG_GLOBAL", root.join("gitconfig")),
            ("GIT_CONFIG_SYSTEM", root.join("git-system")),
            ("PROVENANT_STATE_DIR", state.clone()),
            ("PROVENANT_ROOT", project.clone()),
            ("TMPDIR", root.clone()),
        ]
        .into_iter()
        .map(|(key, value)| (OsString::from(key), value.into_os_string()))
        .chain(
            [
                ("PATH", "/usr/bin:/bin"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("PROVENANT_AGENT_LABEL", "fixture"),
            ]
            .map(|(k, v)| (k.into(), v.into())),
        )
        .collect();
        let rig = Self {
            _temp: temp,
            root,
            home,
            project,
            state,
            env,
            executor: SystemExecutor::new(8)?,
        };
        for tail in [
            ".agents/sentinel",
            ".local/state/agent-harness/sentinel",
            ".local/state/provenant/sentinel",
        ] {
            let path = rig.home.join(tail);
            fs::create_dir_all(path.parent().ok_or("missing sentinel parent")?)?;
            fs::write(path, b"legacy sentinel\0unchanged")?;
        }
        let git = std::env::var_os("PROVENANT_GIT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
        let output = rig.execute(
            &git,
            &["init", "-q", &format!("--object-format={format}")],
            None,
        )?;
        if output.termination != provenant_supervisor::exec::Termination::Exit(0) {
            return Err(format!(
                "git init failed: {}",
                String::from_utf8_lossy(&output.stderr.bytes)
            )
            .into());
        }
        Ok(rig)
    }
    /// Keep this guard armed until the test's final successful assertion.
    /// The rig root includes the local state, nested child stores and loopback peers.
    /// A separate local guard drops before the rig removes its temporary directory.
    pub fn process_cleanup(&self) -> provenant_supervisor::fixture::ProcessCleanupGuard {
        provenant_supervisor::fixture::ProcessCleanupGuard::new(&self.root)
    }

    pub fn command(&self, args: &[&str]) -> Result<ExecOutcome> {
        let mut arguments = vec![
            OsString::from("--state-dir"),
            self.state.clone().into_os_string(),
        ];
        arguments.extend(args.iter().map(OsString::from));
        // Clippy also compiles the included command edge as a binary unit
        // target, where Cargo supplies no integration-test executable path.
        let binary = option_env!("CARGO_BIN_EXE_provenant")
            .ok_or("production binary path is only available to integration targets")?;
        self.execute_os(Path::new(binary), &arguments, None)
    }
    pub fn execute(
        &self,
        program: &Path,
        args: &[&str],
        stdin: Option<&[u8]>,
    ) -> Result<ExecOutcome> {
        self.execute_os(
            program,
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            stdin,
        )
    }
    pub fn execute_os(
        &self,
        program: &Path,
        args: &[OsString],
        stdin: Option<&[u8]>,
    ) -> Result<ExecOutcome> {
        let mut argv = vec![program.as_os_str().to_owned()];
        argv.extend_from_slice(args);
        let mut options = ExecOptions::new(&argv, &self.project, Duration::from_secs(10));
        options.env = Some(&self.env);
        options.stdin = stdin;
        options.stdout = StreamPolicy::Capture {
            limit: 2 * 1024 * 1024,
        };
        options.stderr = StreamPolicy::Capture { limit: 64 * 1024 };
        let result = self.executor.execute(options)?;
        if result.timed_out || result.drain_truncated {
            return Err("fixture watchdog or capture drain failed".into());
        }
        Ok(result)
    }
    pub fn assert_legacy_unchanged(&self) -> Result {
        for tail in [
            ".agents/sentinel",
            ".local/state/agent-harness/sentinel",
            ".local/state/provenant/sentinel",
        ] {
            assert_eq!(
                fs::read(self.home.join(tail))?,
                b"legacy sentinel\0unchanged"
            );
        }
        Ok(())
    }
}
pub fn code(output: &ExecOutcome) -> i64 {
    match output.termination {
        provenant_supervisor::exec::Termination::Exit(code) => code,
        _ => -1,
    }
}

pub mod mcp;
