//! Provenant-only provider customisation and writer publishing setup.
use crate::launch::{exact, subtree};
use crate::{ProviderError, SetupStep};
use provenant_confinement::permissions::ProviderProfileSpec;
use provenant_core::{AttemptId, RUN_STATE_DIRECTORY_ENV};
use provenant_records::Harness;
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub(crate) struct GitWrapper {
    pub(crate) scrub: bool,
    include: Option<String>,
    git: Option<PathBuf>,
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
        let bytes = git_wrapper(
            git,
            &self.env,
            self.git_wrapper.scrub,
            self.git_wrapper.include.as_deref(),
        )?;
        let wrapper = bin.join("git");
        self.setup.retain(|step| {
            !matches!(step,
            SetupStep::WriteExecutable { path, .. } | SetupStep::Symlink { link: path, .. }
            if path == &wrapper)
        });
        if !self.setup.contains(&SetupStep::CreateDir(bin.to_owned())) {
            self.setup.push(SetupStep::CreateDir(bin.to_owned()));
        }
        self.setup.push(SetupStep::WriteExecutable {
            path: wrapper,
            bytes,
        });
        let (_, path) = self
            .env
            .iter_mut()
            .find(|(key, _)| key == "PATH")
            .ok_or(ProviderError::InvalidPath)?;
        if std::env::split_paths(path).next().as_deref() != Some(bin) {
            *path = std::env::join_paths(
                std::iter::once(bin.to_owned()).chain(std::env::split_paths(path)),
            )
            .map_err(|_| ProviderError::InvalidPath)?;
        }
        Ok(())
    }
    /// Restore selected environment before Claude starts each Bash command.
    pub fn configure_claude_shell(&mut self, scratch: &Path) -> Result<(), ProviderError> {
        let path = scratch.join("bin/claude-shell");
        self.setup.push(SetupStep::CreateDir(scratch.join("bin")));
        self.setup.push(SetupStep::WriteExecutable {
            path: path.clone(),
            bytes: claude_shell(&self.env)?,
        });
        self.env
            .push(("CLAUDE_CODE_SHELL_PREFIX".into(), path.into_os_string()));
        Ok(())
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
        let (_, path) = self
            .env
            .iter_mut()
            .find(|(key, _)| key == "PATH")
            .ok_or(ProviderError::InvalidPath)?;
        if std::env::split_paths(path).next().as_deref() != Some(bin.as_path()) {
            *path = std::env::join_paths(
                std::iter::once(bin.clone()).chain(std::env::split_paths(path)),
            )
            .map_err(|_| ProviderError::InvalidPath)?;
        }
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
        self.profile.spec.runtime_reads.extend(
            std::iter::once(exact(git_path.as_str())).chain(
                crate::dylib::linked_libraries(Path::new(&git_path))
                    .into_iter()
                    .map(exact),
            ),
        );
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

fn git_wrapper(
    git: &Path,
    env: &[(OsString, OsString)],
    scrub: bool,
    include: Option<&str>,
) -> Result<Vec<u8>, ProviderError> {
    let git = git.to_str().ok_or(ProviderError::InvalidPath)?;
    if git.chars().any(char::is_control) {
        return Err(ProviderError::InvalidPath);
    }
    let mut bytes = b"#!/bin/sh\n".to_vec();
    if scrub {
        bytes.extend_from_slice(b"GIT_CONFIG_GLOBAL=/dev/null\nGIT_CONFIG_NOSYSTEM=1\nexport GIT_CONFIG_GLOBAL GIT_CONFIG_NOSYSTEM\n");
        bytes.extend_from_slice(identity_exports(env)?.as_bytes());
    }
    let executable = shell_quote(git);
    let include = include
        .map(|value| format!(" -c {value}"))
        .unwrap_or_default();
    bytes.extend_from_slice(format!("exec {executable}{include} \"$@\"\n").as_bytes());
    Ok(bytes)
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

fn shell_quote(value: &str) -> String {
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
        let mut settings = provenant_config::claude_lane_settings(self.executable, self.state)
            .map_err(|_| ProviderError::UnsupportedPolicy)?;
        settings["attribution"] = serde_json::json!({"commit": "", "pr": ""});
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
                bytes: git_wrapper(self.git, env, true, None)?,
            }
        } else {
            SetupStep::Symlink {
                link: bin.join("git"),
                target: self.git.to_owned(),
            }
        });
        env.extend([
            (
                "PROVENANT_AGENT_LABEL".into(),
                format!("lane-{}", self.attempt).into(),
            ),
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
        profile
            .runtime_reads
            .push(subtree(self.state.join("projects")));
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

fn claude_shell(env: &[(OsString, OsString)]) -> Result<Vec<u8>, ProviderError> {
    let mut exports = identity_exports(env)?;
    for name in [
        "CARGO_BUILD_JOBS",
        "CARGO_INCREMENTAL",
        "CARGO_PROFILE_DEV_DEBUG",
    ] {
        if let Some((_, value)) = env.iter().find(|(key, _)| key == name) {
            let value = value.to_str().ok_or(ProviderError::InvalidPath)?;
            exports.push_str(&format!("export {name}={}\n", shell_quote(value)));
        }
    }
    Ok(format!("#!/bin/sh\n{exports}exec /bin/bash -c \"$1\"\n").into_bytes())
}

/// Shell exports that carry the host's git identity past an environment scrub.
fn identity_exports(env: &[(OsString, OsString)]) -> Result<String, ProviderError> {
    let mut identity = String::new();
    for (key, value) in env {
        if [
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_WORK_TREE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
        ]
        .iter()
        .any(|candidate| key == candidate)
        {
            let value = value.to_str().ok_or(ProviderError::InvalidPath)?;
            identity.push_str(&format!(
                "export {}='{}'\n",
                key.to_string_lossy(),
                value.replace('\'', "'\\''")
            ));
        }
    }
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            spec.env
                .push(("GIT_AUTHOR_NAME".into(), "Fixture Author".into()));
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
            let identity = wrapper_text
                .find("export GIT_AUTHOR_NAME='Fixture Author'")
                .ok_or(ProviderError::Malformed)?;
            assert!(identity < wrapper_text.find("exec ").ok_or(ProviderError::Malformed)?);
            assert!(spec.env.iter().any(|(key, value)| {
                key == "GIT_DIR"
                    && value
                        == &plan
                            .permissions()
                            .scratch
                            .path
                            .join("git-common")
                            .into_os_string()
            }));
            assert!(wrapper_text.contains("export GIT_DIR="));
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
                ("GIT_AUTHOR_NAME".into(), "O'Brien".into()),
            ])
            .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let result = std::process::Command::new(path)
            .env_clear()
            .arg("printf '%s\\n' \"$CARGO_BUILD_JOBS\" \"$CARGO_INCREMENTAL\" \"$CARGO_PROFILE_DEV_DEBUG\" \"$GIT_AUTHOR_NAME\" 'quoted argument'")
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            result.stdout,
            b"4\n0\nline-tables-only\nO'Brien\nquoted argument\n"
        );
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn wrapper_restores_identity_after_scrub() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join("git");
        std::fs::write(&git, "#!/bin/sh\nprintf '%s\\n' \"$GIT_AUTHOR_NAME\" \"$GIT_COMMITTER_EMAIL\" \"$GIT_CONFIG_GLOBAL\"\n").unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        let lane = LaneRuntime {
            executable: Path::new("/fixture/provenant"),
            git: &git,
            state: Path::new("/fixture/state"),
            child_state: Path::new("/fixture/children"),
            home: Path::new("/fixture/home"),
            depth: 1,
            attempt: AttemptId::from_bytes([1; 16]),
            assets: Path::new("/fixture/assets"),
        };
        let wrapper = dir.path().join("wrapper");
        std::fs::write(
            &wrapper,
            git_wrapper(
                lane.git,
                &[
                    ("GIT_AUTHOR_NAME".into(), "O'Brien".into()),
                    ("GIT_COMMITTER_EMAIL".into(), "lane@example.org".into()),
                ],
                true,
                None,
            )
            .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = std::process::Command::new(wrapper)
            .env_clear()
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"O'Brien\nlane@example.org\n/dev/null\n");
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
}
