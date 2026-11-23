//! Claude Code `WorktreeCreate` and `WorktreeRemove` hooks. Both hand the work
//! to `provenant worktree create` and `provenant worktree remove`, so a worktree
//! Claude makes for a native agent is registered with its owner from the start,
//! and removal clears what the owner's removal clears. The hooks add no
//! deletion of their own: removal is the owner's non-forcing path, which
//! refuses dirty, live-writer and primary worktrees so Claude keeps the tree.
use crate::hook_state::Cli;
use provenant_cli::Globals;
use provenant_os::{exec::SystemExecutor, sys::hook_io};
use serde::{Deserialize, Serialize};
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

/// Provenant's state root for this hook, as the other commands resolve it.
fn state_root(globals: &Globals, cwd: &Path) -> Result<PathBuf, Error> {
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::At(cwd),
        crate::roots::Product::Optional,
    )?;
    Ok(resolved.roots.state.as_path().to_path_buf())
}

fn fail(hook: &str, error: &dyn std::fmt::Display) -> u8 {
    let _ = writeln!(
        io::stderr().lock(),
        "{hook}: refused: {}",
        error.to_string().chars().take(512).collect::<String>()
    );
    2
}

fn abstain(hook: &str, reason: &dyn std::fmt::Display) -> u8 {
    let _ = writeln!(
        io::stderr().lock(),
        "{hook}: skipped: {}",
        reason.to_string().chars().take(512).collect::<String>()
    );
    0
}

/// Report an adoption that has actually happened, naming what was different so
/// the owner can see why the tree was taken over. Callers emit it only after
/// the tree is known clean and idle (create) or is confirmed removed (remove);
/// a tree the hook only hoped to adopt is never claimed as adopted. Dirty trees
/// and trees with a live writer stay refused by `fail`.
fn warn_adopt(hook: &str, reason: &dyn std::fmt::Display) {
    let _ = writeln!(
        io::stderr().lock(),
        "{hook}: warning: adopting clean worktree: {}",
        reason.to_string().chars().take(512).collect::<String>()
    );
}

/// `worktree create` sets `reused` when it adopted the existing checkout for
/// the branch instead of adding one. A fresh creation omits it, so only a reused
/// tree can be an adoption at the hook boundary.
fn create_reused(data: &serde_json::Value) -> bool {
    data["reused"].as_bool() == Some(true)
}

/// Claude's suggested slug becomes the branch `claude/<slug>`. The name reaches
/// a branch and a directory, so anything outside a plain slug (1-64 letters,
/// digits, '.', '_' or '-') is rewritten to one, never refused. A plain slug is
/// kept as given.
fn branch_for(name: &str) -> String {
    let mut slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    while slug.contains("..") {
        slug = slug.replace("..", "-");
    }
    let mut slug: String = slug
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .chars()
        .take(64)
        .collect();
    while slug.ends_with('.') || slug.ends_with(".lock") {
        slug.truncate(slug.len() - if slug.ends_with('.') { 1 } else { 5 });
    }
    if slug.is_empty() {
        slug.push_str("worktree");
    }
    format!("claude/{slug}")
}

/// The absolute path of an entry a `worktree create` or `list` returned.
fn entry_path(entry: &serde_json::Value) -> Option<PathBuf> {
    let path = &entry["path"];
    let relative = path.pointer("/relative/path").and_then(|p| p.as_str())?;
    let root = path["root"].as_str().unwrap_or("/");
    Some(Path::new(root).join(relative.trim_start_matches('/')))
}

