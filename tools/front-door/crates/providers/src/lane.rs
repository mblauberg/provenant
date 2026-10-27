//! Provenant-only lane customisation; no user configuration is loaded.
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
        branch: &str,
    ) -> Result<(), ProviderError> {
        provenant_confinement::repo::GitRefName::branch(branch)
            .map_err(|_| ProviderError::InvalidPath)?;
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
        let git = shell_quote(&text(git)?);
        let gh_path = text(gh)?;
        let gh = shell_quote(&gh_path);
        let config = scratch.join("publishing.gitconfig");
        let hooks = scratch.join("publishing-hooks");
        let config_text = format!(
            "[core]\n\thooksPath = {}\n[credential]\n\thelper =\n\thelper = {}\n\tinteractive = false\n",
            serde_json::to_string(&text(&hooks)?).map_err(|_| ProviderError::Malformed)?,
            serde_json::to_string(&format!("!{gh} auth git-credential"))
                .map_err(|_| ProviderError::Malformed)?,
        );
        let include = shell_quote(&format!("include.path={}", text(&config)?));
        let wrapper = format!(
            "#!/bin/sh\nGIT_CONFIG_GLOBAL=/dev/null\nGIT_CONFIG_NOSYSTEM=1\n\
             export GIT_CONFIG_GLOBAL GIT_CONFIG_NOSYSTEM\nexec {git} -c {include} \"$@\"\n"
        );
        let wrapper_path = scratch.join("bin/git");
        let step = self
            .setup
            .iter_mut()
            .find(|step| match step {
                SetupStep::Symlink { link, .. } => link == &wrapper_path,
                SetupStep::WriteExecutable { path, .. } => path == &wrapper_path,
                _ => false,
            })
            .ok_or(ProviderError::UnsupportedPolicy)?;
        *step = SetupStep::WriteExecutable {
            path: wrapper_path,
            bytes: wrapper.into_bytes(),
        };
        self.setup.extend([
            SetupStep::CreateDir(hooks.clone()),
            SetupStep::WriteFile {
                path: config,
                bytes: config_text.into_bytes(),
            },
            SetupStep::WriteExecutable {
                path: hooks.join("pre-push"),
                bytes: publishing_hook(&git, &gh, branch).into_bytes(),
            },
            SetupStep::Symlink {
                link: scratch.join("bin/gh"),
                target: gh_path.clone().into(),
            },
        ]);
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

fn publishing_hook(git: &str, gh: &str, branch: &str) -> String {
    let encoded: String = branch
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    let branch = shell_quote(branch);
    format!(
        r#"#!/bin/sh
set -eu
refuse() {{ echo 'lane push refused: only an unprotected lane branch on origin may advance' >&2; exit 1; }}
[ "$1" = origin ] || refuse
branch={branch}
case "$branch" in main|master) refuse ;; esac
[ "$({git} symbolic-ref --quiet HEAD)" = "refs/heads/$branch" ] || refuse
[ "$({git} remote get-url --push origin)" = "$2" ] || refuse
# Bind gh's repository placeholders to this push destination, never upstream.
GH_REPO=$2
export GH_REPO
default=$({gh} repo view "$2" --json defaultBranchRef --jq '.defaultBranchRef.name' 2>/dev/null) || refuse
[ -n "$default" ] && [ "$branch" != "$default" ] || refuse
protected=$({gh} api --paginate 'repos/{{owner}}/{{repo}}/branches?protected=true&per_page=100' --jq '.[].name' 2>/dev/null) || refuse
newline='
'
case "$newline$protected$newline" in *"$newline$branch$newline"*) refuse ;; esac
# Rules can protect a branch before it exists on the remote.
rules=$({gh} api --paginate 'repos/{{owner}}/{{repo}}/rules/branches/{encoded}' --jq '.[].type' 2>/dev/null) || refuse
[ -z "$rules" ] || refuse
while read -r local_ref local_oid remote_ref remote_oid; do
    case "$local_oid" in ''|*[!0]*) ;; *) refuse ;; esac
    case "$local_ref:$remote_ref" in *refs/tags/*) refuse ;; esac
    [ "$remote_ref" = "refs/heads/$branch" ] || refuse
    case "$remote_oid" in
        ''|*[!0]*) {git} merge-base --is-ancestor "$remote_oid" "$local_oid" || refuse ;;
    esac
done
"#
    )
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
                bytes: self.git_wrapper()?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_publishing_pushes_only_its_branch_and_redacts_credentials()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{Access, Adapter, Prompt, codex::Codex};
        use provenant_confinement::redact::Redactor;
        use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
        use std::{fs, os::unix::fs::PermissionsExt, time::Duration};

        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let home = root.join("home");
        let repo = root.join("repo");
        let origin = root.join("origin.git");
        let worktree = root.join("lane");
        let attempt = root.join("attempt");
        let scratch = attempt.join("tmp");
        for path in [&home, &repo, &scratch] {
            fs::create_dir_all(path)?;
        }
        fs::create_dir_all(home.join(".config/gh"))?;
        // This synthetic login stays outside the run files, like the real one.
        let token = format!("ghp_{}", "f".repeat(36));
        fs::write(
            home.join(".config/gh/hosts.yml"),
            format!("username=fixture\npassword={token}\n"),
        )?;
        for (name, value) in [("default", "main\n"), ("protected", ""), ("rules", "")] {
            fs::write(home.join(name), value)?;
        }
        let gh = root.join("gh fixture's executable");
        fs::write(
            &gh,
            br#"#!/bin/sh
[ ! -f "$HOME/offline" ] || exit 1
case "$1" in
    auth) cat "$HOME/.config/gh/hosts.yml" ;;
    repo) cat "$HOME/default" ;;
    api) case "$3" in
        */rules/branches/*) cat "$HOME/rules" ;;
        */branches\?*) cat "$HOME/protected" ;;
        *) exit 1 ;;
    esac ;;
    pr) printf 'fixture pr\n' ;;
    *) exit 1 ;;
