//! Isolated process fixtures; no ambient config, credentials, or product state.
#![allow(dead_code)]
use provenant_os::exec::{ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor};
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

pub const FIXTURE_STDOUT_LIMIT: usize = 2 * 1024 * 1024;
pub const FIXTURE_STDERR_LIMIT: usize = 64 * 1024;

static PROCESS_SPAWN_LOCK: Mutex<()> = Mutex::new(());

pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    provenant_os::fixture::tempdir(|root| tempfile::Builder::new().prefix(".tmp").tempdir_in(root))
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
    provenant_os::fixture::private_state_dir(&rig.state)?;
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
/// Launches in a rig create short `TMPDIR` aliases in the shared `/private/tmp`.
/// Settlement unlinks them, but a test that ends mid-attempt would leave them
/// dangling, so the rig unlinks every alias that points inside it.
impl Drop for TestRig {
    fn drop(&mut self) {
        let Ok(entries) = fs::read_dir("/private/tmp") else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ours = entry.file_name().to_string_lossy().starts_with("pv-")
                && fs::read_link(&path).is_ok_and(|target| target.starts_with(&self.root));
            if ours {
                // A symlink: this unlinks the alias, never its target.
                let _ = fs::remove_file(path);
            }
        }
    }
}

impl TestRig {
    /// Serialise process creation with executable copies in this test binary.
    pub fn with_process_spawn_lock<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = PROCESS_SPAWN_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f()
    }

    pub fn copy_executable(&self, source: &Path, destination: &Path) -> Result {
        self.with_process_spawn_lock(|| {
            fs::copy(source, destination)?;
            Ok(())
        })
    }

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
        provenant_os::fixture::private_state_dir(&root)?;
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
        provenant_os::fixture::private_state_dir(&state)?;
        // Writer fixtures must not depend on the host volume's free space.
        fs::write(
            state.join("config.toml"),
            "schema_version=1\n[gc]\nfree_floor='1B'\n",
        )?;
        let git = fs::canonicalize(
            provenant_os::git::git_executable().ok_or("Git executable unavailable")?,
        )?;
        let mut env: Vec<(OsString, OsString)> = [
            ("HOME", home.clone()),
            ("XDG_CONFIG_HOME", root.join("config")),
            ("XDG_CACHE_HOME", root.join("cache")),
            ("XDG_STATE_HOME", root.join("xdg-state")),
            ("CLAUDE_CONFIG_DIR", root.join("claude")),
            ("CODEX_HOME", root.join("codex")),
            ("PROVENANT_GIT", git.clone()),
            ("PROVENANT_STATE_DIR", state.clone()),
            ("PROVENANT_ROOT", project.clone()),
            ("TMPDIR", root.clone()),
        ]
        .into_iter()
        .map(|(key, value)| (OsString::from(key), value.into_os_string()))
        .chain(
            [
                ("PATH", "/usr/bin:/bin"),
                ("PROVENANT_AGENT_LABEL", "fixture"),
                // Disposable state: skip the per-sync storage-device flush.
                ("PROVENANT_DURABILITY", "relaxed"),
                // Debug hooks otherwise miss the 50 ms delivery budget under CI load.
                ("PROVENANT_TEST_ANSWER_BUDGET_MS", "1000"),
            ]
            .map(|(k, v)| (k.into(), v.into())),
        )
        .collect();
        env.extend(provenant_os::fixture::fixture_git_settings());
        env.push((
            provenant_supervisor::FIXTURE_OWNER_PID_ENV.into(),
            std::process::id().to_string().into(),
        ));
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
        let output = rig.execute(
            &git,
            &["init", "-q", &format!("--object-format={format}")],
            None,
        )?;
        if output.termination != provenant_os::exec::Termination::Exit(0) {
            return Err(format!(
                "git init failed: {}",
                String::from_utf8_lossy(&output.stderr.bytes)
            )
            .into());
        }
        Ok(rig)
    }
    /// Exercise commands whose contract requires an absent state root.
    pub fn without_state(self) -> Result<Self> {
        fs::remove_file(self.state.join("config.toml"))?;
        fs::remove_dir(&self.state)?;
        Ok(self)
    }

    /// Hold this guard for the rest of the test's scope; it stays armed until
    /// it drops, then terminates any recorded custodians and dependents.
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
        let _guard = PROCESS_SPAWN_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.execute_os_locked(program, args, stdin)
    }

    /// Run a child while `PROCESS_SPAWN_LOCK` is already held.
    pub fn execute_os_locked(
        &self,
        program: &Path,
        args: &[OsString],
        stdin: Option<&[u8]>,
    ) -> Result<ExecOutcome> {
        let mut argv = vec![program.as_os_str().to_owned()];
        argv.extend_from_slice(args);
        // Only a backstop for a hung child: commands carry their own `--wait`
        // budgets, and nextest bounds the whole test.
        let mut options = ExecOptions::new(&argv, &self.project, Duration::from_secs(60));
        options.env = Some(&self.env);
        options.stdin = stdin;
        options.stdout = StreamPolicy::Capture {
            limit: FIXTURE_STDOUT_LIMIT,
        };
        options.stderr = StreamPolicy::Capture {
            limit: FIXTURE_STDERR_LIMIT,
        };
        let result = self.executor.execute(options)?;
        if result.timed_out || result.drain_truncated {
            return Err("fixture watchdog or capture drain failed".into());
        }
        Ok(result)
    }
    /// `run ARGS --json`: the launch's exit code and envelope. `run` returns
    /// once work is launched or refused; it never waits for it to settle.
    pub fn launch(&self, args: &[&str]) -> Result<(ExecOutcome, serde_json::Value)> {
        let mut argv = vec!["run"];
        argv.extend_from_slice(args);
        argv.push("--json");
        let out = self.command(&argv)?;
        let value = serde_json::from_slice(&out.stdout.bytes)
            .map_err(|error| format!("launch output: {error}: {out:?}"))?;
        Ok((out, value))
    }

    /// `run ARGS` then `wait NAME --timeout SPAN`, the way a caller waits for
    /// the graph its launch named. The result keeps the launch envelope, the
    /// wait's own outcome and the graph's checkpoint as the wait left it.
    pub fn run_and_wait(&self, args: &[&str], timeout: &str) -> Result<Settled> {
        let (launch, launch_value) = self.launch(args)?;
        let graph = launch_value["data"]["checkpoint"]["graph_id"]
            .as_str()
            .ok_or_else(|| format!("launch named no graph: {launch_value}"))?
            .to_owned();
        let wait = self.command(&["wait", &graph, "--timeout", timeout, "--json"])?;
        let wait_value = serde_json::from_slice(&wait.stdout.bytes)
            .map_err(|error| format!("wait output: {error}: {wait:?}"))?;
        let checkpoint = self.checkpoint(&graph)?;
        Ok(Settled {
            launch,
            launch_value,
            wait,
            wait_value,
            graph,
            checkpoint,
        })
    }

    /// A graph's checkpoint as JSON, read from its journal without a lock.
    pub fn checkpoint(&self, graph: &str) -> Result<serde_json::Value> {
        let identity = self.command(&["fabric", "identity", "--json"])?;
        let identity: serde_json::Value = serde_json::from_slice(&identity.stdout.bytes)?;
        let project = identity["data"]["project"]
            .as_str()
            .ok_or("project id")?
            .parse()?;
        let mut inputs = provenant_core::RootInputs::new(self.home.clone(), self.project.clone());
        inputs.state_dir = Some(self.state.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let checkpoint = provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
            &roots.state,
            project,
            graph,
        )?
        .ok_or_else(|| format!("no checkpoint for {graph}"))?;
        Ok(serde_json::to_value(checkpoint)?)
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
        provenant_os::exec::Termination::Exit(code) => code,
        _ => -1,
    }
}

pub mod mcp;

/// A launched graph after `wait` returned: what the launch said, how the wait
/// ended and where the graph stands.
pub struct Settled {
    pub launch: ExecOutcome,
    pub launch_value: serde_json::Value,
    pub wait: ExecOutcome,
    pub wait_value: serde_json::Value,
    pub graph: String,
    pub checkpoint: serde_json::Value,
}
impl Settled {
    /// The wait's exit code: 0 settled successfully, 1 unsuccessfully, 5 needs
    /// input, 124 the budget expired.
    pub fn exit(&self) -> i64 {
        code(&self.wait)
    }
    /// Whether the wait ran out of time with the work unsettled.
    pub fn timed_out(&self) -> bool {
        self.wait_value["data"]["timed_out"] == true
    }
}
