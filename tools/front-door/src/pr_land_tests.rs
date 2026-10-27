//! `pr land` against a fake `gh` and real Git remotes, worktrees and branches.
use super::worktree_tests::support::{Result, TestRig, code};
use super::worktree_tests::{command, git, json, repository};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};

const NUMBER: &str = "1668";

/// The fake `gh` the driver re-enters as: a passing, unchanged pull request.
pub(crate) fn fake_gh(args: &[String]) -> Result<u8> {
    let call = args.join(" ");
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("gh-calls")?,
        "{call}"
    )?;
    match call.as_str() {
        "pr view 1668 --json headRefOid" => io::stdout().write_all(&fs::read("head.json")?)?,
        "pr view 1668 --json state,baseRefName,headRefName,isCrossRepository" => {
            io::stdout().write_all(&fs::read("view.json")?)?;
        }
        "pr checks 1668 --watch" => {
            fs::write("watched", "")?;
        }
        "pr checks 1668 --json bucket" => {
            if Path::new("checks-fail").exists() {
                return Ok(8);
            }
            io::stdout().write_all(br#"[{"bucket":"pass"}]"#)?;
        }
        value if value.starts_with("pr merge 1668 --merge --match-head-commit ") => {
            fs::write("merge-args", args.join("\n") + "\n")?;
        }
        _ => return Err(format!("unexpected gh arguments: {call}").into()),
    }
    Ok(0)
}

struct Landing {
    rig: TestRig,
    worktree: PathBuf,
    /// The branch head the pull request merged.
    head: String,
    /// `main` before the merge, which the local checkout still has.
    stale: String,
}

fn git_in(rig: &TestRig, dir: &Path, args: &[&str]) -> Result<String> {
    let dir = dir.to_str().ok_or("path")?;
    let mut argv = vec!["-C", dir];
    argv.extend_from_slice(args);
    git(rig, &argv)
}

/// A project whose `feat` worktree was merged into `origin/main` by a merge
/// commit, with the local `main` still before it.
fn landing() -> Result<Landing> {
    let mut rig = repository()?;
    let bin = rig.root.join("bin");
    fs::create_dir(&bin)?;
    symlink(std::env::current_exe()?, bin.join("gh"))?;
    let (_, path) = rig
        .env
        .iter_mut()
        .find(|(key, _)| key == "PATH")
        .ok_or("PATH")?;
    *path = format!("{}:/usr/bin:/bin", bin.display()).into();
    fs::write(
        rig.project.join(".git/info/exclude"),
        "head.json\nview.json\ngh-calls\nwatched\nmerge-args\nchecks-fail\n",
    )?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\n**/target/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["commit", "-qm", "ignore build output"])?;
    let stale = git(&rig, &["rev-parse", "HEAD"])?;
    let origin = rig.root.join("origin.git");
    git(
        &rig,
        &["init", "-q", "--bare", origin.to_str().ok_or("path")?],
    )?;
    git(
        &rig,
        &["remote", "add", "origin", origin.to_str().ok_or("path")?],
    )?;
    git(&rig, &["push", "-q", "origin", "main"])?;
    let created = command(&rig, &["worktree", "create", "feat", "--json"])?;
    assert_eq!(
        code(&created),
        0,
        "{}",
        String::from_utf8_lossy(&created.stdout.bytes)
    );
    let worktree = rig.project.join(".worktrees/feat");
    fs::write(worktree.join("feature.txt"), "feature\n")?;
    git_in(&rig, &worktree, &["add", "feature.txt"])?;
    git_in(&rig, &worktree, &["commit", "-qm", "feature"])?;
    let head = git_in(&rig, &worktree, &["rev-parse", "HEAD"])?;
    git(
        &rig,
        &["merge", "-q", "--no-ff", "-m", "merge feat", "feat"],
    )?;
    git(&rig, &["push", "-q", "origin", "main"])?;
    git(&rig, &["reset", "-q", "--hard", &stale])?;
    fs::write(
        rig.project.join("head.json"),
        format!(r#"{{"headRefOid":"{head}"}}"#),
    )?;
    fs::write(
        rig.project.join("view.json"),
        r#"{"state":"MERGED","baseRefName":"main","headRefName":"feat","isCrossRepository":false}"#,
    )?;
    Ok(Landing {
        rig,
        worktree,
        head,
        stale,
    })
}

