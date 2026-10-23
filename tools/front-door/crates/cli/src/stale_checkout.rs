//! Local, read-only stale-checkout inspection with an explicit Git execution boundary.
use crate::CommandOutput;
use std::path::Path;

pub const FETCH_MAX_AGE_SECONDS: f64 = 3.0 * 86400.0;
pub const CHILD_MARKER: (&str, &str) = ("PROVENANT_STALE_CHECKED", "1");

/// Absence means the local query failed or cannot be established. No fetch is needed.
#[derive(Debug, Default)]
pub struct Snapshot {
    pub primary_checkout: bool,
    pub fetch_modified_at: Option<f64>,
    pub default_branch: Option<String>,
    pub branch: Branch,
    pub counts: Option<(u64, u64)>,
}

#[derive(Debug, Default)]
pub enum Branch {
    #[default]
    Unknown,
    Detached,
    Named(String),
}

pub fn warnings(root: &Path, snapshot: &Snapshot, now: f64) -> Vec<String> {
    if !snapshot.primary_checkout {
        return Vec::new();
    }
    let root = root.display();
    let mut lines = Vec::new();
    if let Some(modified) = snapshot.fetch_modified_at {
        let age = now - modified;
        if age > FETCH_MAX_AGE_SECONDS {
            lines.push(format!(
                "warning: origin last fetched {:.0}d ago; run: git -C {root} fetch",
                (age / 86400.0).floor()
            ));
        }
    }
    let default = snapshot.default_branch.as_deref().unwrap_or("main");
    let where_at = match &snapshot.branch {
        Branch::Unknown => None,
        Branch::Detached => Some("on a detached HEAD".to_owned()),
        Branch::Named(name) if name == default => None,
        Branch::Named(name) => Some(format!("on {name}")),
    };
    if let Some(where_at) = where_at {
        lines.push(format!("warning: product checkout is {where_at}, not {default}; run: git -C {root} switch {default}"));
    }
    match snapshot.counts {
        Some((ahead, behind)) if ahead > 0 && behind > 0 => lines.push(format!("warning: product checkout diverged from origin/{default} ({ahead} ahead, {behind} behind); run: git -C {root} rebase origin/{default} (or merge)")),
        Some((_, behind)) if behind > 0 => lines.push(format!("warning: product checkout is {behind} commits behind origin/{default}; run: git -C {root} pull --ff-only")),
        _ => {}
    }
    lines
}

/// One owner per process tree. The binary edge forwards CHILD_MARKER to children;
/// no library mutates the environment (unsafe outside supervisor is forbidden).
#[derive(Debug)]
pub struct Advisory {
    checked: bool,
}

impl Advisory {
    pub fn new(inherited_marker: Option<&str>) -> Self {
        Self {
            checked: inherited_marker == Some("1"),
        }
    }

    pub fn render_once(&mut self, root: &Path, snapshot: &Snapshot, now: f64) -> CommandOutput {
        if self.checked {
            return CommandOutput::default();
        }
        self.checked = true;
        let lines = warnings(root, snapshot, now);
        CommandOutput {
            stderr: if lines.is_empty() {
                String::new()
            } else {
                lines.join("\n") + "\n"
            },
            ..Default::default()
        }
    }
}

/// Utility execution request. The process owner supplies the bounded runner;
/// this module neither fetches nor inherits repository redirection variables.
#[derive(Debug)]
pub struct GitQuery<'a> {
    pub root: &'a Path,
    pub arguments: &'a [&'a str],
    pub environment: [(&'static str, &'static str); 4],
    pub timeout: std::time::Duration,
    pub stdin_null: bool,
}

