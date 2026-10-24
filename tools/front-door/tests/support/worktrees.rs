#[path = "mod.rs"]
mod support;
use provenant_supervisor::exec::ExecOutcome;
use serde_json::Value;
use std::{fs, path::Path};
use support::{Result, TestRig, code};

// Only the harness=false target compiles these observations. The installed
// binary has no fixture flag, feature or environment override.
pub(crate) struct FreeWorkers;
impl provenant_confinement::worktrees::WorkerObserver for FreeWorkers {
    fn observe_roots(
        &self,
        roots: &[std::path::PathBuf],
    ) -> std::collections::BTreeMap<std::path::PathBuf, provenant_confinement::worktrees::WorkerState>
    {
        roots
            .iter()
            .cloned()
            .map(|root| (root, provenant_confinement::worktrees::WorkerState::Free))
            .collect()
    }
}

pub(crate) fn suite(args: &[std::ffi::OsString]) -> std::io::Result<()> {
    provenant_supervisor::fixture::run_libtest(
        args,
        &[
            "create_list_and_remove_a_managed_worktree",
            "merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees",
            "escaping_branch_is_invalid_and_writes_no_checkout_outside_worktree_root",
        ],
        |name| {
            let result = match name {
                "create_list_and_remove_a_managed_worktree" => {
                    create_list_and_remove_a_managed_worktree()
                }
                "merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees" => {
                    merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees()
                }
                _ => escaping_branch_is_invalid_and_writes_no_checkout_outside_worktree_root(),
            };
            result.map_err(|error| std::io::Error::other(error.to_string()))
        },
    )
}

fn command(rig: &TestRig, args: &[&str]) -> Result<ExecOutcome> {
    let mut argv = vec![
        std::ffi::OsString::from("--state-dir"),
        rig.state.clone().into_os_string(),
    ];
    argv.extend(args.iter().map(std::ffi::OsString::from));
    rig.execute_os(&std::env::current_exe()?, &argv, None)
}

fn git(rig: &TestRig, args: &[&str]) -> Result<String> {
    let output = rig.execute(Path::new("/usr/bin/git"), args, None)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(String::from_utf8(output.stdout.bytes)?.trim().to_owned())
}

fn repository() -> Result<TestRig> {
    let rig = TestRig::new()?;
    git(&rig, &["config", "user.name", "Fixture"])?;
    git(&rig, &["config", "user.email", "fixture@example.invalid"])?;
    git(&rig, &["checkout", "-q", "-b", "main"])?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["commit", "-qm", "Initial"])?;
    Ok(rig)
}

fn json(output: &ExecOutcome) -> Result<Value> {
    Ok(serde_json::from_slice(&output.stdout.bytes)?)
}

fn ok(rig: &TestRig, args: &[&str]) -> Result<Value> {
    let output = command(rig, args)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stdout.bytes)
    );
    let value = json(&output)?;
    assert_eq!(value["status"], "ok");
    Ok(value)
}

fn create_list_and_remove_a_managed_worktree() -> Result {
    let rig = repository()?;
    let base = git(&rig, &["rev-parse", "HEAD"])?;
    let created = ok(
        &rig,
        &[
            "worktree",
            "create",
            "feature/topic",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    assert_eq!(created["command"], "worktree create");
    assert_eq!(created["data"]["head"], base);
    assert!(rig.project.join(".worktrees/feature-topic/.git").is_file());
    let listed = ok(&rig, &["worktree", "list", "--json"])?;
    let trees = listed["data"]
        .as_array()
        .ok_or("worktree list is not an array")?;
    assert_eq!(trees.len(), 2);
    assert!(
        trees
            .iter()
            .any(|tree| tree["branch"] == "refs/heads/feature/topic" && tree["dirty"] == "clean")
    );
    let removed = ok(&rig, &["worktree", "remove", "feature/topic", "--json"])?;
    assert_eq!(
        removed["data"]["removed"]
            .as_array()
            .ok_or("missing removals")?
            .len(),
        1
    );
    assert!(!rig.project.join(".worktrees/feature-topic").exists());
    assert_eq!(
        ok(&rig, &["worktree", "list", "--json"])?["data"]
            .as_array()
            .ok_or("missing list")?
            .len(),
        1
    );
    rig.assert_legacy_unchanged()
}

fn branch_commit(rig: &TestRig, branch: &str) -> Result<std::path::PathBuf> {
    ok(rig, &["worktree", "create", branch, "--json"])?;
    let path = rig.project.join(".worktrees").join(branch);
    fs::write(path.join(branch), branch)?;
    let text = path.to_str().ok_or("non-text fixture path")?;
    git(rig, &["-C", text, "add", branch])?;
    git(rig, &["-C", text, "commit", "-qm", branch])?;
    Ok(path)
}

fn merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees() -> Result {
    for since_scope in [false, true] {
        let rig = repository()?;
        let old = branch_commit(&rig, "old")?;
        git(&rig, &["merge", "--no-ff", "-qm", "Merge old", "old"])?;
        let bound = git(&rig, &["rev-parse", "HEAD"])?;
        let merged = branch_commit(&rig, "merged")?;
        git(&rig, &["merge", "--no-ff", "-qm", "Merge clean", "merged"])?;
        let dirty = branch_commit(&rig, "dirty")?;
        git(&rig, &["merge", "--no-ff", "-qm", "Merge dirty", "dirty"])?;
        fs::write(dirty.join("precious"), "uncommitted work")?;
        let unmerged = branch_commit(&rig, "unmerged")?;
        let mut args = vec![
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "main",
            "--json",
        ];
        if since_scope {
            args.extend(["--since", &bound]);
        } else {
            args.extend([
                "--branch", "merged", "--branch", "dirty", "--branch", "unmerged",
            ]);
        }
        let result = ok(&rig, &args)?;
        assert_eq!(result["data"]["pruned_refs"], serde_json::json!(["merged"]));
        assert!(!merged.exists());
        assert!(old.is_dir());
        assert!(unmerged.is_dir());
        assert_eq!(
            fs::read_to_string(dirty.join("precious"))?,
            "uncommitted work"
        );
        assert_eq!(git(&rig, &["rev-parse", "old"])?.len(), 40);
        assert_eq!(git(&rig, &["rev-parse", "dirty"])?.len(), 40);
        assert_eq!(git(&rig, &["rev-parse", "unmerged"])?.len(), 40);
        let refused = command(&rig, &["worktree", "remove", "dirty", "--json"])?;
        assert_eq!(code(&refused), 1);
        assert_eq!(json(&refused)?["status"], "refused");
        assert!(dirty.join("precious").is_file());
        rig.assert_legacy_unchanged()?;
    }
    Ok(())
}

fn escaping_branch_is_invalid_and_writes_no_checkout_outside_worktree_root() -> Result {
    let rig = repository()?;
    let output = rig.command(&["worktree", "create", "../escape", "--json"])?;
    assert_eq!(code(&output), 2);
    assert_eq!(json(&output)?["status"], "invalid");
    assert!(!rig.project.join("escape").exists());
    assert!(!rig.root.join("escape").exists());
    assert!(!rig.project.join(".worktrees").exists());
    assert_eq!(git(&rig, &["branch", "--format=%(refname:short)"])?, "main");
    rig.assert_legacy_unchanged()
}