fn land(landing: &Landing) -> Result<serde_json::Value> {
    let output = command(
        &landing.rig,
        &["pr", "land", NUMBER, "--head", &landing.head, "--json"],
    )?;
    let value = json(&output)?;
    assert!(
        code(&output) == 0 || value["data"]["merged"] == true,
        "{value}"
    );
    Ok(value)
}

fn status<'a>(value: &'a serde_json::Value, step: &str) -> Result<&'a str> {
    value["data"]["steps"]
        .as_array()
        .and_then(|steps| steps.iter().find(|entry| entry["step"] == step))
        .and_then(|entry| entry["status"].as_str())
        .ok_or_else(|| format!("no {step} step in {value}").into())
}

fn branch_exists(rig: &TestRig, branch: &str) -> Result<bool> {
    Ok(!git(rig, &["branch", "--list", branch])?.is_empty())
}

pub(crate) fn pr_land_merges_then_fetches_fast_forwards_and_prunes() -> Result {
    let landing = landing()?;
    let rig = &landing.rig;
    // Build output the worktree owns, which would otherwise block removal.
    fs::create_dir_all(landing.worktree.join("crate/target/debug"))?;
    fs::write(
        landing.worktree.join("crate/target/debug/artifact"),
        "built",
    )?;
    let output = command(
        rig,
        &["pr", "land", NUMBER, "--head", &landing.head, "--json"],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 0, "{value}");
    assert_eq!(value["data"]["merged"], true);
    for step in ["fetch", "fast_forward", "worktree", "branch"] {
        assert_eq!(status(&value, step)?, "done", "{step}: {value}");
    }
    // The merge went through the existing repository workflow at this head.
    let merge = fs::read_to_string(rig.project.join("merge-args"))?;
    assert!(
        merge.contains(&format!("--match-head-commit\n{}", landing.head)),
        "{merge}"
    );
    assert!(rig.project.join("watched").exists());
    assert_eq!(
        git(rig, &["rev-parse", "main"])?,
        git(rig, &["rev-parse", "origin/main"])?
    );
    assert_ne!(git(rig, &["rev-parse", "main"])?, landing.stale);
    assert!(!landing.worktree.exists());
    assert!(!branch_exists(rig, "feat")?);
    // Landing again finds nothing left to prune, and the text form names each step.
    let text = command(rig, &["pr", "land", NUMBER, "--head", &landing.head])?;
    assert_eq!(code(&text), 0);
    let text = String::from_utf8(text.stdout.bytes)?;
    assert!(
        text.starts_with(&format!("merged #{NUMBER} {}\n", landing.head)),
        "{text}"
    );
    assert!(text.contains("  worktree skipped"), "{text}");
    assert!(text.contains("  branch skipped"), "{text}");
    Ok(())
}

pub(crate) fn pr_land_refuses_each_unsafe_step_and_reports_the_rest() -> Result {
    // A checkout of the base with local changes is not fast-forwarded.
    let landing = landing()?;
    fs::write(landing.rig.project.join("stray.txt"), "mine\n")?;
    let output = command(
        &landing.rig,
        &["pr", "land", NUMBER, "--head", &landing.head, "--json"],
    )?;
    let value = json(&output)?;
    assert_eq!(code(&output), 1, "{value}");
    assert_eq!(value["data"]["merged"], true);
    assert_eq!(status(&value, "fast_forward")?, "refused");
    assert_eq!(status(&value, "worktree")?, "done");
    assert_eq!(
        git(&landing.rig, &["rev-parse", "main"])?,
        landing.stale,
        "a dirty checkout keeps its base"
    );
    assert!(landing.rig.project.join("stray.txt").exists());

    // A worktree with uncommitted work is kept, and so is its branch.
    let landing = self::landing()?;
    fs::write(landing.worktree.join("notes.txt"), "unsaved\n")?;
    fs::create_dir_all(landing.worktree.join("target"))?;
    fs::write(landing.worktree.join("target/artifact"), "built")?;
    let value = land(&landing)?;
    assert_eq!(status(&value, "worktree")?, "refused", "{value}");
    assert_eq!(status(&value, "branch")?, "refused", "{value}");
    assert!(landing.worktree.join("notes.txt").exists());
    assert!(
        landing.worktree.join("target/artifact").exists(),
        "a dirty worktree keeps its build output"
    );
    assert!(branch_exists(&landing.rig, "feat")?);

    // Commits the pull request never merged keep the branch.
    let landing = self::landing()?;
    fs::write(landing.worktree.join("later.txt"), "later\n")?;
    git_in(&landing.rig, &landing.worktree, &["add", "later.txt"])?;
    git_in(&landing.rig, &landing.worktree, &["commit", "-qm", "later"])?;
    let value = land(&landing)?;
    assert_eq!(status(&value, "worktree")?, "refused", "{value}");
    assert_eq!(status(&value, "branch")?, "refused", "{value}");
    assert!(
        landing.worktree.join("later.txt").exists(),
        "the checkout holding unmerged commits survives"
    );
    assert!(branch_exists(&landing.rig, "feat")?);
    Ok(())
}

