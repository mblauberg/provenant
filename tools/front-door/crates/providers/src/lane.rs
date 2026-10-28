//! Provenant-only provider customisation and writer publishing setup.
use crate::launch::{exact, subtree};
use crate::{ProviderError, SetupStep};
use provenant_confinement::permissions::ProviderProfileSpec;
use provenant_core::{AttemptId, RUN_STATE_DIRECTORY_ENV};
use provenant_records::Harness;
use serde_json::Value;
use std::{ffi::OsString, path::Path};

impl crate::LaunchSpec {
    /// Configure shell Git after adapter preparation, including shells that
    /// scrub Git environment variables. No credential is read during setup.
    pub fn configure_publishing(
        &mut self,
        scratch: &Path,
        git: &Path,
        gh: &Path,
        home: &Path,
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
        let git = shell_quote(&git_path);
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
        // Claude scrubs subprocess variables, so the wrapper restores identity.
        let identity = identity_exports(&self.env)?;
        let wrapper = format!(
            "#!/bin/sh\nGIT_CONFIG_GLOBAL=/dev/null\nGIT_CONFIG_NOSYSTEM=1\n\
             export GIT_CONFIG_GLOBAL GIT_CONFIG_NOSYSTEM\n{identity}exec {git} -c {include} \"$@\"\n"
        );
        let wrapper_path = scratch.join("bin/git");
        let step = self.setup.iter_mut().find(|step| match step {
            SetupStep::Symlink { link, .. } => link == &wrapper_path,
            SetupStep::WriteExecutable { path, .. } => path == &wrapper_path,
            _ => false,
        });
        let wrapper = SetupStep::WriteExecutable {
            path: wrapper_path,
            bytes: wrapper.into_bytes(),
        };
        if let Some(step) = step {
            *step = wrapper;
        } else {
            self.setup.push(SetupStep::CreateDir(scratch.join("bin")));
            self.setup.push(wrapper);
        }
        let (_, path) = self
            .env
            .iter_mut()
            .find(|(key, _)| key == "PATH")
            .ok_or(ProviderError::InvalidPath)?;
        *path = std::env::join_paths(
            std::iter::once(scratch.join("bin")).chain(std::env::split_paths(path)),
        )
        .map_err(|_| ProviderError::InvalidPath)?;
        self.setup.extend([
            SetupStep::WriteFile {
                path: config,
                bytes: config_text.into_bytes(),
            },
            SetupStep::WriteExecutable {
                path: scratch.join("bin/gh"),
                bytes: format!(
                    "#!/bin/sh\nGH_CONFIG_DIR={gh_config}\nexport GH_CONFIG_DIR\nexec {} \"$@\"\n",
                    shell_quote(&gh_path)
                )
                .into_bytes(),
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
                bytes: self.git_wrapper(env)?,
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

    fn git_wrapper(&self, env: &[(OsString, OsString)]) -> Result<Vec<u8>, ProviderError> {
        let git = self.git.to_str().ok_or(ProviderError::InvalidPath)?;
        if git.contains(['\'', '\n']) {
            return Err(ProviderError::InvalidPath);
        }
        let identity = identity_exports(env)?;
        Ok(format!(
            "#!/bin/sh\nGIT_CONFIG_GLOBAL=/dev/null\nGIT_CONFIG_NOSYSTEM=1\n\
             export GIT_CONFIG_GLOBAL GIT_CONFIG_NOSYSTEM\n{identity}exec '{git}' \"$@\"\n"
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

/// Shell exports that carry the host's git identity past an environment scrub.
fn identity_exports(env: &[(OsString, OsString)]) -> Result<String, ProviderError> {
    let mut identity = String::new();
    for (key, value) in env {
        if [
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "GIT_OBJECT_DIRECTORY",
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
    fn publishing_setup_supports_every_writer_adapter() -> Result<(), ProviderError> {
        let prompt = crate::Prompt::new("fixture".into());
        let token = crate::tests::fixture_token();
        for entry in crate::REGISTRY {
            let id = entry.adapter.id();
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
                Path::new("/fixture/git"),
                Path::new("/fixture/gh"),
                Path::new("/fixture/home"),
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
            // The publishing wrapper replaces the lane's, so it keeps the identity.
            assert!(
                String::from_utf8_lossy(bytes).contains("export GIT_AUTHOR_NAME='Fixture Author'"),
                "{id} publishing wrapper drops the git identity"
            );
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
        let mut spec = Codex.prepare(crate::tests::fixture_input(
            &route, &plan, &prompt, &root, None,
        ))?;
        spec.configure_publishing(&scratch, &git, &gh, &home)?;
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

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn wrapper_restores_identity_after_scrub() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join("git");
        std::fs::write(&git, "#!/bin/sh\nprintf '%s\\n' \"$GIT_AUTHOR_NAME\" \"$GIT_COMMITTER_EMAIL\" \"$GIT_CONFIG_GLOBAL\" \"$GIT_OBJECT_DIRECTORY\"\n").unwrap();
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
            lane.git_wrapper(&[
                ("GIT_AUTHOR_NAME".into(), "O'Brien".into()),
                ("GIT_COMMITTER_EMAIL".into(), "lane@example.org".into()),
                (
                    "GIT_OBJECT_DIRECTORY".into(),
                    "/scratch/O'Brien/objects".into(),
                ),
            ])
            .unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = std::process::Command::new(wrapper)
            .env_clear()
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            output.stdout,
            b"O'Brien\nlane@example.org\n/dev/null\n/scratch/O'Brien/objects\n"
        );
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