esac
"#,
        )?;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700))?;
        let git = provenant_os::git::git_executable()
            .ok_or("Git unavailable")?
            .canonicalize()?;
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
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "base",
            ],
            vec![
                "init",
                "-q",
                "--bare",
                origin.to_str().ok_or("origin path")?,
            ],
            vec![
                "remote",
                "add",
                "origin",
                origin.to_str().ok_or("origin path")?,
            ],
            vec![
                "worktree",
                "add",
                "-q",
                "-b",
                "feat/lane",
                worktree.to_str().ok_or("worktree path")?,
            ],
        ] {
            let output = run(&git, &args, &repo, &env, None)?;
            assert_eq!(
                output.termination,
                Termination::Exit(0),
                "fixture repository: {output:?}"
            );
        }
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
            &route, &plan, &prompt, &worktree, None,
        ))?;
        spec.configure_publishing(&scratch, &git, &gh, "feat/lane")?;
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
        let credential = run(
            &lane_git,
            &["credential", "fill"],
            &worktree,
            &env,
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
        let push = |args: &[&str]| run(&lane_git, args, &worktree, &env, None);
        assert_eq!(
            push(&["push", "origin", "HEAD"])?.termination,
            Termination::Exit(0)
        );
        for args in [
            vec!["push", "origin", "HEAD:main"],
            vec!["push", "origin", "HEAD:other"],
            vec!["push", "origin", "HEAD:refs/tags/v1"],
            vec!["push", "origin", ":refs/heads/feat/lane"],
            vec!["push", origin.to_str().ok_or("origin path")?, "HEAD"],
        ] {
            assert_ne!(
                push(&args)?.termination,
                Termination::Exit(0),
                "refused push: {args:?}"
            );
        }
        for (file, value) in [
            ("default", "feat/lane\n"),
            ("protected", "feat/lane\n"),
            ("rules", "pull_request\n"),
            ("offline", "yes"),
        ] {
            fs::write(home.join(file), value)?;
            // A second commit ensures Git supplies an update to the hook.
            assert_eq!(
                push(&[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "advance"
                ])?
                .termination,
                Termination::Exit(0)
            );
            assert_ne!(
                push(&["push", "origin", "HEAD"])?.termination,
                Termination::Exit(0),
                "policy: {file}"
            );
            if file == "offline" {
                fs::remove_file(home.join(file))?;
            } else {
                fs::write(
                    home.join(file),
                    if file == "default" { "main\n" } else { "" },
                )?;
            }
        }
        assert_eq!(
            push(&["push", "origin", "HEAD"])?.termination,
            Termination::Exit(0)
        );
        // Rewinding a ref with --force is rejected by the stdin OID ancestry check.
        assert_eq!(
            push(&["reset", "--hard", "HEAD~1"])?.termination,
            Termination::Exit(0)
        );
        assert_ne!(
            push(&["push", "--force", "origin", "HEAD"])?.termination,
            Termination::Exit(0)
        );
        for verb in ["create", "comment", "view", "checks"] {
            assert_eq!(
                run(
                    &scratch.join("bin/gh"),
                    &["pr", verb],
                    &worktree,
                    &env,
                    None
                )?
                .termination,
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