pub(crate) fn pr_land_leaves_a_local_namesake_of_a_fork_branch_alone() -> Result {
    // The merged pull request came from a fork's `feat`, so the local `feat`
    // branch and its clean, fully merged worktree are somebody else's.
    let landing = landing()?;
    fs::write(
        landing.rig.project.join("view.json"),
        r#"{"state":"MERGED","baseRefName":"main","headRefName":"feat","isCrossRepository":true}"#,
    )?;
    let value = land(&landing)?;
    assert_eq!(value["data"]["merged"], true, "{value}");
    assert_eq!(status(&value, "fast_forward")?, "done", "{value}");
    assert_eq!(status(&value, "worktree")?, "skipped", "{value}");
    assert_eq!(status(&value, "branch")?, "skipped", "{value}");
    assert!(
        value.to_string().contains("another repository"),
        "the report says why: {value}"
    );
    assert!(landing.worktree.exists());
    assert!(branch_exists(&landing.rig, "feat")?);
    Ok(())
}

pub(crate) fn pr_land_leaves_a_reused_branch_name_alone() -> Result {
    // Someone recreated `feat` at the base after the pull request's `feat` was
    // merged. The base contains that tip, but it is not the merged head, so the
    // branch and its clean checkout are not the pull request's to remove.
    let landing = landing()?;
    git_in(
        &landing.rig,
        &landing.worktree,
        &["reset", "-q", "--hard", "origin/main"],
    )?;
    let reused = git(&landing.rig, &["rev-parse", "feat"])?;
    assert_ne!(reused, landing.head);
    let value = land(&landing)?;
    assert_eq!(value["data"]["merged"], true, "{value}");
    assert_eq!(status(&value, "fast_forward")?, "done", "{value}");
    assert_eq!(status(&value, "worktree")?, "skipped", "{value}");
    assert_eq!(status(&value, "branch")?, "skipped", "{value}");
    assert!(
        value
            .to_string()
            .contains("not the merged pull request head"),
        "the report says why: {value}"
    );
    assert!(landing.worktree.exists());
    assert_eq!(git(&landing.rig, &["rev-parse", "feat"])?, reused);
    Ok(())
}

pub(crate) fn pr_land_deletes_an_unregistered_branch_only_at_the_merged_head() -> Result {
    // No worktree is registered for `feat`. After the cleanup check passes,
    // another process moves `feat` to the base, which also contains it. The
    // deletion is pinned to the merged head, so it leaves that branch alone.
    let landing = landing()?;
    unregister(&landing)?;
    let moved = git(&landing.rig, &["rev-parse", "origin/main"])?;
    let (status, detail) = race(&landing, &["branch", "-f", "feat", "origin/main"])?;
    assert_eq!(status, "refused", "{detail}");
    assert_eq!(git(&landing.rig, &["rev-parse", "feat"])?, moved);

    // Undisturbed, the same branch at the merged head is deleted.
    let landing = self::landing()?;
    unregister(&landing)?;
    let (status, detail) = race(&landing, &[])?;
    assert_eq!(status, "done", "{detail}");
    assert!(!branch_exists(&landing.rig, "feat")?);
    Ok(())
}