pub const TRUSTED_PATH: &str = "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/opt/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// A failed spawn, timeout, nonzero exit, import or text decode becomes None.
/// The returned string is strictly decoded stdout of a successful local query.
pub trait LocalGit {
    fn read(&mut self, query: GitQuery<'_>) -> Option<String>;
}
impl<F> LocalGit for F
where
    F: FnMut(GitQuery<'_>) -> Option<String>,
{
    fn read(&mut self, query: GitQuery<'_>) -> Option<String> {
        self(query)
    }
}

fn query(git: &mut impl LocalGit, root: &Path, arguments: &[&str]) -> Option<Vec<String>> {
    git.read(GitQuery {
        root,
        arguments,
        environment: [
            ("PATH", TRUSTED_PATH),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ],
        timeout: std::time::Duration::from_secs(1),
        stdin_null: true,
    })
    .map(|text| {
        crate::pending::python_splitlines(&text)
            .into_iter()
            .map(str::to_owned)
            .collect()
    })
}

/// Collect only local observations. Git failures are advisory silence, including
/// missing/invalid FETCH_HEAD, detached HEADs and unresolvable remote refs.
pub fn inspect(root: &Path, git: &mut impl LocalGit) -> Snapshot {
    let Some(paths) = query(
        git,
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
            "--git-path",
            "FETCH_HEAD",
        ],
    ) else {
        return Snapshot::default();
    };
    if paths.len() != 3 || paths[0] != paths[1] {
        return Snapshot::default();
    }
    let fetch_modified_at = std::fs::metadata(&paths[2])
        .and_then(|m| m.modified())
        .ok()
        .map(|time| match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs_f64(),
            Err(e) => -e.duration().as_secs_f64(),
        });
    let default = query(
        git,
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .and_then(|lines| {
        lines
            .first()
            .map(|s| s.strip_prefix("origin/").unwrap_or(s).to_owned())
    })
    .unwrap_or_else(|| "main".into());
    let branch = match query(git, root, &["branch", "--show-current"]) {
        None => Branch::Unknown,
        Some(lines) if lines == [default.clone()] => Branch::Named(default.clone()),
        Some(lines) => match lines.first() {
            Some(name) => Branch::Named(name.clone()),
            None => Branch::Detached,
        },
    };
    let counts = query(
        git,
        root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...origin/{default}"),
        ],
    )
    .and_then(|lines| {
        lines.first().and_then(|line| {
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.len() != 2 {
                return None;
            }
            Some((parts[0].parse().ok()?, parts[1].parse().ok()?))
        })
    });
    Snapshot {
        primary_checkout: true,
        fetch_modified_at,
        default_branch: Some(default),
        branch,
        counts,
    }
}

pub fn check_stale_checkout(root: &Path, now: f64, git: &mut impl LocalGit) -> Vec<String> {
    warnings(root, &inspect(root, git), now)
}

