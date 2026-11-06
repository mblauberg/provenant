//! Provenant-only provider customisation and writer publishing setup.
use crate::launch::{exact, subtree};
use crate::{ProviderError, SetupStep};
use provenant_confinement::permissions::ProviderProfileSpec;
use provenant_core::{AttemptId, RUN_STATE_DIRECTORY_ENV, STATE_DIRECTORY_ENV};
use provenant_records::Harness;
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub(crate) struct GitWrapper {
    pub(crate) scrub: bool,
    pub(crate) identity_config: Option<PathBuf>,
    include: Option<String>,
    git: Option<PathBuf>,
    pub(crate) bound: Option<BoundGit>,
}

/// The writer's private repository binding. It is never exported into the
/// lane's ambient environment: a `git` run in an unrelated directory (a test
/// fixture's temporary repository) must act on that directory. The wrapper
/// applies it only to Git run in `worktree`.
pub(crate) struct BoundGit {
    pub(crate) worktree: PathBuf,
    /// Git selectors and the lane's commit identity.
    pub(crate) env: Vec<(OsString, OsString)>,
}

impl crate::LaunchSpec {
    pub(crate) fn write_git_wrapper(
        &mut self,
        bin: &Path,
        git: &Path,
    ) -> Result<(), ProviderError> {
        if self
            .git_wrapper
            .git
            .as_deref()
            .is_some_and(|selected| selected != git)
        {
            return Err(ProviderError::InvalidPath);
        }
        self.git_wrapper.git = Some(git.to_owned());
        install_git(
            bin,
            git,
            &mut self.env,
            &mut self.setup,
            &self.git_wrapper,
            true,
        )
    }
    /// Restore selected environment before the provider starts a shell command.
    pub fn configure_shell(&mut self, scratch: &Path, adapter: &provenant_records::AdapterSpec) {
        let Some((relative, selector)) = adapter.shell_prefix else {
            return;
        };
        let path = scratch.join(relative);
        self.setup.push(SetupStep::CreateDir(scratch.join("bin")));
        self.setup.push(SetupStep::WriteExecutable {
            path: path.clone(),
            bytes: claude_shell(&self.env),
        });
        self.env.push((selector.into(), path.into_os_string()));
    }
    /// Put a `cargo` shim first on the lane's PATH. Cargo commands that compile
    /// take one host build slot (`slots` across every project on this state
    /// directory) before the real `cargo` runs, and keep it until it exits.
    /// Waiting blocks only that command.
    ///
    /// The host provisions the slot files (`BuildSlots::provision`) before
    /// the lane starts. The lane is granted read access to that directory and
    /// nothing more, so it can neither create, unlink nor replace a slot file;
    /// the shim opens a slot read-only and locks it. The directory and count
    /// are written into the shim script as environment for the one command,
    /// which overrides anything the lane's own environment says.
    pub fn configure_build_slots(
        &mut self,
        scratch: &Path,
        gate: &BuildSlotGate<'_>,
    ) -> Result<(), ProviderError> {
        let text = |path: &Path| -> Result<String, ProviderError> {
            let value = path.to_str().ok_or(ProviderError::InvalidPath)?;
            if !path.is_absolute() || value.chars().any(char::is_control) {
                return Err(ProviderError::InvalidPath);
            }
            Ok(shell_quote(value))
        };
        if !(1..=64).contains(&gate.slots) || gate.wait_seconds == 0 {
            return Err(ProviderError::Malformed);
        }
        let bin = scratch.join("bin");
        let script = format!(
            "#!/bin/sh\nPROVENANT_BUILD_SLOT_STATE={} PROVENANT_BUILD_SLOTS={} PROVENANT_BUILD_WAIT_SECS={} PROVENANT_BUILD_WAIT_MARKER={}\nexport PROVENANT_BUILD_SLOT_STATE PROVENANT_BUILD_SLOTS PROVENANT_BUILD_WAIT_SECS PROVENANT_BUILD_WAIT_MARKER\nexec {} internal build-slot -- {} \"$@\"\n",
            text(gate.state)?,
            gate.slots,
            gate.wait_seconds,
            text(gate.wait_marker)?,
            text(gate.provenant)?,
            text(gate.cargo)?,
        );
        // Validate the environment before mutating setup or grants. The caller
        // can then omit this optional shim if its paths cannot be represented.
        let path_index = self
            .env
            .iter()
            .position(|(key, _)| key == "PATH")
            .ok_or(ProviderError::InvalidPath)?;
        let path = &self.env[path_index].1;
        let next_path = if std::env::split_paths(path).next().as_deref() == Some(bin.as_path()) {
            path.clone()
        } else {
            std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(path)))
                .map_err(|_| ProviderError::InvalidPath)?
        };
        let slots = gate
            .state
            .join(provenant_os::build_slots::DIRECTORY.join("/"));
        let create = SetupStep::CreateDir(bin.clone());
        if !self.setup.contains(&create) {
            self.setup.push(create);
        }
        self.setup.push(SetupStep::WriteExecutable {
            path: bin.join("cargo"),
            bytes: script.into_bytes(),
        });
        self.env[path_index].1 = next_path;
        let profile = &mut self.profile.spec;
        // Read only: no write grant covers the slot directory.
        profile.runtime_reads.push(subtree(slots));
        profile
            .process_executables
            .extend([exact("/bin/sh"), exact(gate.provenant)]);
        profile.runtime_reads.push(exact(gate.provenant));
        profile.tool_exec_roots.push(subtree(bin));
        Ok(())
    }
    /// Configure shell Git after adapter preparation, including shells that
    /// scrub Git environment variables. No credential is read during setup.
    pub fn configure_publishing(
        &mut self,
        scratch: &Path,
        git: &Path,
        gh: &Path,
        home: &Path,
        policy: &provenant_config::GithubPolicy,
    ) -> Result<(), ProviderError> {
        let text = |path: &Path| -> Result<String, ProviderError> {
            if !path.is_absolute() {
                return Err(ProviderError::InvalidPath);
            }
            let value = path.to_str().ok_or(ProviderError::InvalidPath)?;
            if value.chars().any(char::is_control) {
                return Err(ProviderError::InvalidPath);
            }
            Ok(value.to_owned())
        };
        let git_path = text(git)?;
        let gh_path = text(gh)?;
        let gh_config = shell_quote(&text(&home.join(".config/gh"))?);
        let gh = format!("GH_CONFIG_DIR={gh_config} {}", shell_quote(&gh_path));
        let config = scratch.join("publishing.gitconfig");
        let config_text = format!(
            "[credential]\n\thelper =\n\thelper = {}\n\tinteractive = false\n",
            serde_json::to_string(&format!("!{gh} auth git-credential"))
                .map_err(|_| ProviderError::Malformed)?,
        );
        let include = shell_quote(&format!("include.path={}", text(&config)?));
        self.git_wrapper.scrub = true;
        self.git_wrapper.include = Some(include);
        self.write_git_wrapper(&scratch.join("bin"), git)?;
        self.setup.extend([
            SetupStep::WriteFile {
                path: config,
                bytes: config_text.into_bytes(),
            },
            SetupStep::WriteExecutable {
                path: scratch.join("bin/gh"),
                bytes: gh_wrapper(&gh_path, &gh_config, policy).into_bytes(),
            },
        ]);
        // Cursor and Agy do not use LaneRuntime::grant for Git.
        self.profile
            .spec
            .process_executables
            .push(exact(git_path.as_str()));
        self.profile
            .spec
            .runtime_reads
            .push(exact(git_path.as_str()));
        if let Some(prefix) = Path::new(&git_path).parent().and_then(Path::parent) {
            let core = prefix.join("libexec/git-core");
            if core.is_dir() {
                self.profile.spec.runtime_reads.push(subtree(&core));
                self.profile.spec.tool_exec_roots.push(subtree(core));
            }
        }
        self.profile
            .spec
            .process_executables
            .push(exact(Path::new(&gh_path)));
        self.profile
            .spec
            .runtime_reads
            .push(exact(Path::new(&gh_path)));
        Ok(())
    }
}

/// The helper directory the host's `git` reports for itself. Apple's
/// `/usr/bin/git` is a shim that finds it through the active developer
/// directory, which a confined lane cannot consult, so the launcher pins it.
/// `None` when Git cannot say, in which case Git resolves it as before.
pub(crate) fn host_exec_path(git: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let output = std::process::Command::new(git)
        .arg("--exec-path")
        .env_remove("GIT_EXEC_PATH")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    let path = Path::new(std::ffi::OsStr::from_bytes(text));
    (output.status.success() && path.is_absolute() && path.is_dir() && !text.contains(&0))
        .then(|| path.to_owned())
}

/// Git configuration that answers a GitHub HTTPS credential request from the
/// lane's own `GH_TOKEN`, read by the helper at run time. The value is never
/// written to the launcher or to any argument.
const TOKEN_HELPER: &str = "!f() { test \"$1\" = get && test -n \"$GH_TOKEN\" && printf 'username=x-access-token\\npassword=%s\\n' \"$GH_TOKEN\"; }; f";