pub(crate) fn pr_land_keeps_a_branch_held_by_a_paused_rebase() -> Result {
    // The rebase leaves its worktree on a detached HEAD, so only the rebase
    // state records that `feat` is still in use.
    let landing = landing()?;
    let held = hold(&landing, "rebase")?;
    let (status, detail) = race(&landing, &[])?;
    assert_eq!(status, "refused", "{detail}");
    assert!(detail.contains("rebased"), "{detail}");
    assert_eq!(git(&landing.rig, &["rev-parse", "feat"])?, landing.head);

    // Once the rebase is aborted nothing holds the branch, so it is deleted.
    git_in(&landing.rig, &held, &["rebase", "--abort"])?;
    git_in(&landing.rig, &held, &["checkout", "-q", "--detach"])?;
    let (status, detail) = race(&landing, &[])?;
    assert_eq!(status, "done", "{detail}");
    assert!(!branch_exists(&landing.rig, "feat")?);
    Ok(())
}

pub(crate) fn pr_land_keeps_a_branch_held_by_a_bisect() -> Result {
    let landing = landing()?;
    let held = hold(&landing, "bisect")?;
    let (status, detail) = race(&landing, &[])?;
    assert_eq!(status, "refused", "{detail}");
    assert!(detail.contains("bisected"), "{detail}");
    assert_eq!(git(&landing.rig, &["rev-parse", "feat"])?, landing.head);

    git_in(&landing.rig, &held, &["bisect", "reset"])?;
    git_in(&landing.rig, &held, &["checkout", "-q", "--detach"])?;
    let (status, detail) = race(&landing, &[])?;
    assert_eq!(status, "done", "{detail}");
    assert!(!branch_exists(&landing.rig, "feat")?);
    Ok(())
}

/// A second worktree that has `feat` mid-`operation` on a detached HEAD, with
/// the registered `feat` worktree dropped.
fn hold(landing: &Landing, operation: &str) -> Result<PathBuf> {
    unregister(landing)?;
    let held = landing.rig.root.join("held");
    git(
        &landing.rig,
        &[
            "worktree",
            "add",
            "-q",
            held.to_str().ok_or("path")?,
            "feat",
        ],
    )?;
    if operation == "rebase" {
        git_in(
            &landing.rig,
            &held,
            &[
                "-c",
                "sequence.editor=sed -i.bak s/^pick/edit/",
                "rebase",
                "-q",
                "-i",
                "HEAD~1",
            ],
        )?;
    } else {
        let first = git_in(
            &landing.rig,
            &held,
            &["rev-list", "--max-parents=0", "HEAD"],
        )?;
        git_in(&landing.rig, &held, &["bisect", "start", "HEAD", &first])?;
    }
    let listing = git(&landing.rig, &["worktree", "list", "--porcelain"])?;
    assert!(
        !listing.contains("branch refs/heads/feat"),
        "the worktree must be detached: {listing}"
    );
    Ok(held)
}

/// The `branch` step when `between` runs after the cleanup check.
fn race(landing: &Landing, between: &[&str]) -> Result<(String, String)> {
    let mut argv = vec!["--land-race", landing.head.as_str()];
    argv.extend_from_slice(between);
    let argv: Vec<_> = argv.into_iter().map(std::ffi::OsString::from).collect();
    let output = landing
        .rig
        .execute_os(&std::env::current_exe()?, &argv, None)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    let text = String::from_utf8(output.stdout.bytes)?;
    let (status, detail) = text.split_once('\n').ok_or("race output")?;
    Ok((status.to_owned(), detail.trim().to_owned()))
}

/// Drop the `feat` worktree and keep its branch.
fn unregister(landing: &Landing) -> Result {
    let path = landing.worktree.to_str().ok_or("path")?;
    git(&landing.rig, &["worktree", "remove", "--force", path])?;
    Ok(())
}