/// The worktree already holding `branch` that this hook can reuse. A clean,
/// idle worktree is adopted with a warning even when it is off the hook's usual
/// path or carries no ownership record: those are preferences, not boundaries.
/// The primary checkout, a live or held writer, uncommitted files and an unknown
/// path or layout stay refused. `Err(None)` means no worktree holds the branch,
/// so the create failure stands alone.
fn reusable(
    state: &Path,
    list: &serde_json::Value,
    branch: &str,
) -> Result<PathBuf, Option<String>> {
    let wanted = format!("refs/heads/{branch}");
    let entries = list.as_array().map_or(&[][..], Vec::as_slice);
    let Some(holder) = entries.iter().find(|e| e["branch"] == wanted.as_str()) else {
        return Err(None);
    };
    let refuse = |why: &str| Err(Some(why.to_owned()));
    let (Some(primary), Some(path)) = (entries.first().and_then(entry_path), entry_path(holder))
    else {
        return refuse("its path is unknown");
    };
    if path == primary {
        return refuse("the branch is checked out in the primary checkout");
    }
    let expected = match provenant_confinement::worktrees::managed_writer_relative_path(
        &primary, state, branch,
    ) {
        Ok(relative) => primary.join(relative),
        Err(_) => return refuse("the repository worktree layout is unavailable"),
    };
    // `unknown` is common (any process the owner cannot inspect makes every
    // writer unknown), so only an actually occupied writer refuses here.
    // `Observed` renders as `live` and `Held` (an active writer lease) as
    // `busy`; both mean another writer owns the tree, a real boundary.
    // Ownership and path are the hook's own preferences, adopted below.
    if matches!(holder["writer"].as_str(), Some("live" | "busy")) {
        return refuse("a writer is live or holds a lease in it");
    }
    if holder["dirty"] != "clean" {
        return refuse("it has uncommitted or ignored files");
    }
    if path != expected {
        warn_adopt(
            "worktree-create",
            &format!("{path:?} is off the hook's usual path ({expected:?})"),
        );
    }
    let head = holder["head"].as_str().unwrap_or_default();
    if !hook_owned(state, &path, branch, head) {
        warn_adopt(
            "worktree-create",
            &format!("{path:?} carries no ownership record from this hook"),
        );
    }
    Ok(path)
}

/// Name of the ownership record in a worktree's Git admin directory (not in
/// the tree, so it never shows as a change, and Git deletes it with the
/// worktree).
const OWNER_MARKER: &str = "provenant-claude-hook";

/// What one creation by this hook established. Reuse needs the copy in the
/// admin directory and the copy in Provenant's state to agree with each other
/// and with the tree as it stands: the same canonical path, the same branch,
/// the HEAD the hook created it at, and the random token of that creation. A
/// tree removed and recreated by hand, or repurposed, carries none of these.
/// Forgery by the same user is outside the threat model: the aim is never to
/// adopt a tree the hook did not make.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct Ownership {
    path: PathBuf,
    branch: String,
    head: String,
    token: String,
}

/// The admin directory a linked worktree's `.git` file points at.
fn admin_dir(worktree: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(worktree.join(".git")).ok()?;
    let dir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
    (dir.is_absolute() && dir.parent()?.file_name()? == "worktrees").then_some(dir)
}

/// The state-side record for a canonical worktree path.
fn record_path(state: &Path, canonical: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    // Only a file name: the record's own fields are what reuse checks.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    let name = format!("{:016x}", hasher.finish());
    state
        .join("hooks")
        .join("claude-worktrees")
        .join(format!("{name}.json"))
}

fn random_token() -> Result<String, Error> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn mark_owned(state: &Path, worktree: &Path, branch: &str, head: &str) -> Result<(), Error> {
    let canonical = std::fs::canonicalize(worktree)?;
    let admin = admin_dir(&canonical).ok_or("no Git admin directory for the worktree")?;
    let record = Ownership {
        path: canonical.clone(),
        branch: branch.to_owned(),
        head: head.to_owned(),
        token: random_token()?,
    };
    let text = serde_json::to_vec(&record)?;
    let stored = record_path(state, &canonical);
    std::fs::create_dir_all(stored.parent().ok_or("no state directory")?)?;
    std::fs::write(&stored, &text)?;
    std::fs::write(admin.join(OWNER_MARKER), &text)?;
    Ok(())
}

/// The ownership record when both copies exist, agree, carry a token and name
/// this canonical path. Branch and HEAD are the caller's to check.
fn owned_record(state: &Path, worktree: &Path) -> Option<Ownership> {
    let canonical = std::fs::canonicalize(worktree).ok()?;
    let read = |path: PathBuf| -> Option<Ownership> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    };
    let admin = admin_dir(&canonical).and_then(|dir| read(dir.join(OWNER_MARKER)))?;
    let stored = read(record_path(state, &canonical))?;
    (admin == stored && !stored.token.is_empty() && stored.path == canonical).then_some(stored)
}