pub(crate) fn git_wrapper(git: &Path, options: &GitWrapper) -> Result<Vec<u8>, ProviderError> {
    use std::os::unix::ffi::OsStrExt;
    let git = git.as_os_str().as_bytes();
    if git.contains(&0) {
        return Err(ProviderError::InvalidPath);
    }
    let mut bytes = b"#!/bin/sh\n".to_vec();
    if options.scrub {
        let global = if let Some(config) = &options.identity_config {
            let config = config.to_str().ok_or(ProviderError::InvalidPath)?;
            if config.chars().any(char::is_control) {
                return Err(ProviderError::InvalidPath);
            }
            shell_quote(config)
        } else {
            "'/dev/null'".to_owned()
        };
        bytes.extend_from_slice(
            format!("GIT_CONFIG_GLOBAL={global}\nexport GIT_CONFIG_GLOBAL\n").as_bytes(),
        );
        if options.identity_config.is_none() {
            bytes.extend_from_slice(b"GIT_CONFIG_NOSYSTEM=1\nexport GIT_CONFIG_NOSYSTEM\n");
        }
        if let Some(bound) = &options.bound {
            bytes.extend_from_slice(bound_exports(bound)?.as_bytes());
        }
    }
    // Environment configuration keeps the arguments the lane passed untouched.
    // A host with no `gh` login yields no answer and Git falls through.
    bytes.extend_from_slice(
        format!(
            "GIT_CONFIG_COUNT=1\nGIT_CONFIG_KEY_0=credential.https://github.com.helper\nGIT_CONFIG_VALUE_0={}\nexport GIT_CONFIG_COUNT GIT_CONFIG_KEY_0 GIT_CONFIG_VALUE_0\n",
            shell_quote(TOKEN_HELPER)
        )
        .as_bytes(),
    );
    if let Some(exec_path) = host_exec_path(Path::new(std::ffi::OsStr::from_bytes(git))) {
        let exec_path = exec_path.to_str().ok_or(ProviderError::InvalidPath)?;
        bytes.extend_from_slice(
            format!(
                "GIT_EXEC_PATH={}\nexport GIT_EXEC_PATH\n",
                shell_quote(exec_path)
            )
            .as_bytes(),
        );
    }
    bytes.extend_from_slice(b"exec '");
    for byte in git {
        if *byte == b'\'' {
            bytes.extend_from_slice(b"'\\''");
        } else {
            bytes.push(*byte);
        }
    }
    bytes.push(b'\'');
    let include = options
        .include
        .as_deref()
        .map(|value| format!(" -c {value}"))
        .unwrap_or_default();

    bytes.extend_from_slice(format!("{include} \"$@\"\n").as_bytes());
    Ok(bytes)
}

// Both adapter setup and the writer/publishing composition use the same
// executable selection, PATH and configuration policy.
fn install_git(
    bin: &Path,
    git: &Path,
    env: &mut Vec<(OsString, OsString)>,
    setup: &mut Vec<SetupStep>,
    options: &GitWrapper,
    wrapper: bool,
) -> Result<(), ProviderError> {
    let path = bin.join("git");
    let step = if wrapper {
        SetupStep::WriteExecutable {
            path: path.clone(),
            bytes: git_wrapper(git, options)?,
        }
    } else {
        SetupStep::Symlink {
            link: path.clone(),
            target: git.to_owned(),
        }
    };
    setup.retain(|step| !matches!(step, SetupStep::WriteExecutable { path: existing, .. } | SetupStep::Symlink { link: existing, .. } if existing == &path));
    if !setup.contains(&SetupStep::CreateDir(bin.to_owned())) {
        setup.push(SetupStep::CreateDir(bin.to_owned()));
    }
    setup.push(step);
    let index = match env.iter().position(|(key, _)| key == "PATH") {
        Some(index) => index,
        None => {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: lane PATH is missing; using system defaults"
            );
            env.push(("PATH".into(), "/usr/bin:/bin".into()));
            env.len() - 1
        }
    };
    let search_path = &mut env[index].1;
    if std::env::split_paths(&*search_path).next().as_deref() != Some(bin) {
        match std::env::join_paths(
            std::iter::once(bin.to_owned()).chain(std::env::split_paths(&*search_path)),
        ) {
            Ok(value) => *search_path = value,
            Err(_) if options.bound.is_none() && options.include.is_none() => {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: the Git launcher cannot be included in PATH; continuing without it"
                );
            }
            Err(_) => return Err(ProviderError::InvalidPath),
        }
    }
    Ok(())
}

fn gh_wrapper(gh: &str, gh_config: &str, policy: &provenant_config::GithubPolicy) -> String {
    let mut guard = String::new();
    for verb in &policy.deny_verbs {
        let words: Vec<_> = verb.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        let conditions = words
            .iter()
            .enumerate()
            .map(|(i, word)| format!("[ \"${{{}}}\" = {} ]", i + 1, shell_quote(word)))
            .collect::<Vec<_>>()
            .join(" && ");
        guard.push_str(&format!("if [ \"$#\" -ge {} ] && {conditions}; then\n  printf '%s\\n' 'github.deny_verbs' >&2; exit 1\nfi\n", words.len()));
    }
    if !policy.deny_labels.is_empty() {
        guard.push_str("check_labels() {\n  short=false\n  case \"$1 $2\" in\n    'pr create'|'issue create') flag=--label; short=true ;;\n    'pr edit'|'issue edit') flag=--add-label ;;\n    *) return ;;\n  esac\n  shift 2\n  while [ \"$#\" -gt 0 ]; do\n    case \"$1\" in\n      --) break ;;\n      \"$flag\") shift; [ \"$#\" -gt 0 ] || break; value=$1 ;;\n      \"$flag\"=*) value=${1#*=} ;;\n      -l) if $short; then shift; [ \"$#\" -gt 0 ] || break; value=$1; else shift; continue; fi ;;\n      -l=*) if $short; then value=${1#-l=}; else shift; continue; fi ;;\n      -l?*) if $short; then value=${1#-l}; else shift; continue; fi ;;\n      *) shift; continue ;;\n    esac\n    while :; do\n      item=${value%%,*}\n      item=${item#\"${item%%[![:space:]]*}\"}\n      item=${item%\"${item##*[![:space:]]}\"}\n");
        for label in &policy.deny_labels {
            // Quoting the literal keeps shell metacharacters in labels inert.
            let label = label.trim();
            if !label.is_empty() {
                guard.push_str(&format!("      if [ \"$item\" = {} ]; then printf '%s\\n' 'github.deny_labels' >&2; exit 1; fi\n", shell_quote(label)));
            }
        }
        guard.push_str("      case \"$value\" in *,*) value=${value#*,} ;; *) break ;; esac\n    done\n    shift\n  done\n}\ncheck_labels \"$@\"\n");
    }
    format!(
        "#!/bin/sh\n{guard}GH_CONFIG_DIR={gh_config}\nexport GH_CONFIG_DIR\nexec {} \"$@\"\n",
        shell_quote(gh)
    )
}

pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// What the `cargo` shim needs to take a host build slot.
pub struct BuildSlotGate<'a> {
    /// The `provenant` executable that runs `internal build-slot`.
    pub provenant: &'a Path,
    /// Host state directory, shared by every project, holding the slot files.
    pub state: &'a Path,
    /// How many builds the host runs at once.
    pub slots: u64,
    /// Seconds a build waits for a slot or a Cargo lock before giving up.
    pub wait_seconds: u64,
    /// The real `cargo` the shim runs once it holds a slot.
    pub cargo: &'a Path,
    /// File in the lane's private temporary directory where a waiting build
    /// says so, for the host's `lanes` and `status`.
    pub wait_marker: &'a Path,
}

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
    /// Short, attempt-specific path, including for deeply nested child stores.
    pub fn fabric_socket(attempt: AttemptId) -> PathBuf {
        let root = if cfg!(target_os = "macos") {
            "/private/tmp"
        } else {
            "/tmp"
        };
        Path::new(root)
            .join(format!("provenant-fabric-{attempt}"))
            .join("relay.sock")
    }

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
        .all(|p| p.is_absolute() && p.to_str().is_some())
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
            STATE_DIRECTORY_ENV: self.state,
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
        image_rules: &[String],
    ) -> Result<Vec<SetupStep>, ProviderError> {
        let mcp = match self.mcp(Harness::Claude) {
            Ok(entry) => serde_json::json!({"mcpServers": {"fabric": entry}}),
            Err(error) => {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: Fabric MCP configuration is unavailable ({error}); continuing without Fabric MCP"
                );
                serde_json::json!({"mcpServers": {}})
            }
        };
        let mut settings = provenant_config::claude_lane_settings(self.executable, self.state)
            .map_err(|_| ProviderError::UnsupportedPolicy)?;
        settings["attribution"] = serde_json::json!({"commit": "", "pr": ""});
        if !image_rules.is_empty() {
            settings["permissions"]["allow"] = serde_json::json!(image_rules);
        }
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
        let mcp = match self.mcp(Harness::Codex) {
            Ok(entry) => entry,
            Err(error) => {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: Fabric MCP configuration is unavailable ({error}); continuing without Fabric MCP"
                );
                return Ok(());
            }
        };
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
        // Pin the CLI's context in its launcher too: providers may replace
        // HOME or scrub the shell environment. Children still use the host's
        // configuration; writer Git identity is installed into a lane-private config
        // after adapter preparation, with custody in the private Run store.
        let entry = self.mcp(Harness::Codex)?;
        let mut script = String::from("#!/bin/sh\n");
        for (key, value) in entry["env"].as_object().ok_or(ProviderError::Malformed)? {
            let value = value.as_str().ok_or(ProviderError::InvalidPath)?;
            if value.chars().any(char::is_control) {
                return Err(ProviderError::InvalidPath);
            }
            script.push_str(&format!("export {key}={}\n", shell_quote(value)));
        }
        let executable = self.executable.to_str().ok_or(ProviderError::InvalidPath)?;
        if executable.chars().any(char::is_control) {
            return Err(ProviderError::InvalidPath);
        }
        script.push_str(&format!("exec {} \"$@\"\n", shell_quote(executable)));
        // A private name also pins ordinary shell `git`, independently of
        // PATH's system shim and of the override executable's basename.
        install_git(
            &scratch.join("bin"),
            self.git,
            env,
            setup,
            &GitWrapper {
                scrub: scrubbed_git_config,
                ..Default::default()
            },
            scrubbed_git_config,
        )?;
        setup.push(SetupStep::WriteExecutable {
            path: scratch.join("bin/provenant"),
            bytes: script.into_bytes(),
        });
        env.extend([
            (
                "PROVENANT_AGENT_LABEL".into(),
                format!("lane-{}", self.attempt).into(),
            ),
            ("PROVENANT_ROOT".into(), self.assets.into()),
            ("PROVENANT_GIT".into(), self.git.into()),
            (STATE_DIRECTORY_ENV.into(), self.state.into()),
            ("PROVENANT_LANE_DEPTH".into(), self.depth.to_string().into()),
            (
                "PROVENANT_PARENT_ATTEMPT_ID".into(),
                self.attempt.to_string().into(),
            ),
            (RUN_STATE_DIRECTORY_ENV.into(), self.child_state.into()),
        ]);
        Ok(())
    }

    /// Fabric reads WAL without writes; mutations use the attempt's host socket.
    pub(crate) fn grant(
        &self,
        profile: &mut ProviderProfileSpec,
        setup: &mut Vec<SetupStep>,
        hooks: bool,
    ) {
        profile
            .process_executables
            .extend([exact(self.executable), exact(self.git)]);
        profile.runtime_reads.extend([
            exact(self.executable),
            exact(self.git),
            exact(self.assets.join("release.json")),
            exact(self.state.join("config.toml")),
            exact(self.state.join("host-id")),
        ]);
        // Homebrew Git links pcre2 and gettext from sibling kegs.
        profile
            .runtime_reads
            .push(subtree(self.state.join("projects")));
        // Run databases and custody are private to child_state. Explicit exact
        // reads include WAL companions; never grant the Fabric directory.
        setup.push(SetupStep::CreateDir(self.child_state.to_owned()));
        profile.state_writes.push(subtree(self.child_state));
        for name in [
            "fabric/fabric.sqlite3",
            "fabric/fabric.sqlite3-wal",
            "fabric/fabric.sqlite3-shm",
            "fabric/fabric.sqlite3-journal",
        ] {
            profile.runtime_reads.push(exact(self.state.join(name)));
        }
        profile.fabric_socket = Some(exact(Self::fabric_socket(self.attempt)));
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

fn claude_shell(env: &[(OsString, OsString)]) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut script = b"#!/bin/sh\n".to_vec();
    for name in [
        "CARGO_BUILD_JOBS",
        "CARGO_INCREMENTAL",
        "CARGO_PROFILE_DEV_DEBUG",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
    ] {
        if let Some((_, value)) = env.iter().find(|(key, _)| key == name) {
            script.extend_from_slice(format!("export {name}='").as_bytes());
            for byte in value.as_os_str().as_bytes() {
                if *byte == b'\'' {
                    script.extend_from_slice(b"'\\''");
                } else {
                    script.push(*byte);
                }
            }
            script.extend_from_slice(b"'\n");
        }
    }
    script.extend_from_slice(b"exec /bin/bash -c \"$1\"\n");
    script
}

/// Shell that applies the private binding only when Git acts on the lane
/// checkout itself, and otherwise clears every inherited binding. Git acts on
/// the checkout when its physical working directory (moved by any leading
/// `-C`) lies inside the worktree and the nearest `.git` entry walking up from
/// there is the worktree's own marker. `init` and `clone` create a repository,
/// so they never bind. The leading global options are Git's own list (`git help
/// git`); an option the parser does not know, or a missing value, means it
/// cannot tell where the subcommand starts, so it does not bind.
fn bound_exports(bound: &BoundGit) -> Result<String, ProviderError> {
    let worktree = bound.worktree.to_str().ok_or(ProviderError::InvalidPath)?;
    if worktree.chars().any(char::is_control) {
        return Err(ProviderError::InvalidPath);
    }
    let mut exports = String::new();
    let mut names = Vec::new();
    for (key, value) in &bound.env {
        let value = value.to_str().ok_or(ProviderError::InvalidPath)?;
        let key = key.to_str().ok_or(ProviderError::InvalidPath)?;
        names.push(key);
        exports.push_str(&format!("  export {key}={}\n", shell_quote(value)));
    }
    // Whatever the lane does not bind is cleared in both branches, so an
    // inherited index or identity cannot redirect Git inside the checkout.
    let others: Vec<&str> = UNBOUND
        .into_iter()
        .filter(|name| !names.contains(name))
        .collect();
    let others = others.join(" ");
    names.extend(others.split(' ').filter(|name| !name.is_empty()));
    Ok(format!(
        concat!(
            "lane_worktree={}\n",
            "lane_git_bound() {{\n",
            "  lane_d=$(pwd -P) || return 1\n",
            "  while [ \"$#\" -gt 0 ]; do\n",
            "    case \"$1\" in\n",
            "      -C) [ \"$#\" -ge 2 ] || break; lane_d=$(cd \"$lane_d\" 2>/dev/null && cd \"$2\" 2>/dev/null && pwd -P) || return 1; shift 2 ;;\n",
            "      -c|--git-dir|--work-tree|--namespace|--config-env|--attr-source|--super-prefix) [ \"$#\" -ge 2 ] || return 1; shift 2 ;;\n",
            "      --git-dir=*|--work-tree=*|--namespace=*|--config-env=*|--attr-source=*|--super-prefix=*|--exec-path|--exec-path=*|--list-cmds=*) shift ;;\n",
            "      -v|--version|-h|--help|--html-path|--man-path|--info-path|-p|--paginate|-P|--no-pager) shift ;;\n",
            "      --no-replace-objects|--no-lazy-fetch|--no-optional-locks|--no-advice|--bare) shift ;;\n",
            "      --literal-pathspecs|--glob-pathspecs|--noglob-pathspecs|--icase-pathspecs) shift ;;\n",
            "      -*) return 1 ;;\n",
            "      *) break ;;\n",
            "    esac\n",
            "  done\n",
            "  case \"$1\" in init|clone) return 1 ;; esac\n",
            "  case \"$lane_d\" in \"$lane_worktree\"|\"$lane_worktree\"/*) ;; *) return 1 ;; esac\n",
            "  while [ \"$lane_d\" != \"$lane_worktree\" ]; do\n",
            "    if [ -e \"$lane_d/.git\" ] || [ -L \"$lane_d/.git\" ]; then return 1; fi\n",
            "    lane_d=${{lane_d%/*}}\n",
            "  done\n",
            "}}\n",
            "if lane_git_bound \"$@\"; then\n",
            "  unset {}\n",
            "{}",
            "else\n",
            "  unset {}\n",
            "fi\n",
        ),
        shell_quote(worktree),
        others,
        exports,
        names.join(" "),
    ))
}