/// The message of a refused `pr open`, which is a usage error.
fn open_refusal(rig: &TestRig, args: &[&str]) -> Result<String> {
    let mut argv = vec!["pr", "open"];
    argv.extend_from_slice(args);
    argv.push("--json");
    let output = command(rig, &argv)?;
    assert_eq!(
        code(&output),
        2,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let value = json(&output)?;
    Ok(value["error"]["message"]
        .as_str()
        .ok_or("error message")?
        .to_owned())
}

pub(crate) fn pr_open_refuses_what_the_receipts_cannot_support() -> Result {
    let rig = repository()?;
    let summary = rig.root.join("summary.md");
    let summary_arg = summary.to_str().ok_or("path")?;
    fs::write(
        &summary,
        "# feat(cli): add the thing\n\nIt adds the thing.\n",
    )?;
    let unknown = "ab".repeat(16);
    let args = [
        "--from-attempt",
        &unknown,
        "--summary-file",
        summary_arg,
        "--dry-run",
    ];
    // No template, then an empty summary, then an attempt nobody ran.
    assert!(open_refusal(&rig, &args)?.contains("pull request template"));
    fs::create_dir_all(rig.project.join(".github"))?;
    fs::write(
        rig.project.join(".github/pull_request_template.md"),
        include_str!("../../../.github/pull_request_template.md"),
    )?;
    fs::write(&summary, "\n")?;
    assert!(open_refusal(&rig, &args)?.contains("summary file is empty"));
    fs::write(
        &summary,
        "# feat(cli): add the thing\n\nIt adds the thing.\n",
    )?;
    assert!(!open_refusal(&rig, &args)?.is_empty());

    // A lane that never ran cannot back a pull request.
    let dispatched = rig.execute_os(
        &std::env::current_exe()?,
        &[
            "--state-dir".into(),
            rig.state.clone().into_os_string(),
            "dispatch".into(),
            "--model".into(),
            "codex/gpt-6-luna".into(),
            "--operation-id".into(),
            "pr-open-fixture".into(),
            "--prompt-stdin".into(),
            "--json".into(),
        ],
        Some(b"never launched"),
    )?;
    let attempt = json(&dispatched)?["data"]["lanes"][0]["attempt_id"]
        .as_str()
        .ok_or("attempt")?
        .to_owned();
    let message = open_refusal(
        &rig,
        &[
            "--from-attempt",
            &attempt,
            "--summary-file",
            summary_arg,
            "--dry-run",
        ],
    )?;
    assert!(
        message.contains("only a succeeded or review_required attempt"),
        "{message}"
    );
    // A malformed attempt ID is a usage error before anything is read.
    let invalid = command(
        &rig,
        &[
            "pr",
            "open",
            "--from-attempt",
            "nope",
            "--summary-file",
            summary_arg,
        ],
    )?;
    assert_eq!(code(&invalid), 2);
    Ok(())
}

pub(crate) fn pr_land_changes_nothing_when_the_merge_does_not_happen() -> Result {
    for flag in ["checks-fail", "moved"] {
        let landing = landing()?;
        let rig = &landing.rig;
        if flag == "moved" {
            fs::write(
                rig.project.join("head.json"),
                r#"{"headRefOid":"2222222222222222222222222222222222222222"}"#,
            )?;
        } else {
            fs::write(rig.project.join(flag), "")?;
        }
        let output = command(
            rig,
            &["pr", "land", NUMBER, "--head", &landing.head, "--json"],
        )?;
        assert_ne!(code(&output), 0, "{flag}");
        let value = json(&output)?;
        assert_eq!(value["data"]["merged"], false, "{value}");
        assert!(!rig.project.join("merge-args").exists(), "{flag}");
        assert!(landing.worktree.exists(), "{flag}");
        assert!(branch_exists(rig, "feat")?, "{flag}");
        assert_eq!(git(rig, &["rev-parse", "main"])?, landing.stale, "{flag}");
    }
    // The head is required, and a pending merge cleans nothing.
    let landing = landing()?;
    let missing = command(&landing.rig, &["pr", "land", NUMBER])?;
    assert_eq!(code(&missing), 2);
    fs::write(
        landing.rig.project.join("view.json"),
        r#"{"state":"OPEN","baseRefName":"main","headRefName":"feat","isCrossRepository":false}"#,
    )?;
    let value = land(&landing)?;
    assert_eq!(value["data"]["merged"], false);
    assert!(landing.worktree.exists());
    Ok(())
}
