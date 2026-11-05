//! Git commands for fixtures, independent of a writer lane's Git binding.
use crate::exec::{ExecOptions, Executor, SystemExecutor, Termination};
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// Whether an inherited variable is a Git setting that a fixture must not see.
fn inherited_git_setting(key: &OsStr) -> bool {
    key.to_string_lossy().starts_with("GIT_")
}

/// Remove all inherited Git settings before a fixture adds its own settings.
pub fn scrub_git_env(command: &mut Command) -> &mut Command {
    for (key, _) in std::env::vars_os() {
        if inherited_git_setting(&key) {
            command.env_remove(key);
        }
    }
    command
}

/// Spawn a specified native Git executable with no inherited Git settings.
#[allow(clippy::disallowed_methods)]
pub fn git_command_at(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    scrub_git_env(&mut command);
    command.envs(fixture_git_settings());
    command
}

/// Native Git program for fixtures: the resolved executable, bypassing the lane's PATH wrapper.
fn git_program() -> OsString {
    crate::git::git_executable()
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| {
            [
                "/opt/homebrew/bin/git",
                "/usr/local/bin/git",
                "/opt/local/bin/git",
                "/Library/Developer/CommandLineTools/usr/bin/git",
                "/usr/bin/git",
                "/bin/git",
            ]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .unwrap_or("/usr/bin/git")
            .into()
        })
}

/// Select native Git directly, bypassing the lane's PATH wrapper.
pub fn git_command() -> Command {
    git_command_at(git_program())
}

/// Git configuration common to direct commands and executor-backed fixtures.
/// Override inherited identity and config injection, and keep hooks and signing off.
pub fn fixture_git_settings() -> [(OsString, OsString); 12] {
    [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_COUNT", "4"),
        ("GIT_CONFIG_KEY_0", "commit.gpgsign"),
        ("GIT_CONFIG_VALUE_0", "false"),
        ("GIT_CONFIG_KEY_1", "core.hooksPath"),
        ("GIT_CONFIG_VALUE_1", "/dev/null"),
        ("GIT_CONFIG_KEY_2", "user.name"),
        ("GIT_CONFIG_VALUE_2", "Fixture"),
        ("GIT_CONFIG_KEY_3", "user.email"),
        ("GIT_CONFIG_VALUE_3", "fixture@example.invalid"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ]
    .map(|(key, value)| (key.into(), value.into()))
}

/// Environment for a fixture that runs Git through the executor: the inherited
/// environment without any `GIT_*` setting, with global and system Git config off,
/// and with the native Git directory first on `PATH`.
/// Pass it as `ExecOptions::env`; an unset `env` inherits the lane's Git binding.
pub fn git_env() -> Vec<(OsString, OsString)> {
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !inherited_git_setting(key))
        .collect();
    // Children that find `git` on PATH, such as cargo's Git CLI, must reach native Git, not a lane wrapper.
    if let Some(dir) =
        crate::git::git_executable().and_then(|git| git.parent().map(Path::to_path_buf))
    {
        // A PATH that cannot be joined is left as inherited, not replaced by an empty one.
        if let Ok(path) = std::env::join_paths(std::iter::once(dir).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ))) {
            env.retain(|(key, _)| key != "PATH");
            env.push(("PATH".into(), path));
        }
    }
    env.extend(fixture_git_settings());
    env
}

/// One executor for every fixture Git command, built once per process. Its capacity
/// covers the test threads that run Git at the same time.
fn fixture_executor() -> io::Result<&'static SystemExecutor> {
    static EXECUTOR: std::sync::OnceLock<Result<SystemExecutor, String>> =
        std::sync::OnceLock::new();
    EXECUTOR
        .get_or_init(|| SystemExecutor::new(64).map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| io::Error::other(error.clone()))
}

