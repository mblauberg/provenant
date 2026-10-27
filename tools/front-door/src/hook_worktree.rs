//! Claude Code `WorktreeCreate` and `WorktreeRemove` hooks. Both hand the work
//! to `provenant worktree create` and `provenant worktree remove`, so a worktree
//! Claude makes for a native agent is registered with its owner from the start,
//! and removal clears what the owner's removal clears. The hooks add no
//! deletion of their own: removal is the owner's non-forcing path, which
//! refuses dirty, live-writer and primary worktrees, and a refusal exits
//! non-zero so Claude keeps the worktree and shows the cause.
use crate::hook_state::Cli;
use provenant_cli::Globals;
use provenant_os::{exec::SystemExecutor, sys::hook_io};
use serde::Deserialize;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const INPUT_CAP: usize = 64 * 1024;
const INPUT_TIME: Duration = Duration::from_secs(2);
/// Under the registered 60s hook timeout.
const WORK_TIME: Duration = Duration::from_secs(55);
const LIST_TIME: Duration = Duration::from_secs(5);
type Error = Box<dyn std::error::Error>;

#[derive(Deserialize)]
struct CreateInput {
    hook_event_name: String,
    name: String,
    cwd: PathBuf,
}
#[derive(Deserialize)]
struct RemoveInput {
    hook_event_name: String,
    worktree_path: PathBuf,
    cwd: Option<PathBuf>,
}

fn fail(hook: &str, error: &dyn std::fmt::Display) -> u8 {
    let _ = writeln!(
        io::stderr().lock(),
        "{hook}: refused: {}",
        error.to_string().chars().take(512).collect::<String>()
    );
    2
}

/// Claude's suggested slug becomes the branch `claude/<slug>`. Anything outside
/// a plain slug is refused, since the name reaches a branch and a directory.
fn branch_for(name: &str) -> Result<String, Error> {
    let plain = !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.contains("..")
        && !name.ends_with('.')
        && !name.ends_with(".lock");
    if !plain {
        return Err("worktree name must be 1-64 letters, digits, '.', '_' or '-'".into());
    }
    Ok(format!("claude/{name}"))
}

/// The absolute path of an entry a `worktree create` or `list` returned.
fn entry_path(entry: &serde_json::Value) -> Option<PathBuf> {
    let path = &entry["path"];
    let relative = path.pointer("/relative/path").and_then(|p| p.as_str())?;
    let root = path["root"].as_str().unwrap_or("/");
    Some(Path::new(root).join(relative.trim_start_matches('/')))
}

// Hook I/O is bounded by real monotonic deadlines.
#[allow(clippy::disallowed_methods)]
pub fn create(globals: &Globals) -> u8 {
    let result = (|| -> Result<PathBuf, Error> {
        let input = hook_io::read_stdin(INPUT_CAP, Instant::now() + INPUT_TIME)?;
        let input: CreateInput = serde_json::from_slice(&input)?;
        if input.hook_event_name != "WorktreeCreate" || !input.cwd.is_absolute() {
            return Err("invalid hook input".into());
        }
        let branch = branch_for(&input.name)?;
        let deadline = Instant::now() + WORK_TIME;
        let executor = SystemExecutor::new(1)?;
        let cli = Cli::new(globals, &input.cwd, &executor).ok_or("cannot find this executable")?;
        let created = cli.data(&["worktree", "create", &branch], WORK_TIME, deadline);
        let path = match created {
            Ok(data) => entry_path(&data).ok_or("worktree create returned no path")?,
            Err(cause) => {
                // A name Claude reuses finds its own earlier worktree, unless a
                // writer holds it.
                let list = cli
                    .data(&["worktree", "list"], LIST_TIME, deadline)
                    .map_err(|_| cause.clone())?;
                let wanted = format!("refs/heads/{branch}");
                list.as_array()
                    .and_then(|entries| {
                        entries
                            .iter()
                            .find(|e| e["branch"] == wanted.as_str() && e["writer"] != "live")
                    })
                    .and_then(entry_path)
                    .ok_or(cause)?
            }
        };
        if !path.is_absolute() || !path.is_dir() {
            return Err("worktree path unavailable after creation".into());
        }
        Ok(path)
    })();
    match result {
        Ok(path) => {
            // Claude reads only the path from stdout.
            let mut line = path.into_os_string().into_encoded_bytes();
            line.push(b'\n');
            #[allow(clippy::disallowed_methods)]
            let deadline = Instant::now() + INPUT_TIME;
            match hook_io::write_stdout(&line, deadline) {
                Ok(()) => 0,
                Err(error) => fail("worktree-create", &error),
            }
        }
        Err(error) => fail("worktree-create", &error),
    }
}

// Hook I/O is bounded by real monotonic deadlines.
#[allow(clippy::disallowed_methods)]
pub fn remove(globals: &Globals) -> u8 {
    let result = (|| -> Result<(), Error> {
        let input = hook_io::read_stdin(INPUT_CAP, Instant::now() + INPUT_TIME)?;
        let input: RemoveInput = serde_json::from_slice(&input)?;
        if input.hook_event_name != "WorktreeRemove" || !input.worktree_path.is_absolute() {
            return Err("invalid hook input".into());
        }
        let Ok(target) = std::fs::canonicalize(&input.worktree_path) else {
            // Nothing is left at the path, so there is nothing to remove.
            return Ok(());
        };
        // Run from a directory that survives the removal: the session's own
        // directory unless it is inside the target, else the target's parent.
        let cwd = input
            .cwd
            .filter(|cwd| cwd.is_absolute())
            .and_then(|cwd| std::fs::canonicalize(cwd).ok())
            .filter(|cwd| !cwd.starts_with(&target))
            .or_else(|| target.parent().map(Path::to_path_buf))
            .ok_or("no directory to run from")?;
        let deadline = Instant::now() + WORK_TIME;
        let executor = SystemExecutor::new(1)?;
        let cli = Cli::new(globals, &cwd, &executor).ok_or("cannot find this executable")?;
        let list = cli.data(&["worktree", "list"], LIST_TIME, deadline)?;
        // Only a linked worktree this project's owner lists is removable; the
        // first entry is the primary checkout.
        let listed = list
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .skip(1)
            .filter_map(entry_path)
            .any(|path| std::fs::canonicalize(path).is_ok_and(|path| path == target));
        if !listed {
            return Err("not a linked worktree that Provenant lists for this project".into());
        }
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("worktree name is not UTF-8")?;
        let outcome = cli.data(&["worktree", "remove", name], WORK_TIME, deadline)?;
        if target.exists() {
            let reason = outcome["retained"][0]["reason"].as_str().unwrap_or("kept");
            return Err(format!("worktree kept: {reason}").into());
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => fail("worktree-remove", &error),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn only_plain_slugs_become_branches() {
        use super::*;
        assert_eq!(
            branch_for("bright-fox-3").unwrap_or_default(),
            "claude/bright-fox-3"
        );
        for bad in [
            "",
            "-x",
            ".x",
            "a/b",
            "a..b",
            "a b",
            "x.lock",
            "x.",
            "é",
            &"a".repeat(65),
        ] {
            assert!(branch_for(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn entry_paths_join_the_wire_root() {
        use super::*;
        use serde_json::json;
        let entry = json!({"path":{"kind":"path","root":"/","relative":{"encoding":"utf8","path":"p/.worktrees/a"}}});
        assert_eq!(entry_path(&entry), Some(PathBuf::from("/p/.worktrees/a")));
        assert_eq!(entry_path(&json!({"path":"/bare"})), None);
    }
}
