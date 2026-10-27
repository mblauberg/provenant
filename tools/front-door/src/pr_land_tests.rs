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
        "pr view 1668 --json state,baseRefName,headRefName" => {
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
        r#"{"state":"MERGED","baseRefName":"main","headRefName":"feat"}"#,
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
    assert_eq!(status(&value, "branch")?, "refused", "{value}");
    assert!(branch_exists(&landing.rig, "feat")?);
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
        r#"{"state":"OPEN","baseRefName":"main","headRefName":"feat"}"#,
    )?;
    let value = land(&landing)?;
    assert_eq!(value["data"]["merged"], false);
    assert!(landing.worktree.exists());
    Ok(())
}