/// Whether the records describe this tree as it is now.
fn hook_owned(state: &Path, worktree: &Path, branch: &str, head: &str) -> bool {
    owned_record(state, worktree).is_some_and(|r| r.branch == branch && r.head == head)
}

/// Drop the state-side record once the hook has removed the tree.
fn forget(state: &Path, canonical: &Path) {
    let _ = std::fs::remove_file(record_path(state, canonical));
}

// Hook I/O is bounded by real monotonic deadlines.
#[allow(clippy::disallowed_methods)]
pub fn create(globals: &Globals) -> u8 {
    let input = match hook_io::read_stdin(INPUT_CAP, Instant::now() + INPUT_TIME) {
        Ok(bytes) => match serde_json::from_slice::<CreateInput>(&bytes) {
            Ok(input) => input,
            Err(error) => return abstain("worktree-create", &error),
        },
        Err(error) => return abstain("worktree-create", &error),
    };
    if input.hook_event_name != "WorktreeCreate" || !input.cwd.is_absolute() {
        return abstain("worktree-create", &"unrecognised hook input");
    }
    let branch = branch_for(&input.name);
    if branch != format!("claude/{}", input.name) {
        let _ = writeln!(
            io::stderr().lock(),
            "worktree-create: warning: worktree name {:?} is not a plain slug; using branch {branch} (source: hook input name)",
            input.name.chars().take(64).collect::<String>()
        );
    }
    let result = (|| -> Result<PathBuf, Error> {
        let executor = SystemExecutor::new(1)?;
        let cli = Cli::new(globals, &input.cwd, &executor)?;
        let deadline = cli.deadline_after(WORK_TIME);
        let state = state_root(globals, &input.cwd)?;
        let created = cli.data(&["worktree", "create", &branch], WORK_TIME, deadline);
        let path = match created {
            Ok(data) => {
                let path = entry_path(&data).ok_or("worktree create returned no path")?;
                let head = data["head"].as_str().unwrap_or_default();
                // An occupied managed path is now reused rather than refused. A
                // reused tree this hook did not create, or whose record no longer
                // describes it, is adopted: warn before re-marking it. A fresh
                // creation has no record by definition and is not an adoption.
                if create_reused(&data) && !hook_owned(&state, &path, &branch, head) {
                    warn_adopt(
                        "worktree-create",
                        &format!("{path:?} carries no ownership record from this hook"),
                    );
                }
                if let Err(error) = mark_owned(&state, &path, &branch, head) {
                    // Without the mark a later call cannot reuse this tree.
                    let _ = writeln!(io::stderr().lock(), "worktree-create: not marked: {error}");
                }
                path
            }
            Err(cause) => {
                // A name Claude reuses finds its own earlier worktree, and
                // nothing else.
                let list = cli
                    .data(&["worktree", "list"], LIST_TIME, deadline)
                    .map_err(|_| cause.clone())?;
                reusable(&state, &list, &branch).map_err(|reason| match reason {
                    Some(reason) => format!("{cause}; not reusing the existing worktree: {reason}"),
                    None => cause,
                })?
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
    let input = match hook_io::read_stdin(INPUT_CAP, Instant::now() + INPUT_TIME) {
        Ok(bytes) => match serde_json::from_slice::<RemoveInput>(&bytes) {
            Ok(input) => input,
            Err(error) => return abstain("worktree-remove", &error),
        },
        Err(error) => return abstain("worktree-remove", &error),
    };
    if input.hook_event_name != "WorktreeRemove" || !input.worktree_path.is_absolute() {
        return abstain("worktree-remove", &"unrecognised hook input");
    }
    let result = (|| -> Result<(), Error> {
        let target = match std::fs::canonicalize(&input.worktree_path) {
            Ok(target) => target,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("cannot resolve worktree: {:?}", error.kind()).into()),
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
        let executor = SystemExecutor::new(1)?;
        let cli = Cli::new(globals, &cwd, &executor)?;
        let deadline = cli.deadline_after(WORK_TIME);
        let list = cli.data(&["worktree", "list"], LIST_TIME, deadline)?;
        // The target must be a worktree this repository lists. Besides being
        // an identity check, this keeps the basename-based `worktree remove`
        // below from resolving an unlisted path to a different listed tree
        // that happens to share the name. A person-created tree is listed by
        // Git, so adoption still works; ownership alone stays a preference.
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
        // HEAD may have moved on, so only the records are checked. Whether the
        // tree is unowned is decided before removal, but the adoption warning
        // is only emitted once the removal is confirmed: a dirty or held tree
        // is kept, and calling that an adoption would be a lie.
        let state = state_root(globals, &cwd)?;
        let unowned = owned_record(&state, &target).is_none();
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("worktree name is not UTF-8")?;
        let outcome = cli.data(&["worktree", "remove", name], WORK_TIME, deadline)?;
        if !target.exists() {
            forget(&state, &target);
            if unowned {
                warn_adopt(
                    "worktree-remove",
                    &"the worktree was not created by this hook",
                );
            }
        }
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
    fn plain_slugs_are_kept_and_other_names_are_rewritten_to_plain_ones() {
        use super::*;
        assert_eq!(branch_for("bright-fox-3"), "claude/bright-fox-3");
        for (name, expected) in [
            ("", "claude/worktree"),
            ("-x", "claude/x"),
            (".x", "claude/x"),
            ("a/b", "claude/a-b"),
            ("a..b", "claude/a-b"),
            ("a b", "claude/a-b"),
            ("x.lock", "claude/x"),
            ("x.", "claude/x"),
            ("é", "claude/worktree"),
        ] {
            assert_eq!(branch_for(name), expected, "{name:?}");
        }
        assert_eq!(
            branch_for(&"a".repeat(65)),
            format!("claude/{}", "a".repeat(64))
        );
    }

    #[test]
    fn entry_paths_join_the_wire_root() {
        use super::*;
        use serde_json::json;
        let entry = json!({"path":{"kind":"path","root":"/","relative":{"encoding":"utf8","path":"p/.worktrees/a"}}});
        assert_eq!(entry_path(&entry), Some(PathBuf::from("/p/.worktrees/a")));
        assert_eq!(entry_path(&json!({"path":"/bare"})), None);
    }

    #[test]
    fn reusable_adopts_a_clean_idle_tree_and_refuses_dirty_or_occupied() {
        use super::*;
        use serde_json::json;
        fn entry(path: &str, branch: &str, dirty: &str, writer: &str) -> serde_json::Value {
            json!({
                "path": {"kind":"path","root":"/","relative":{"encoding":"utf8","path":path}},
                "branch": format!("refs/heads/{branch}"),
                "dirty": dirty,
                "writer": writer,
                "head": "abc123",
            })
        }
        let branch = "claude/bright-fox-3";
        let primary = entry("primary", "main", "clean", "unknown");
        let state = Path::new("/unused-state");
        let clean = json!([
            primary.clone(),
            entry("adopted", branch, "clean", "unknown")
        ]);
        assert_eq!(
            reusable(state, &clean, branch).unwrap(),
            PathBuf::from("/adopted"),
            "a clean, idle tree is adopted even off-path and unowned"
        );
        // "dirty" is the real WorktreeCondition label in `worktree list`.
        let dirty = json!([
            primary.clone(),
            entry("adopted", branch, "dirty", "unknown")
        ]);
        assert!(
            reusable(state, &dirty, branch).is_err(),
            "dirty tree stays refused"
        );
        // "live" is Observed; "busy" is Held, an active writer lease. Both
        // mean another writer owns the tree and must stay refused.
        for writer in ["live", "busy"] {
            let occupied = json!([primary.clone(), entry("adopted", branch, "clean", writer)]);
            assert!(
                reusable(state, &occupied, branch).is_err(),
                "writer={writer} stays refused"
            );
        }
    }
}