impl Advisory {
    /// The binary forwards CHILD_MARKER to its children when this is first called.
    pub fn inspect_once(
        &mut self,
        root: &Path,
        now: f64,
        git: &mut impl LocalGit,
    ) -> CommandOutput {
        if self.checked {
            return CommandOutput::default();
        }
        let state = inspect(root, git);
        self.render_once(root, &state, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Snapshot {
        Snapshot {
            primary_checkout: true,
            branch: Branch::Named("main".into()),
            counts: Some((0, 0)),
            ..Default::default()
        }
    }

    #[test]
    fn healthy_ahead_only_and_linked_checkouts_are_silent() {
        let mut state = snapshot();
        assert!(warnings(Path::new("/p"), &state, 0.0).is_empty());
        state.counts = Some((2, 0));
        assert!(warnings(Path::new("/p"), &state, 0.0).is_empty());
        state.primary_checkout = false;
        state.counts = Some((0, 2));
        assert!(warnings(Path::new("/p"), &state, 0.0).is_empty());
        assert!(warnings(Path::new("/p"), &Snapshot::default(), 0.0).is_empty());
    }

    #[test]
    fn fetch_age_has_strict_threshold_and_explicit_clock() {
        let mut state = snapshot();
        state.fetch_modified_at = Some(0.0);
        assert!(warnings(Path::new("/p"), &state, FETCH_MAX_AGE_SECONDS).is_empty());
        assert_eq!(
            warnings(Path::new("/p"), &state, 5.0 * 86400.0),
            ["warning: origin last fetched 5d ago; run: git -C /p fetch"]
        );
        assert!(warnings(Path::new("/p"), &state, -1.0).is_empty());
    }

    #[test]
    fn custom_default_detached_and_divergence_text() {
        let mut state = snapshot();
        state.default_branch = Some("trunk".into());
        state.branch = Branch::Detached;
        state.counts = Some((1, 2));
        assert_eq!(
            warnings(Path::new("/p"), &state, 0.0),
            [
                "warning: product checkout is on a detached HEAD, not trunk; run: git -C /p switch trunk",
                "warning: product checkout diverged from origin/trunk (1 ahead, 2 behind); run: git -C /p rebase origin/trunk (or merge)"
            ]
        );
        state.branch = Branch::Named("trunk".into());
        state.counts = Some((0, 2));
        assert_eq!(
            warnings(Path::new("/p"), &state, 0.0),
            [
                "warning: product checkout is 2 commits behind origin/trunk; run: git -C /p pull --ff-only"
            ]
        );
    }

    #[test]
    fn warning_never_blocks_and_deduplicates_even_unknown_state() {
        let mut state = snapshot();
        state.counts = Some((0, 1));
        for marker in [None, Some(""), Some("0")] {
            let mut advisory = Advisory::new(marker);
            let output = advisory.render_once(Path::new("/p"), &state, 0.0);
            assert_eq!(output.exit_code, 0);
            assert!(output.stderr.ends_with('\n'));
            assert_eq!(
                advisory.render_once(Path::new("/p"), &state, 0.0),
                CommandOutput::default()
            );
        }
        assert_eq!(
            Advisory::new(Some("1")).render_once(Path::new("/p"), &state, 0.0),
            CommandOutput::default()
        );
        let mut advisory = Advisory::new(None);
        advisory.render_once(Path::new("/p"), &Snapshot::default(), 0.0);
        assert_eq!(
            advisory.render_once(Path::new("/p"), &state, 0.0),
            CommandOutput::default()
        );
    }
    #[test]
    fn advisory_matches_python_on_a_real_local_checkout() {
        use std::{
            fs::{self, File, FileTimes},
            process::Command,
            time::{Duration, UNIX_EPOCH},
        };
        let dir = tempfile::tempdir().unwrap();
        let env = crate::git_evidence::sanitized_environment(&std::env::vars_os().collect());
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                ])
                .args(args)
                .env_clear()
                .envs(&env)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q", "-b", "feature"]);
        fs::write(dir.path().join("file"), "first").unwrap();
        git(&["add", "file"]);
        git(&["commit", "-qm", "first"]);
        let first = git(&["rev-parse", "HEAD"]);
        fs::write(dir.path().join("file"), "second").unwrap();
        git(&["add", "file"]);
        git(&["commit", "-qm", "second"]);
        git(&["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ]);
        git(&["update-ref", "refs/heads/feature", &first]);
        let fetch = dir.path().join(".git/FETCH_HEAD");
        fs::write(&fetch, "").unwrap();
        File::options()
            .write(true)
            .open(fetch)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(568000)))
            .unwrap();
        let root = dir.path().canonicalize().unwrap();
        let input = serde_json::json!({"root":root, "now":1000000.0});
        let Some(expected): Option<Vec<String>> = crate::oracle::python(
            "import json,sys; from pathlib import Path; sys.path.insert(0,'scripts'); from lib.stale_checkout import check_stale_checkout; x=json.load(sys.stdin); print(json.dumps(check_stale_checkout(Path(x['root']),now=x['now'])))",
            &input,
        ) else {
            return;
        };
        let mut local_git = |query: GitQuery<'_>| {
            assert_eq!(query.timeout, std::time::Duration::from_secs(1));
            assert!(query.stdin_null);
            let mut args = vec![
                std::ffi::OsString::from("-C"),
                query.root.as_os_str().to_owned(),
            ];
            args.extend(query.arguments.iter().map(std::ffi::OsString::from));
            let result = crate::oracle::utility("git".as_ref(), &args, &query.environment).unwrap();
            result
                .status
                .success()
                .then(|| String::from_utf8(result.stdout).ok())
                .flatten()
        };
        assert_eq!(
            check_stale_checkout(&root, 1000000.0, &mut local_git),
            expected
        );
        // A linked tree and nonrepository are silent, using the actual collector.
        let linked = dir.path().join("linked");
        git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ]);
        assert!(check_stale_checkout(&linked, 1000000.0, &mut local_git).is_empty());
        let absent = dir.path().join("absent");
        assert!(check_stale_checkout(&absent, 1000000.0, &mut local_git).is_empty());
        assert_eq!(expected.len(), 3);
    }
}