/// Run native Git in `cwd` through the executor with [`git_env`].
/// Returns its stdout, or an error unless Git exits zero.
pub fn run_git(cwd: &Path, args: &[&str]) -> io::Result<Vec<u8>> {
    let argv: Vec<OsString> = std::iter::once(git_program())
        .chain(args.iter().map(OsString::from))
        .collect();
    let env = git_env();
    let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(60));
    options.env = Some(&env);
    let outcome = fixture_executor()?
        .execute(options)
        .map_err(io::Error::other)?;
    if outcome.termination != Termination::Exit(0) {
        return Err(io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&outcome.stderr.bytes)
        )));
    }
    Ok(outcome.stdout.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn fixture_commits_ignore_hostile_configuration() -> Result<(), Box<dyn std::error::Error>> {
        use std::{fs, os::unix::fs::PermissionsExt};
        if let Some(root) = std::env::var_os("PROVENANT_HOSTILE_GIT_CHILD") {
            for (index, direct) in [false, true].into_iter().enumerate() {
                let dir = Path::new(&root).join(format!("repo-{index}"));
                fs::create_dir(&dir)?;
                for args in [
                    &["init", "-q"][..],
                    &["commit", "--allow-empty", "-qm", "fixture"][..],
                ] {
                    if direct {
                        assert!(
                            git_command()
                                .current_dir(&dir)
                                .args(args)
                                .status()?
                                .success()
                        );
                    } else {
                        run_git(&dir, args)?;
                    }
                }
                let identity = run_git(&dir, &["log", "-1", "--format=%an <%ae>|%cn <%ce>"])?;
                assert_eq!(
                    String::from_utf8(identity)?.trim(),
                    "Fixture <fixture@example.invalid>|Fixture <fixture@example.invalid>"
                );
                run_git(
                    &dir,
                    &[
                        "-c",
                        "user.name=Explicit",
                        "commit",
                        "--allow-empty",
                        "-qm",
                        "explicit",
                    ],
                )?;
                let name = run_git(&dir, &["log", "-1", "--format=%an"])?;
                assert_eq!(String::from_utf8(name)?.trim(), "Explicit");
            }
            fs::write(Path::new(&root).join("child-ran"), b"")?;
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let hook_dir = temp.path().join("hooks");
        fs::create_dir(&hook_dir)?;
        let hook = hook_dir.join("pre-commit");
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\ntouch '{}'\nexit 1\n",
                temp.path().join("hook-ran").display()
            ),
        )?;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o700))?;
        let config = temp.path().join("config");
        fs::write(
            &config,
            format!(
                "[commit]\n gpgsign = true\n template = {}/missing-template\n[gpg]\n program = /bin/false\n[core]\n hooksPath = {}\n[user]\n name = Hostile\n email = hostile@example.invalid\n",
                temp.path().display(),
                hook_dir.display()
            ),
        )?;
        let status = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "fixture::fixture_git::tests::fixture_commits_ignore_hostile_configuration",
            ])
            .env("PROVENANT_HOSTILE_GIT_CHILD", temp.path())
            .env("HOME", temp.path())
            .env("XDG_CONFIG_HOME", temp.path())
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("GIT_CONFIG_SYSTEM", &config)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
            .env("GIT_CONFIG_VALUE_0", "true")
            .env("GIT_AUTHOR_NAME", "Hostile")
            .env("GIT_COMMITTER_EMAIL", "hostile@example.invalid")
            .status()?;
        assert!(status.success());
        assert!(temp.path().join("child-ran").is_file());
        assert!(!temp.path().join("hook-ran").exists());
        Ok(())
    }

    #[test]
    fn fixture_uses_resolved_git_when_available() {
        if let Some(git) = crate::git::git_executable() {
            assert_eq!(super::git_command().get_program(), git.as_os_str());
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn executor_git_ignores_poisoned_lane_environment() -> Result<(), Box<dyn std::error::Error>> {
        use std::{fs, os::unix::fs::PermissionsExt};

        fn lane_snapshot(lane: &Path) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
            let rev = git_command()
                .current_dir(lane)
                .args(["rev-parse", "HEAD"])
                .output()?;
            let refs = git_command()
                .current_dir(lane)
                .args(["for-each-ref", "--format=%(refname) %(objectname)"])
                .output()?;
            Ok(vec![
                rev.stdout,
                refs.stdout,
                fs::read(lane.join(".git/index"))?,
                fs::read(lane.join(".git/config"))?,
            ])
        }

        if let Some(root) = std::env::var_os("PROVENANT_FIXTURE_EXECUTOR_CHILD") {
            let fixture = Path::new(&root).join("fixture");
            fs::create_dir(&fixture)?;
            run_git(&fixture, &["init", "-q"])?;
            run_git(&fixture, &["commit", "--allow-empty", "-qm", "fixture"])?;
            assert!(fixture.join(".git/HEAD").is_file());
            fs::write(Path::new(&root).join("child-ran"), b"")?;
            return Ok(());
        }

        let temp = tempfile::tempdir()?;
        let lane = temp.path().join("lane");
        let shim = temp.path().join("shim");
        fs::create_dir(&lane)?;
        fs::create_dir(&shim)?;
        assert!(
            git_command()
                .current_dir(&lane)
                .args(["init", "-q"])
                .status()?
                .success()
        );
        assert!(
            git_command()
                .current_dir(&lane)
                .args([
                    "-c",
                    "user.name=Lane",
                    "-c",
                    "user.email=lane@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    "lane",
                ])
                .status()?
                .success()
        );
        let before = lane_snapshot(&lane)?;
        fs::write(shim.join("git"), "#!/bin/sh\nexit 99\n")?;
        fs::set_permissions(shim.join("git"), fs::Permissions::from_mode(0o700))?;
        let status = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "fixture::fixture_git::tests::executor_git_ignores_poisoned_lane_environment",
            ])
            .env("PROVENANT_FIXTURE_EXECUTOR_CHILD", temp.path())
            .env("PATH", &shim)
            .env("GIT_DIR", lane.join(".git"))
            .env("GIT_WORK_TREE", &lane)
            .env("GIT_COMMON_DIR", lane.join(".git"))
            .env("GIT_INDEX_FILE", lane.join(".git/index"))
            .env("GIT_OBJECT_DIRECTORY", lane.join(".git/objects"))
            .env(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                lane.join(".git/objects"),
            )
            .env("GIT_NAMESPACE", "poison")
            .env("GIT_CEILING_DIRECTORIES", temp.path())
            .status()?;
        assert!(status.success());
        // A filter that matched no test also exits zero; the child must have run its body.
        assert!(
            temp.path().join("child-ran").is_file(),
            "the child test did not run"
        );
        assert!(temp.path().join("fixture/.git/HEAD").is_file());
        assert_eq!(before, lane_snapshot(&lane)?);
        Ok(())
    }
}
