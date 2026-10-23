//! Best-effort warning when the selected product checkout is behind origin.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime};

const FETCH_MAX_AGE: u64 = 3 * 86400;
const TRUSTED_PATH: &str = "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/opt/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
// These queries print a few lines, so their piped stdout cannot fill before exit.
fn output_with_timeout(mut command: Command) -> Option<Output> {
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn git(root: &Path, args: &[&str]) -> Option<Vec<String>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
        .env_clear()
        .env("PATH", TRUSTED_PATH)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = output_with_timeout(command)?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect(),
    )
}

fn warning_lines(root: &Path) -> Vec<String> {
    let Some(paths) = git(
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
        return Vec::new();
    };
    if paths.len() != 3 || paths[0] != paths[1] {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    if let Ok(modified) = std::fs::metadata(&paths[2]).and_then(|m| m.modified()) {
        if let Ok(age) = SystemTime::now().duration_since(modified) {
            if age.as_secs() > FETCH_MAX_AGE {
                warnings.push(format!(
                    "warning: origin last fetched {}d ago; run: git -C {} fetch",
                    age.as_secs() / 86400,
                    root.display()
                ));
            }
        }
    }
    let remote_head = git(
        root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    );
    let default = remote_head
        .as_ref()
        .and_then(|v| v.first())
        .map(|s| s.strip_prefix("origin/").unwrap_or(s))
        .unwrap_or("main");
    let branch = git(root, &["branch", "--show-current"]);
    if let Some(branch) = branch.as_ref() {
        if branch.as_slice() != [default] {
            let where_ = branch
                .first()
                .map_or("on a detached HEAD".to_string(), |b| format!("on {b}"));
            warnings.push(format!("warning: product checkout is {where_}, not {default}; run: git -C {} switch {default}", root.display()));
        }
    }
    let counts = git(
        root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...origin/{default}"),
        ],
    );
    let Some(counts) = counts.and_then(|v| v.first().cloned()) else {
        return warnings;
    };
    let mut counts = counts
        .split_whitespace()
        .filter_map(|s| s.parse::<u64>().ok());
    let (Some(ahead), Some(behind)) = (counts.next(), counts.next()) else {
        return warnings;
    };
    if behind > 0 && ahead > 0 {
        warnings.push(format!("warning: product checkout diverged from origin/{default} ({ahead} ahead, {behind} behind); run: git -C {} rebase origin/{default} (or merge)", root.display()));
    } else if behind > 0 {
        warnings.push(format!("warning: product checkout is {behind} commits behind origin/{default}; run: git -C {} pull --ff-only", root.display()));
    }
    warnings
}

pub fn warn(root: &Path) {
    if std::env::var_os("PROVENANT_STALE_CHECKED").is_some_and(|v| v == "1") {
        return;
    }
    for line in warning_lines(root) {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_query_is_killed_and_reaped() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exec /bin/sleep 10"]);
        let start = Instant::now();
        assert!(output_with_timeout(command).is_none());
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}