/// Selectors and identity a non-lane Git must not inherit.
const UNBOUND: [&str; 9] = [
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_INDEX_FILE",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_pins_the_hosts_exec_path_and_answers_credentials_from_the_token() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let core = dir.path().join("dev/libexec/git-core");
        std::fs::create_dir_all(&core).unwrap();
        let git = dir.path().join("git");
        std::fs::write(&git, format!("#!/bin/sh\necho '{}'\n", core.display())).unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let text = String::from_utf8(git_wrapper(&git, &GitWrapper::default()).unwrap()).unwrap();
        assert!(text.contains(&format!("GIT_EXEC_PATH='{}'", core.display())));
        assert!(text.contains("GIT_CONFIG_KEY_0=credential.https://github.com.helper"));
        assert!(!text.contains("secret"));
        let missing = git_wrapper(Path::new("/fixture/git"), &GitWrapper::default()).unwrap();
        assert!(
            !String::from_utf8(missing)
                .unwrap()
                .contains("GIT_EXEC_PATH")
        );
    }

    #[test]
    fn unencodable_git_path_warns_but_bound_writer_still_refuses() {
        let bin = Path::new("/fixture/scratch:other/bin");
        for scrub in [false, true] {
            let mut env = vec![("PATH".into(), "/usr/bin:/bin".into())];
            let mut setup = Vec::new();
            let options = GitWrapper {
                scrub,
                ..Default::default()
            };
            install_git(
                bin,
                Path::new("/fixture/git"),
                &mut env,
                &mut setup,
                &options,
                scrub,
            )
            .unwrap();
            assert_eq!(env[0].1, "/usr/bin:/bin");
        }
        let mut env = vec![("PATH".into(), "/usr/bin:/bin".into())];
        let mut setup = Vec::new();
        let options = GitWrapper {
            scrub: true,
            bound: Some(BoundGit {
                worktree: "/fixture/repo".into(),
                env: vec![],
            }),
            ..Default::default()
        };
        assert!(matches!(
            install_git(
                bin,
                Path::new("/fixture/git"),
                &mut env,
                &mut setup,
                &options,
                true
            ),
            Err(ProviderError::InvalidPath)
        ));
    }

    #[test]
    fn missing_search_path_uses_git_wrapper_and_system_defaults() {
        let mut env = Vec::new();
        let mut setup = Vec::new();
        let bin = Path::new("/fixture/scratch/bin");
        install_git(
            bin,
            Path::new("/fixture/git"),
            &mut env,
            &mut setup,
            &GitWrapper::default(),
            false,
        )
        .unwrap();
        let path = env.iter().find(|(key, _)| key == "PATH").unwrap();
        assert_eq!(
            std::env::split_paths(&path.1).collect::<Vec<_>>(),
            vec![
                bin.to_owned(),
                Path::new("/usr/bin").to_owned(),
                Path::new("/bin").to_owned()
            ]
        );
        assert!(setup.contains(&SetupStep::Symlink {
            link: bin.join("git"),
            target: "/fixture/git".into()
        }));
    }

    #[allow(clippy::disallowed_methods)] // Exercise real subprocess environment clearing.
    fn adapter_private_git(adapter: &str) -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
        use std::{
            fs,
            io::Write,
            os::unix::fs::PermissionsExt,
            process::{Command, Stdio},
        };

        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let repo = root.join("repo");
        let scratch = root.join("scratch");
        let home = root.join("home");
        fs::create_dir_all(&repo)?;
        fs::create_dir_all(&scratch)?;
        fs::create_dir_all(home.join(".config/gh"))?;
        let real_git = provenant_os::git::git_executable()
            .ok_or("Git unavailable")?
            .canonicalize()?;
        let init = |path: &Path, bare: bool| -> Result<(), Box<dyn std::error::Error>> {
            let mut command = Command::new(&real_git);
            command.arg("init");
            if bare {
                command.arg("--bare");
            }
            assert!(
                command
                    .arg(path)
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .output()?
                    .status
                    .success()
            );
            Ok(())
        };
        init(&repo, false)?;
        init(&scratch.join("git-common"), true)?;
        fs::create_dir_all(scratch.join("git-objects"))?;
        let host_head = fs::read(repo.join(".git/HEAD"))?;
        let host_config = fs::read(repo.join(".git/config"))?;
        fs::write(
            home.join(".gitconfig"),
            "[user]\n name = Host Name\n email = host@example.com\n[url \"https://rewritten.example/\"]\n insteadOf = fixture://\n",
        )?;
        let marker = root.join("pinned-called");
        let git = root.join("selected-git");
        fs::write(
            &git,
            format!(
                "#!/bin/sh\nif [ -z \"$GIT_CONFIG_GLOBAL\" ]; then GIT_CONFIG_GLOBAL={}; export GIT_CONFIG_GLOBAL; fi\nprintf called > {}\nexec {} \"$@\"\n",
                shell_quote(home.join(".gitconfig").to_str().ok_or("global config")?),
                shell_quote(marker.to_str().ok_or("marker")?),
                shell_quote(real_git.to_str().ok_or("git")?)
            ),
        )?;
        fs::set_permissions(&git, fs::Permissions::from_mode(0o700))?;
        let gh = root.join("gh");
        fs::write(
            &gh,
            "#!/bin/sh\nprintf 'username=fixture\\npassword=fixture-secret\\n'\n",
        )?;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700))?;
        let registration = crate::REGISTRY
            .iter()
            .find(|entry| entry.adapter.id() == adapter)
            .ok_or("adapter")?;
        let base = crate::tests::fixture_access_plan(
            crate::Access::WorktreeWrite,
            provenant_records::AdapterSpec::get(adapter).and_then(|s| s.authentication),
            false,
        );
        let mut permissions = base.permissions().clone();
        permissions.scratch.path = scratch.clone();
        let plan = ConfinedPlan::new(PlanRequest {
            scope: base.scope(),
            permissions,
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        })?;
        let mut route = crate::tests::route(adapter, "fixture-model");
        route.account_key = None;
        let prompt = crate::Prompt::new("fixture".into());
        let token = crate::tests::fixture_token();
        let mut input = crate::tests::fixture_input(
            &route,
            &plan,
            &prompt,
            &repo,
            (adapter == "claude").then_some(&token),
        );
        input.provenant.git = &git;
        input.home = &home;
        let mut spec = registration.adapter.prepare(input)?;
        let bound = &mut spec.git_wrapper.bound.as_mut().ok_or("bound")?.env;
        bound.retain(|(key, _)| {
            ![
                "GIT_AUTHOR_NAME",
                "GIT_AUTHOR_EMAIL",
                "GIT_COMMITTER_NAME",
                "GIT_COMMITTER_EMAIL",
            ]
            .iter()
            .any(|name| key == name)
        });
        bound.extend([
            ("GIT_AUTHOR_NAME".into(), "Lane Author".into()),
            ("GIT_AUTHOR_EMAIL".into(), "lane@example.com".into()),
            ("GIT_COMMITTER_NAME".into(), "Lane Committer".into()),
            ("GIT_COMMITTER_EMAIL".into(), "lane@example.com".into()),
        ]);
        spec.configure_publishing(&scratch, &git, &gh, &home, &Default::default())?;
        for step in &spec.setup {
            match step {
                SetupStep::CreateDir(path) if path.starts_with(&scratch) => {
                    fs::create_dir_all(path)?
                }
                SetupStep::WriteFile { path, bytes } if path.starts_with(&scratch) => {
                    fs::write(path, bytes)?
                }
                SetupStep::WriteExecutable { path, bytes } if path.starts_with(&scratch) => {
                    fs::write(path, bytes)?;
                    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
                }
                SetupStep::Symlink { link, target } if link.starts_with(&scratch) => {
                    std::os::unix::fs::symlink(target, link)?
                }
                _ => {}
            }
        }
        let identity_config = spec
            .env
            .iter()
            .find(|(key, _)| key == "GIT_CONFIG_GLOBAL")
            .map(|(_, value)| PathBuf::from(value))
            .ok_or("lane Git identity config")?;
        let use_config_only = Command::new(&real_git)
            .args(["config", "--file"])
            .arg(&identity_config)
            .args(["--get", "user.useConfigOnly"])
            .output()?;
        assert_eq!(use_config_only.stdout, b"true\n");

        // Absolute Git receives the same host identity from the lane config,
        // including from a subdirectory. A repository-local identity remains
        // higher priority, and an empty lane identity cannot synthesize one.
        let absolute_repo = root.join("absolute-git-repo");
        let nested = absolute_repo.join("nested");
        fs::create_dir_all(&nested)?;
        let absolute = |args: &[&str], cwd: &Path| {
            Command::new(&real_git)
                .args(args)
                .current_dir(cwd)
                .env_clear()
                .envs(spec.env.iter().cloned())
                .output()
        };
        let rewrite = absolute(
            &["ls-remote", "--get-url", "fixture://project"],
            &absolute_repo,
        )?;
        assert!(rewrite.status.success());
        assert_eq!(rewrite.stdout, b"https://rewritten.example/project\n");
        assert!(absolute(&["init", "-q"], &absolute_repo)?.status.success());
        fs::write(nested.join("file"), "absolute Git identity")?;
        assert!(absolute(&["add", "file"], &nested)?.status.success());
        let host_name = Command::new(&real_git)
            .args(["config", "--file"])
            .arg(&identity_config)
            .args(["--includes", "--get", "user.name"])
            .output()?;
        let host_email = Command::new(&real_git)
            .args(["config", "--file"])
            .arg(&identity_config)
            .args(["--includes", "--get", "user.email"])
            .output()?;
        let host_commit = absolute(&["commit", "-m", "absolute Git"], &nested)?;
        let host_commit_identity = absolute(&["log", "-1", "--format=%an <%ae>"], &nested)?;
        if host_name.status.success() && host_email.status.success() {
            assert!(
                host_commit.status.success()
                    && host_commit_identity.stdout
                        == [
                            host_name
                                .stdout
                                .strip_suffix(b"\n")
                                .unwrap_or(&host_name.stdout),
                            b" <".as_slice(),
                            host_email
                                .stdout
                                .strip_suffix(b"\n")
                                .unwrap_or(&host_email.stdout),
                            b">\n".as_slice()
                        ]
                        .concat(),
                "absolute Git did not use the host's configured identity"
            );
        } else {
            assert!(!host_commit.status.success());
        }

        assert!(
            absolute(&["config", "user.name", "Repository Identity"], &nested)?
                .status
                .success()
        );
        assert!(
            absolute(
                &["config", "user.email", "repository@example.test"],
                &nested
            )?
            .status
            .success()
        );
        fs::write(nested.join("local"), "repository identity")?;
        assert!(absolute(&["add", "local"], &nested)?.status.success());
        assert!(
            absolute(&["commit", "-m", "local identity"], &nested)?
                .status
                .success()
        );
        let local_identity = absolute(&["log", "-1", "--format=%an <%ae>"], &nested)?;
        assert_eq!(
            local_identity.stdout,
            b"Repository Identity <repository@example.test>\n"
        );

        fs::write(
            &identity_config,
            "[user]\n useConfigOnly = true\n name = Fixture Host\n email = fixture-host@example.test\n",
        )?;
        let unbound_repo = root.join("unbound-wrapper-repo");
        fs::create_dir(&unbound_repo)?;
        let unbound_path = format!("{}:/usr/bin:/bin", scratch.join("bin").display());
        let unbound = |script: &str| -> Result<std::process::Output, Box<dyn std::error::Error>> {
            Ok(Command::new("/bin/sh")
                .args(["-c", script])
                .current_dir(&unbound_repo)
                .env_clear()
                .env("HOME", &home)
                .env("PATH", &unbound_path)
                .output()?)
        };
        assert!(unbound("git init -q")?.status.success());
        fs::write(unbound_repo.join("file"), "unbound wrapper identity")?;
        assert!(
            unbound("git add file && git commit -qm wrapper")?
                .status
                .success()
        );
        let unbound_identity = unbound("git log -1 --format='%an <%ae>'")?;
        assert_eq!(
            unbound_identity.stdout,
            b"Fixture Host <fixture-host@example.test>\n"
        );

        let empty_identity = root.join("empty-identity.gitconfig");
        fs::write(&empty_identity, "[user]\n useConfigOnly = true\n")?;
        let no_identity = root.join("no-identity-repo");
        fs::create_dir(&no_identity)?;
        let no_identity_env = spec
            .env
            .iter()
            .filter(|(key, _)| key != "GIT_CONFIG_GLOBAL")
            .cloned()
            .collect::<Vec<_>>();
        let init = Command::new(&real_git)
            .args(["init", "-q"])
            .current_dir(&no_identity)
            .env_clear()
            .envs(no_identity_env.iter().cloned())
            .env("GIT_CONFIG_GLOBAL", &empty_identity)
            .output()?;
        assert!(init.status.success());
        fs::write(no_identity.join("file"), "missing identity")?;
        let add = Command::new(&real_git)
            .args(["add", "file"])
            .current_dir(&no_identity)
            .env_clear()
            .envs(no_identity_env.iter().cloned())
            .env("GIT_CONFIG_GLOBAL", &empty_identity)
            .output()?;
        assert!(add.status.success());
        let refused_commit = Command::new(&real_git)
            .args(["commit", "-m", "must refuse"])
            .current_dir(&no_identity)
            .env_clear()
            .envs(no_identity_env.iter().cloned())
            .env("GIT_CONFIG_GLOBAL", &empty_identity)
            .output()?;
        assert!(!refused_commit.status.success());

        let lane_subdir = repo.join("nested");
        fs::create_dir(&lane_subdir)?;
        fs::write(lane_subdir.join("file"), "lane object")?;
        let run_at = |cwd: &Path,
                      args: &[&str]|
         -> Result<std::process::Output, Box<dyn std::error::Error>> {
            Ok(Command::new("/bin/sh")
                .args(["-c", "exec git \"$@\"", "sh"])
                .args(args)
                .current_dir(cwd)
                .env_clear()
                .env("HOME", &home)
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin", scratch.join("bin").display()),
                )
                .output()?)
        };
        let run = |args: &[&str]| run_at(&repo, args);
        for args in [&["add", "file"][..], &["commit", "-m", "lane"][..]] {
            let output = run_at(&lane_subdir, args)?;
            assert!(
                output.status.success(),
                "{adapter}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(marker.exists(), "{adapter}: pinned Git was not invoked");
        assert!(scratch.join("git-common/index").exists());
        assert!(
            scratch
                .join("git-common/refs/heads")
                .read_dir()?
                .next()
                .is_some()
        );
        assert!(scratch.join("git-objects").read_dir()?.next().is_some());
        let author = run(&["log", "-1", "--format=%an <%ae>"])?;
        assert_eq!(author.stdout, b"Lane Author <lane@example.com>\n");
        let lane_head = run(&["rev-parse", "HEAD"])?.stdout;
        let lane_index = fs::read(scratch.join("git-common/index"))?;
        let fixture = scratch.join("fixture-repo");
        fs::create_dir(&fixture)?;
        fs::write(repo.join("unstaged-lane-file"), "lane content")?;
        let fixture_commit = Command::new("/bin/sh")
            .args([
                "-c",
                "set -e; git init -q; printf fixture > file; git add -A; git -c user.name=Fixture -c user.email=fixture@example.test commit -qm base; git log -1 --format=%an",
            ])
            .current_dir(&fixture)
            .env_clear()
            .envs(spec.env.iter().cloned())
            .output()?;
        assert!(
            fixture_commit.status.success(),
            "{adapter}: {}",
            String::from_utf8_lossy(&fixture_commit.stderr)
        );
        assert!(fixture.join(".git").is_dir(), "{adapter}");
        assert_eq!(fixture_commit.stdout, b"Fixture\n", "{adapter}");
        assert_eq!(run(&["rev-parse", "HEAD"])?.stdout, lane_head, "{adapter}");
        assert_eq!(
            fs::read(scratch.join("git-common/index"))?,
            lane_index,
            "{adapter}"
        );
        assert!(
            !run(&["config", "--get", "fixture.poison"])?
                .status
                .success()
        );
        let helper = run(&["config", "--get-all", "credential.helper"])?;
        assert!(String::from_utf8_lossy(&helper.stdout).contains("auth git-credential"));
        let mut credential = Command::new(scratch.join("bin/git"))
            .args(["credential", "fill"])
            .current_dir(&repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        credential
            .stdin
            .take()
            .ok_or("stdin")?
            .write_all(b"protocol=https\nhost=github.com\n\n")?;
        let credential = credential.wait_with_output()?;
        assert!(credential.status.success());
        assert!(String::from_utf8_lossy(&credential.stdout).contains("password=fixture-secret"));
        assert_eq!(fs::read(repo.join(".git/HEAD"))?, host_head);
        assert_eq!(fs::read(repo.join(".git/config"))?, host_config);
        assert!(!repo.join(".git/index").exists());
        assert!(repo.join(".git/refs/heads").read_dir()?.next().is_none());
        Ok(())
    }

    macro_rules! adapter_git_test {
        ($name:ident, $adapter:literal) => {
            #[test]
            fn $name() -> Result<(), Box<dyn std::error::Error>> {
                adapter_private_git($adapter)
            }
        };
    }
    adapter_git_test!(private_git_claude, "claude");
    adapter_git_test!(private_git_codex, "codex");
    adapter_git_test!(private_git_cursor, "cursor");
    adapter_git_test!(private_git_agy, "agy");
    adapter_git_test!(private_git_opencode, "opencode");

    #[test]
    fn private_git_command_writer_is_unsupported() {
        assert!(
            !provenant_records::AdapterSpec::get("command")
                .expect("command")
                .launch
                .write
        );
    }

    #[test]
    fn github_policy_guards_labels_and_verb_prefixes() -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
        use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
        let temp = tempfile::tempdir()?;
        let shim = temp.path().join("gh");
        let real_gh = temp.path().join("real-gh");
        fs::write(&real_gh, "#!/bin/sh\nprintf '<%s>\\n' \"$@\"\n")?;
        fs::set_permissions(&real_gh, fs::Permissions::from_mode(0o700))?;
        let policy = provenant_config::GithubPolicy {
            deny_verbs: vec!["release create".into()],
            deny_labels: vec![" ci ".into(), "".into(), "needs review".into()],
        };
        fs::write(
            &shim,
            gh_wrapper(real_gh.to_str().unwrap(), "'/tmp'", &policy),
        )?;
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o700))?;
        let executor = SystemExecutor::new(1)?;
        let run = |args: &[&str]| {
            let argv: Vec<_> = std::iter::once(shim.as_os_str().to_owned())
                .chain(args.iter().map(OsString::from))
                .collect();
            executor.execute(ExecOptions::new(&argv, temp.path(), Duration::from_secs(5)))
        };
        for args in [
            vec!["pr", "edit", "42", "--add-label", "ci"],
            vec!["pr", "create", "--label=docs,ci"],
            vec!["issue", "edit", "42", "--add-label=ci"],
            vec!["issue", "create", "--label", "ci"],
            vec!["pr", "create", "-l", "ci"],
            vec!["pr", "create", "-l=ci"],
            vec!["issue", "create", "-lci"],
            vec!["issue", "edit", "42", "--add-label", "docs, ci"],
            vec!["pr", "create", "--label", "docs, needs review "],
        ] {
            let output = run(&args)?;
            assert_eq!(output.termination, Termination::Exit(1));
            assert_eq!(output.stderr.bytes, b"github.deny_labels\n");
        }
        let output = run(&["release", "create", "v1"])?;
        assert_eq!(output.termination, Termination::Exit(1));
        assert_eq!(output.stderr.bytes, b"github.deny_verbs\n");
        let output = run(&["pr", "create", "--title", "hello world", "-l", "docs"])?;
        assert_eq!(output.termination, Termination::Exit(0));
        assert_eq!(
            output.stdout.bytes,
            b"<pr>\n<create>\n<--title>\n<hello world>\n<-l>\n<docs>\n"
        );
        assert!(run(&["pr", "create", "--title", "docs"])?.termination == Termination::Exit(0));
        assert!(run(&["release", "view"])?.termination == Termination::Exit(0));
        Ok(())
    }

    #[test]
    fn publishing_setup_supports_every_writer_adapter() -> Result<(), ProviderError> {
        let prompt = crate::Prompt::new("fixture".into());
        let token = crate::tests::fixture_token();
        for entry in crate::REGISTRY {
            let id = entry.adapter.id();
            if id == "command" {
                assert!(
                    !provenant_records::AdapterSpec::get(id)
                        .expect("registered adapter spec")
                        .launch
                        .write
                );
                continue;
            }
            let mut route = crate::tests::route(id, "fixture-model");
            route.account_key = None;
            let plan = crate::tests::fixture_access_plan(
                crate::Access::WorktreeWrite,
                provenant_records::AdapterSpec::get(id).and_then(|spec| spec.authentication),
                false,
            );
            let mut spec = entry.adapter.prepare(crate::tests::fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                (id == "claude").then_some(&token),
            ))?;
            let result = spec.configure_publishing(
                &plan.permissions().scratch.path,
                Path::new("/fixture/git/bin/git"),
                Path::new("/fixture/gh"),
                Path::new("/fixture/home"),
                &provenant_config::GithubPolicy::default(),
            );
            assert!(result.is_ok(), "publishing setup for {id}: {result:?}");
            let wrapper = plan.permissions().scratch.path.join("bin/git");
            let bytes = spec
                .setup
                .iter()
                .find_map(|step| match step {
                    SetupStep::WriteExecutable { path, bytes } if path == &wrapper => Some(bytes),
                    _ => None,
                })
                .ok_or(ProviderError::InvalidPath)?;
            let wrapper_text = String::from_utf8_lossy(bytes);
            assert!(wrapper_text.contains("include.path="));
        }
        Ok(())
    }

    #[test]
    fn writer_publishing_redacts_credentials() -> Result<(), Box<dyn std::error::Error>> {
        use crate::{Access, Adapter, Prompt, codex::Codex};
        use provenant_confinement::redact::Redactor;
        use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
        use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let home = root.join("home fixture's directory");
        let attempt = root.join("attempt");
        let scratch = attempt.join("scratch fixture's directory");
        for path in [&home, &scratch] {
            fs::create_dir_all(path)?;
        }
        fs::create_dir_all(home.join(".config/gh"))?;
        // This synthetic login stays outside the run files, like the real one.
        let token = format!("ghp_{}", "f".repeat(36));
        fs::write(
            home.join(".config/gh/hosts.yml"),
            format!("username=fixture\npassword={token}\n"),
        )?;
        let gh = root.join("gh fixture's executable");
        fs::write(
            &gh,
            format!(
                "#!/bin/sh\n[ \"$GH_CONFIG_DIR\" = {} ] || exit 1\ncase \"$*\" in\n    'auth git-credential get') cat \"$GH_CONFIG_DIR/hosts.yml\" ;;\n    'pr create'|'pr comment'|'pr view'|'pr checks') printf 'fixture pr\\n' ;;\n    *) exit 1 ;;\nesac\n",
                shell_quote(home.join(".config/gh").to_str().ok_or("home path")?),
            ),
        )?;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700))?;
        let git = root.join("git fixture's executable");
        std::os::unix::fs::symlink(
            provenant_os::git::git_executable()
                .ok_or("Git unavailable")?
                .canonicalize()?,
            &git,
        )?;
        let executor = SystemExecutor::new(1)?;
        let mut env = vec![
            ("HOME".into(), home.as_os_str().to_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_COUNT".into(), "0".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ];
        let run = |tool: &Path,
                   args: &[&str],
                   cwd: &Path,
                   env: &[(OsString, OsString)],
                   stdin: Option<&[u8]>| {
            let argv: Vec<_> = std::iter::once(tool.as_os_str().to_owned())
                .chain(args.iter().map(OsString::from))
                .collect();
            let mut stdout = Redactor::new([])?;
            let mut stderr = Redactor::new([])?;
            let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(5));
            options.env = Some(env);
            options.stdin = stdin;
            options.stdout_filter = Some(&mut stdout);
            options.stderr_filter = Some(&mut stderr);
            Ok::<_, Box<dyn std::error::Error>>(executor.execute(options)?)
        };
        let base = crate::tests::fixture_access_plan(Access::WorktreeWrite, Some("login"), false);
        let mut permissions = base.permissions().clone();
        permissions.scratch.path = scratch.clone();
        if let Some(private) = &mut permissions.private_state {
            private.path = attempt.join("provider-state");
        }
        let plan = ConfinedPlan::new(PlanRequest {
            scope: base.scope(),
            permissions,
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        })?;
        let mut route = crate::tests::route("codex", "fixture-model");
        route.account_key = None;
        let prompt = Prompt::new("fixture prompt".into());
        let mut input = crate::tests::fixture_input(&route, &plan, &prompt, &root, None);
        input.provenant.git = &git;
        let mut spec = Codex.prepare(input)?;
        spec.configure_publishing(&scratch, &git, &gh, &home, &Default::default())?;
        // Apply only the generated publishing setup; adapter-specific auth
        // setup belongs to the existing provider tests.
        fs::create_dir_all(scratch.join("bin"))?;
        for step in &spec.setup {
            match step {
                SetupStep::CreateDir(path) if path.starts_with(&scratch) => {
                    fs::create_dir_all(path)?
                }
                SetupStep::WriteFile { path, bytes } if path.starts_with(&scratch) => {
                    fs::write(path, bytes)?
                }
                SetupStep::WriteExecutable { path, bytes } if path.starts_with(&scratch) => {
                    fs::write(path, bytes)?;
                    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
                }
                SetupStep::Symlink { link, target } if link.starts_with(&scratch) => {
                    std::os::unix::fs::symlink(target, link)?
                }
                _ => {}
            }
        }
        env[1].1 = format!("{}:/usr/bin:/bin", scratch.join("bin").display()).into();
        let lane_git = scratch.join("bin/git");
        // The wrapper must carry the config even when the shell scrubs env.
        let mut private_env = env.clone();
        private_env[0].1 = root.join("private-home").into();
        let credential = run(
            &lane_git,
            &["credential", "fill"],
            &root,
            &private_env,
            Some(b"protocol=https\nhost=github.com\n\n"),
        )?;
        assert_eq!(credential.termination, Termination::Exit(0));
        assert!(
            credential
                .stdout
                .bytes
                .windows(b"password=[REDACTED]".len())
                .any(|window| window == b"password=[REDACTED]")
        );
        fs::write(attempt.join("stdout.log"), &credential.stdout.bytes)?;
        fs::write(attempt.join("stderr.log"), &credential.stderr.bytes)?;
        fs::write(
            attempt.join("launch.json"),
            serde_json::to_vec(&serde_json::json!({"argv": spec.argv, "env": env}))?,
        )?;
        fs::write(attempt.join("result.md"), &credential.stdout.bytes)?;
        for verb in ["create", "comment", "view", "checks"] {
            assert_eq!(
                run(&scratch.join("bin/gh"), &["pr", verb], &root, &env, None)?.termination,
                Termination::Exit(0)
            );
        }
        let mut pending = vec![attempt];
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    pending.push(entry.path());
                } else if kind.is_file() {
                    assert!(
                        !fs::read(entry.path())?
                            .windows(token.len())
                            .any(|window| window == token.as_bytes()),
                        "fixture credential leaked to a run file"
                    );
                }
            }
        }
        Ok(())
    }

    fn bare_spec(path: &str) -> crate::LaunchSpec {
        crate::LaunchSpec {
            argv: Vec::new(),
            env: vec![("PATH".into(), path.into())],
            stdin: crate::Stdin::Null,
            secret_input: None,
            cwd: PathBuf::from("/"),
            setup: Vec::new(),
            omitted_discovery: Vec::new(),
            user_skills: Default::default(),
            tool_requirements: Default::default(),
            git_wrapper: Default::default(),
            profile: provenant_confinement::permissions::ProviderProfile {
                spec: ProviderProfileSpec::default(),
                observations: Default::default(),
            },
            stream: crate::StreamState::new("claude", |_| Ok(Vec::new())),
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn the_cargo_shim_hands_the_command_to_the_build_slot_gate() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (scratch, state) = (root.join("it's scratch"), root.join("state"));
        let mut spec = bare_spec("/home/.cargo/bin:/usr/bin:/bin");
        spec.cwd = root.clone();
        let provenant = root.join("fake provenant");
        let gate = BuildSlotGate {
            provenant: &provenant,
            state: &state,
            slots: 3,
            wait_seconds: 1800,
            cargo: Path::new("/home/.cargo/bin/cargo"),
            wait_marker: Path::new("/tmp/lane/build-slot-wait"),
        };
        spec.configure_build_slots(&scratch, &gate).unwrap();
        let (_, path) = spec.env.iter().find(|(key, _)| key == "PATH").unwrap();
        assert_eq!(
            std::env::split_paths(path).next().unwrap(),
            scratch.join("bin")
        );
        // The lane gets no write to the slot directory, the state root,
        // `fabric/locks` or anything near a repository's `.git`: only a read
        // of the slot directory, which the host provisions beforehand.
        let slots = state.join(provenant_os::build_slots::DIRECTORY.join("/"));
        assert!(spec.profile.spec.state_writes.is_empty());
        assert!(spec.profile.spec.tool_writes.is_empty());
        assert!(spec.profile.spec.runtime_reads.contains(&subtree(&slots)));
        assert!(
            !spec
                .setup
                .iter()
                .any(|step| matches!(step, SetupStep::CreateDir(path) if path.starts_with(&slots))),
            "the lane never creates the slot directory"
        );
        let bytes = spec
            .setup
            .iter()
            .find_map(|step| match step {
                SetupStep::WriteExecutable { path, bytes } if path.ends_with("bin/cargo") => {
                    Some(bytes.clone())
                }
                _ => None,
            })
            .unwrap();
        // Run the script against a stand-in for `provenant` that prints argv
        // and the host's slot environment, with hostile values in the lane's.
        std::fs::write(
            &provenant,
            "#!/bin/sh\nprintf '<%s>' \"$@\"\nprintf '[%s|%s|%s]' \"$PROVENANT_BUILD_SLOT_STATE\" \"$PROVENANT_BUILD_SLOTS\" \"$PROVENANT_BUILD_WAIT_SECS\"\n",
        )
        .unwrap();
        let shim = root.join("cargo");
        std::fs::write(&shim, bytes).unwrap();
        for file in [&provenant, &shim] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = std::process::Command::new(&shim)
            .args(["test", "--", "two words"])
            .env("PROVENANT_BUILD_SLOT_STATE", "/lane/chosen")
            .env("PROVENANT_BUILD_SLOTS", "64")
            .env("PROVENANT_BUILD_WAIT_SECS", "1")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "<internal><build-slot><--></home/.cargo/bin/cargo><test><--><two words>[{}|3|1800]",
                state.display()
            ),
            "the host's slot directory, count and wait override the lane's environment"
        );
    }

    /// The finished writer profile, with the confinement's own list of every
    /// write allowance, must leave the slot directory out of reach: no grant
    /// equals it or contains it, so a lane cannot create, unlink or replace a
    /// slot file. The profile comes from the real writer preparation, with the
    /// state grants `grant` adds for a real state root, so a grant covering the
    /// state root itself is caught rather than hidden by a synthetic spec.
    #[test]
    fn no_write_grant_of_the_finished_lane_profile_reaches_the_slot_directory() {
        use crate::{Access, Adapter, Prompt, codex::Codex};
        use provenant_confinement::permissions::{PathScope, write_grants};
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let state = base.join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
        let slots = state.join(provenant_os::build_slots::DIRECTORY.join("/"));
        let plan = crate::tests::fixture_access_plan(Access::WorktreeWrite, Some("login"), true);
        let prompt = Prompt::new("fixture prompt".into());
        let mut route = crate::tests::route("codex", "fixture-model");
        route.account_key = None;
        let mut input = crate::tests::fixture_input(&route, &plan, &prompt, &base, None);
        input.provenant.state = &state;
        let mut spec = Codex.prepare(input).unwrap();
        let gate = BuildSlotGate {
            provenant: Path::new("/fixture/bin/provenant"),
            state: &state,
            slots: 2,
            wait_seconds: 60,
            cargo: Path::new("/home/.cargo/bin/cargo"),
            wait_marker: Path::new("/tmp/lane/build-slot-wait"),
        };
        spec.configure_build_slots(&base.join("scratch"), &gate)
            .unwrap();
        // The lane reads the slot directory so it can lock a slot file.
        assert!(spec.profile.spec.runtime_reads.contains(&subtree(&slots)));
        let grants = write_grants(plan.permissions(), &spec.profile);
        assert!(!grants.is_empty(), "the profile lists its write grants");
        for grant in &grants {
            let reaches = match grant.scope {
                PathScope::Exact => grant.path == slots,
                PathScope::Subtree => slots.starts_with(&grant.path),
            };
            assert!(
                !reaches,
                "{:?} grant {} reaches {}",
                grant.source,
                grant.path.display(),
                slots.display()
            );
        }
    }

    #[test]
    fn the_cargo_shim_refuses_settings_that_cannot_be_trusted() {
        let mut spec = bare_spec("/usr/bin");
        for (state, slots, wait) in [
            ("/s", 0, 60),
            ("/s", 65, 60),
            ("/s", 1, 0),
            ("relative", 1, 60),
            ("/s\nx", 1, 60),
        ] {
            let gate = BuildSlotGate {
                provenant: Path::new("/p"),
                state: Path::new(state),
                slots,
                wait_seconds: wait,
                cargo: Path::new("/c"),
                wait_marker: Path::new("/t/build-slot-wait"),
            };
            assert!(
                spec.configure_build_slots(Path::new("/scratch"), &gate)
                    .is_err(),
                "{state} {slots} {wait}"
            );
        }
        assert!(spec.setup.is_empty());
    }

    #[test]
    fn rejected_build_slot_shim_leaves_launch_unchanged() {
        let gate = BuildSlotGate {
            provenant: Path::new("/runtime"),
            state: Path::new("/state"),
            slots: 1,
            wait_seconds: 60,
            cargo: Path::new("/cargo"),
            wait_marker: Path::new("/scratch/wait"),
        };
        for missing_path in [true, false] {
            let mut spec = bare_spec("/usr/bin");
            if missing_path {
                spec.env.retain(|(key, _)| key != "PATH");
            }
            let env = spec.env.clone();
            let profile = spec.profile.spec.clone();
            let scratch = if missing_path {
                Path::new("/scratch")
            } else {
                Path::new("/scratch:invalid")
            };
            assert!(spec.configure_build_slots(scratch, &gate).is_err());
            assert_eq!(spec.env, env);
            assert_eq!(spec.profile.spec, profile);
            assert!(spec.setup.is_empty());
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn claude_shell_restores_jobs_after_scrub() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefix");
        std::fs::write(
            &path,
            claude_shell(&[
                ("CARGO_BUILD_JOBS".into(), "4".into()),
                ("CARGO_INCREMENTAL".into(), "0".into()),
                ("CARGO_PROFILE_DEV_DEBUG".into(), "line-tables-only".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/lane/gitconfig".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ]),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let result = std::process::Command::new(path)
            .env_clear()
            .arg("printf '%s\\n' \"$CARGO_BUILD_JOBS\" \"$CARGO_INCREMENTAL\" \"$CARGO_PROFILE_DEV_DEBUG\" \"$GIT_CONFIG_GLOBAL\" \"$GIT_CONFIG_NOSYSTEM\" 'quoted argument'")
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            result.stdout,
            b"4\n0\nline-tables-only\n/lane/gitconfig\n1\nquoted argument\n"
        );
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn claude_shell_preserves_non_utf8_and_quotes() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir()?;
        let script = dir.path().join("prefix");
        let value = vec![b'a', 0xff, b'\'', b'$', b'\n', b'z'];
        std::fs::write(
            &script,
            claude_shell(&[("CARGO_BUILD_JOBS".into(), OsString::from_vec(value.clone()))]),
        )?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        let output = std::process::Command::new(script)
            .env_clear()
            .arg("printf %s \"$CARGO_BUILD_JOBS\"")
            .output()?;
        assert!(output.status.success());
        assert_eq!(output.stdout, value);
        Ok(())
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn wrapper_binds_the_worktree_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let worktree = root.join("work tree");
        let nested = worktree.join("a");
        let sibling = root.join("work tree2");
        let outside = root.join("outside");
        // A repository created under the checkout, and a plain directory in it.
        let inner = nested.join("fixture");
        let inner_deep = inner.join("sub/deeper");
        let inner_file = nested.join("gitfile");
        for path in [&inner_deep, &sibling, &outside, &inner_file] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(worktree.join(".git"), "gitdir: /host/admin\n").unwrap();
        std::fs::create_dir(inner.join(".git")).unwrap();
        std::fs::write(inner_file.join(".git"), "gitdir: /elsewhere\n").unwrap();
        let git = root.join("git");
        std::fs::write(
            &git,
            "#!/bin/sh\nprintf '%s|%s|%s|%s\\n' \"$GIT_DIR\" \"$GIT_AUTHOR_NAME\" \"$GIT_INDEX_FILE\" \"$*\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let wrapper = root.join("wrapper");
        std::fs::write(
            &wrapper,
            git_wrapper(
                &git,
                &GitWrapper {
                    scrub: true,
                    bound: Some(BoundGit {
                        worktree: worktree.clone(),
                        env: vec![
                            ("GIT_DIR".into(), "/private/it's".into()),
                            ("GIT_AUTHOR_NAME".into(), "Lane".into()),
                        ],
                    }),
                    ..Default::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Selectors inherited from a parent, such as a hook's Git, are cleared.
        let run = |cwd: &Path, args: &[&str]| {
            let output = std::process::Command::new(&wrapper)
                .env_clear()
                .env("GIT_DIR", "/inherited")
                .env("GIT_AUTHOR_NAME", "Inherited")
                .env("GIT_INDEX_FILE", "/inherited/index")
                .current_dir(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap()
        };
        let bound = |args: &str| format!("/private/it's|Lane||{args}\n");
        let unbound = |args: &str| format!("|||{args}\n");
        assert_eq!(run(&worktree, &["status"]), bound("status"));
        assert_eq!(run(&nested, &["status"]), bound("status"));
        for cwd in [&outside, &sibling] {
            assert_eq!(run(cwd, &["status"]), unbound("status"), "{cwd:?}");
        }
        // A repository nested in the checkout is not the lane's.
        for cwd in [&inner, &inner.join("sub"), &inner_deep, &inner_file] {
            assert_eq!(run(cwd, &["commit"]), unbound("commit"), "{cwd:?}");
        }
        // `init` and `clone` create a repository, so they never bind.
        let plain = nested.join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert_eq!(run(&plain, &["status"]), bound("status"));
        assert_eq!(run(&plain, &["init", "-q"]), unbound("init -q"));
        assert_eq!(
            run(&plain, &["-C", ".", "clone", "x"]),
            unbound("-C . clone x")
        );
        let inside = nested.to_str().unwrap();
        let outside_path = outside.to_str().unwrap();
        assert_eq!(
            run(&outside, &["-c", "a=b", "-C", inside, "log"]),
            bound(&format!("-c a=b -C {inside} log"))
        );
        // Options with a separate value do not hide a later -C.
        for option in [
            "--git-dir",
            "--work-tree",
            "--namespace",
            "--config-env",
            "--attr-source",
            "--super-prefix",
        ] {
            assert_eq!(
                run(&outside, &[option, "v", "-C", inside, "log"]),
                bound(&format!("{option} v -C {inside} log"))
            );
        }
        let fixture = inner.to_str().unwrap();
        assert_eq!(
            run(&worktree, &["--config-env", "x=Y", "-C", fixture, "commit"]),
            unbound(&format!("--config-env x=Y -C {fixture} commit"))
        );
        assert_eq!(
            run(&outside, &["--config-env", "x=Y", "-C", inside, "commit"]),
            bound(&format!("--config-env x=Y -C {inside} commit"))
        );
        // Known valueless and `=` options are skipped; unknown ones do not bind.
        assert_eq!(
            run(&worktree, &["--no-pager", "--git-dir=v", "-p", "log"]),
            bound("--no-pager --git-dir=v -p log")
        );
        for args in [&["--future-option", "log"][..], &["-X", "log"], &["-c"]] {
            assert_eq!(run(&worktree, args), unbound(&args.join(" ")), "{args:?}");
        }
        assert_eq!(
            run(&worktree, &["-C", outside_path, "log"]),
            unbound(&format!("-C {outside_path} log"))
        );
        let nested_repo = inner.to_str().unwrap();
        assert_eq!(
            run(&worktree, &["-C", nested_repo, "log"]),
            unbound(&format!("-C {nested_repo} log"))
        );
        // A symlink into the worktree is judged by its physical path.
        let link = outside.join("link");
        std::os::unix::fs::symlink(&worktree, &link).unwrap();
        assert_eq!(run(&link, &["status"]), bound("status"));
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn git_wrapper_preserves_native_executable_path() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::PermissionsExt,
        };
        let dir = tempfile::tempdir()?;
        let git = PathBuf::from(OsString::from_vec(b"/fixture/git-\xff-'\n".to_vec()));
        let mut script = git_wrapper(
            &git,
            &GitWrapper {
                scrub: true,
                ..Default::default()
            },
        )?;
        let position = script
            .windows(5)
            .position(|bytes| bytes == b"exec ")
            .ok_or("missing exec")?;
        script.splice(position..position + 5, b"printf '%s' ".iter().copied());
        let wrapper = dir.path().join("git-wrapper");
        std::fs::write(&wrapper, script)?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))?;
        let output = std::process::Command::new(wrapper).output()?;
        assert!(output.status.success());
        assert_eq!(output.stdout, git.as_os_str().as_bytes());
        Ok(())
    }

    #[test]
    fn unrepresentable_mcp_paths_warn_and_preserve_claude_settings() -> Result<(), ProviderError> {
        use std::os::unix::ffi::OsStringExt;
        let bad = PathBuf::from(OsString::from_vec(b"/fixture/\xff".to_vec()));
        let lane = LaneRuntime {
            executable: Path::new("/fixture/provenant"),
            git: Path::new("/fixture/git"),
            state: Path::new("/fixture/state"),
            child_state: &bad,
            home: Path::new("/fixture/home"),
            depth: 1,
            attempt: AttemptId::from_bytes([1; 16]),
            assets: Path::new("/fixture/assets"),
        };
        let mut codex = Vec::new();
        lane.codex(&mut codex)?;
        assert!(codex.is_empty());
        let mut claude = Vec::new();
        let setup = lane.claude(Path::new("/fixture/scratch"), &mut claude, "Bash", &[])?;
        let documents: Vec<Value> = setup
            .iter()
            .filter_map(|step| match step {
                SetupStep::WriteFile { bytes, .. } => serde_json::from_slice(bytes).ok(),
                _ => None,
            })
            .collect();
        assert_eq!(documents[0]["mcpServers"], serde_json::json!({}));
        assert!(documents[1]["hooks"].is_object());
        assert!(claude.iter().any(|arg| arg == "--setting-sources"));
        Ok(())
    }

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

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn lane_path_pins_provenant_and_child_context_after_shell_scrubbing()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{Adapter, Prompt, codex::Codex};
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let executable = root.join("host's provenant");
        // Process-edge stand-in observes the actual launcher, without a
        // provider login or a nested native sandbox.
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '<%s>' \"$@\"\nprintf '\\n%s\\n' \"$HOME\" \"$PROVENANT_STATE_DIR\" \"$PROVENANT_RUN_STATE_DIR\" \"$PROVENANT_PARENT_ATTEMPT_ID\" \"$PROVENANT_LANE_DEPTH\" \"$PROVENANT_GIT\" \"$PROVENANT_ROOT\" \"$PROVENANT_AGENT_LABEL\"\n",
        )?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        let plan = crate::tests::fixture_access_plan(crate::Access::ReadOnly, Some("login"), true);
        let mut route = crate::tests::route("codex", "fixture-model");
        route.account_key = None;
        let prompt = Prompt::new("fixture prompt".into());
        let mut input =
            crate::tests::fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.provenant.executable = &executable;
        let lane = &input.provenant;
        let child_state = lane.child_state.to_owned();
        let host_state = lane.state.to_owned();
        let expected_context = [
            lane.home.display().to_string(),
            lane.state.display().to_string(),
            lane.child_state.display().to_string(),
            lane.attempt.to_string(),
            lane.depth.to_string(),
            lane.git.display().to_string(),
            lane.assets.display().to_string(),
            format!("lane-{}", lane.attempt),
        ];
        let spec = Codex.prepare(input)?;
        let scratch = &plan.permissions().scratch.path;
        let bin = scratch.join("bin");
        let path = spec
            .env
            .iter()
            .find(|(key, _)| key == "PATH")
            .ok_or("PATH")?;
        assert!(std::env::split_paths(&path.1).any(|entry| entry == bin));
        let bytes = spec
            .setup
            .iter()
            .find_map(|step| match step {
                SetupStep::WriteExecutable { path, bytes } if path == &bin.join("provenant") => {
                    Some(bytes)
                }
                _ => None,
            })
            .ok_or("provenant launcher")?;
        // Stage outside the synthetic plan's /fixture paths.
        let staged_bin = root.join("bin");
        std::fs::create_dir(&staged_bin)?;
        let launcher = staged_bin.join("provenant");
        std::fs::write(&launcher, bytes)?;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))?;
        assert!(
            spec.profile
                .spec
                .process_executables
                .contains(&exact(&executable))
        );
        assert!(
            spec.profile
                .spec
                .state_writes
                .contains(&subtree(&child_state))
        );
        assert!(
            !spec
                .profile
                .spec
                .state_writes
                .contains(&subtree(&host_state))
        );
        for args in [
            vec!["run", "--mode", "read-only", "--prompt", "two words"],
            vec!["output", "child-id"],
        ] {
            let output = std::process::Command::new("/bin/sh")
                .args(["-c", "exec provenant \"$@\"", "lane"])
                .args(&args)
                .env_clear()
                .env("PATH", &staged_bin)
                .env("HOME", root.join("private-provider-home"))
                .output()?;
            assert!(output.status.success(), "{output:?}");
            let expected = format!(
                "{}\n{}\n",
                args.iter()
                    .map(|arg| format!("<{arg}>"))
                    .collect::<String>(),
                expected_context.join("\n\n"),
            );
            assert_eq!(String::from_utf8(output.stdout)?, expected);
        }
        Ok(())
    }
}
