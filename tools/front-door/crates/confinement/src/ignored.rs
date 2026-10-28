//! Directories git ignores wholesale in a worktree, such as an in-tree Cargo
//! `target/` or `node_modules/`. The writable-tree census skips them: build
//! tools hard-link inside their output, which must not block a dispatch.
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_LIMIT: usize = 1024 * 1024;

/// Absolute paths of the directories under `root` that git ignores entirely,
/// found by the host's Git. No Git, or any failure, yields none, so the census
/// then treats every directory as ordinary and refuses on the first hard link.
pub(crate) fn ignored_directories(root: &Path, git_dir: &Path) -> BTreeSet<PathBuf> {
    provenant_os::git::git_executable()
        .and_then(|git| ignored_directories_with(&git, root, git_dir))
        .unwrap_or_default()
}

fn ignored_directories_with(git: &Path, root: &Path, git_dir: &Path) -> Option<BTreeSet<PathBuf>> {
    // Repository configuration must not run code: no hooks, no fsmonitor.
    let mut argv: Vec<OsString> = vec![git.into()];
    argv.extend(
        [
            "--no-pager",
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "--git-dir",
        ]
        .map(OsString::from),
    );
    argv.extend([git_dir.into(), "--work-tree".into(), root.into()]);
    argv.extend(
        [
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ]
        .map(OsString::from),
    );
    let env: Vec<(OsString, OsString)> = [
        ("HOME", "/nonexistent"),
        ("XDG_CONFIG_HOME", "/nonexistent"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
    ]
    .map(|(key, value)| (key.into(), value.into()))
    .into();
    let mut options = ExecOptions::new(&argv, root, TIMEOUT);
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture {
        limit: OUTPUT_LIMIT,
    };
    options.stderr = StreamPolicy::Discard;
    options.stop_on_output_limit = true;
    let outcome = SystemExecutor::new(1).ok()?.execute(options).ok()?;
    if outcome.termination != Termination::Exit(0)
        || outcome.timed_out
        || outcome.drain_truncated
        || outcome.stdout.truncated
        || outcome.stdout.read_failed
    {
        return None;
    }
    // A trailing `/` marks a directory that is ignored as a whole.
    Some(
        outcome
            .stdout
            .bytes
            .split(|byte| *byte == 0)
            .filter_map(|entry| entry.strip_suffix(b"/"))
            .filter(|name| !name.is_empty())
            .map(|name| root.join(String::from_utf8_lossy(name).as_ref()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git() -> PathBuf {
        provenant_os::git::git_executable().expect("Git fixture is unavailable")
    }
    fn run(root: &Path, args: &[&str]) {
        let env = crate::test_support::fixture_env(Path::new("/nonexistent"), root);
        let executor = provenant_os::exec::SystemExecutor::new(1).unwrap();
        crate::test_support::run_fixture_git(&executor, &git(), root, &env, args).unwrap();
    }

    #[test]
    fn wholly_ignored_directories_are_listed_and_others_are_not() {
        let temp = crate::test_support::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        run(&root, &["init", "--quiet"]);
        fs::write(root.join(".gitignore"), "target/\nnode_modules\n*.log\n").unwrap();
        for directory in [
            "target/debug",
            "node_modules/pkg",
            "src",
            "docs/target",
            "kept/build",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
            fs::write(root.join(directory).join("file"), b"x").unwrap();
        }
        fs::write(root.join("trace.log"), b"x").unwrap();
        fs::write(root.join("docs/readme"), b"x").unwrap();
        // A tracked file inside keeps its directory out of the list.
        fs::write(root.join("kept/build/.gitignore"), "*\n!tracked\n").unwrap();
        fs::write(root.join("kept/build/tracked"), b"x").unwrap();
        run(&root, &["add", "-f", ".gitignore", "kept/build/tracked"]);

        let ignored = ignored_directories_with(&git(), &root, &root.join(".git")).unwrap();
        assert_eq!(
            ignored,
            BTreeSet::from(["target", "node_modules", "docs/target"].map(|name| root.join(name)))
        );
    }

    #[test]
    fn an_unreadable_repository_lists_nothing() {
        let temp = crate::test_support::tempdir().unwrap();
        assert!(ignored_directories_with(&git(), temp.path(), &temp.path().join(".git")).is_none());
        assert!(ignored_directories(temp.path(), &temp.path().join(".git")).is_empty());
    }
}
