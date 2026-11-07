use super::*;

pub(super) fn create_from_remote_base_reports_ok_and_exists() -> Result {
    let upstream = repository()?;
    let rig = repository()?;
    git(
        &rig,
        &[
            "remote",
            "add",
            "origin",
            upstream.project.to_str().ok_or("upstream path")?,
        ],
    )?;
    git(&rig, &["fetch", "origin"])?;
    git(&rig, &["config", "worktree.useRelativePaths", "true"])?;
    let config = fs::read(rig.project.join(".git/config"))?;
    let created = ok(
        &rig,
        &[
            "worktree",
            "create",
            "feature/status",
            "--base",
            "origin/main",
            "--json",
        ],
    )?;
    assert_eq!(created["status"], "ok");
    assert!(rig.project.join(".worktrees/feature-status/.git").is_file());
    assert_eq!(fs::read(rig.project.join(".git/config"))?, config);
    Ok(())
}

pub(super) fn create_list_and_remove_a_managed_worktree() -> Result {
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
    assert!(created["data"].is_object());
    assert_eq!(created["data"]["path"]["kind"], "path");
    assert_eq!(created["data"]["path"]["relative"]["encoding"], "utf8");
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
            .any(|tree| tree["path"] == created["data"]["path"])
    );
    assert!(
        trees
            .iter()
            .any(|tree| tree["branch"] == "refs/heads/feature/topic"
                && tree["dirty"] == "clean"
                && tree["owner"] == "unowned")
    );
    assert!(trees.iter().any(|tree| tree["owner"] == "primary"));
    let diagnosis = ok(
        &rig,
        &["worktree", "doctor", ".worktrees/feature-topic", "--json"],
    )?;
    assert_eq!(diagnosis["command"], "worktree doctor");
    assert_eq!(diagnosis["data"]["state"], "clean");
    assert_eq!(diagnosis["data"]["path"], created["data"]["path"]);
    assert!(
        diagnosis["data"]["repair"]
            .as_str()
            .is_some_and(|repair| { repair.starts_with("provenant worktree doctor ") })
    );
    let cache = prepare_lane_cache(
        &rig,
        &rig.project.join(".worktrees/feature-topic"),
        "feature/topic",
    )?;
    ok(&rig, &["worktree", "create", "feature/gone", "--json"])?;
    let gone = rig.project.join(".worktrees/feature-gone");
    let orphan = prepare_lane_cache(&rig, &gone, "feature/gone")?;
    git(
        &rig,
        &["worktree", "remove", gone.to_str().ok_or("worktree path")?],
    )?;
    assert!(!gone.exists());
    assert!(orphan.is_dir());
    let removed = ok(&rig, &["worktree", "remove", "feature/topic", "--json"])?;
    assert_eq!(
        removed["data"]["removed"]
            .as_array()
            .ok_or("missing removals")?
            .len(),
        1
    );
    assert!(!rig.project.join(".worktrees/feature-topic").exists());
    assert!(!cache.exists(), "orphaned lane cache remains after removal");
    assert!(
        orphan.is_dir(),
        "removing one worktree must preserve other orphaned caches"
    );
    assert_eq!(
        fs::read(orphan.join("artifact"))?,
        b"lane build output",
        "removing one worktree must preserve other orphaned caches"
    );
    assert_eq!(
        ok(&rig, &["worktree", "list", "--json"])?["data"]
            .as_array()
            .ok_or("missing list")?
            .len(),
        1
    );
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(!orphan.exists(), "gc must still sweep orphaned caches");
    rig.assert_legacy_unchanged()
}

pub(super) fn existing_branch_with_base_names_the_repair_in_json_and_human_output() -> Result {
    let rig = repository()?;
    git(&rig, &["branch", "existing"])?;
    git(&rig, &["commit", "--allow-empty", "-qm", "advance"])?;
    let json_output = command(
        &rig,
        &["worktree", "create", "existing", "--base", "HEAD", "--json"],
    )?;
    assert_ne!(code(&json_output), 0);
    let value = json(&json_output)?;
    let message = value["error"]["message"].as_str().ok_or("error message")?;
    assert!(
        message.contains("branch existing at")
            && message.contains("does not contain base")
            && message.contains("omit --base"),
        "{value}"
    );

    let human_output = command(&rig, &["worktree", "create", "existing", "--base", "HEAD"])?;
    assert_ne!(code(&human_output), 0);
    let message = String::from_utf8_lossy(&human_output.stderr.bytes);
    assert!(
        message.contains("branch existing at")
            && message.contains("does not contain base")
            && message.contains("omit --base"),
        "{message}"
    );
    Ok(())
}

pub(super) fn merged_pruning_keeps_dirty_unmerged_and_out_of_scope_worktrees() -> Result {
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

pub(super) fn remove_by_name_deletes_a_branch_fully_contained_in_integration() -> Result {
    let rig = repository()?;
    git(
        &rig,
        &["remote", "add", "origin", "/missing/offline-upstream.git"],
    )?;
    // At the integration tip: nothing unique, so the name removal deletes it.
    ok(
        &rig,
        &["worktree", "create", "at-tip", "--base", "HEAD", "--json"],
    )?;
    let at_tip = rig.project.join(".worktrees/at-tip");
    let output = command(&rig, &["worktree", "remove", "at-tip", "--json"])?;
    assert_eq!(code(&output), 0);
    assert!(
        String::from_utf8_lossy(&output.stderr.bytes)
            .contains("using the current integration resolution")
    );
    let removed = json(&output)?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["at-tip"])
    );
    assert!(!at_tip.exists());
    assert!(!has_branch(&rig, "at-tip")?);

    // An ancestor of a later integration tip: its commit is contained in main.
    let old = git(&rig, &["rev-parse", "HEAD"])?;
    ok(
        &rig,
        &["worktree", "create", "behind", "--base", &old, "--json"],
    )?;
    git(
        &rig,
        &["commit", "--allow-empty", "-qm", "advance integration"],
    )?;
    let removed = ok(&rig, &["worktree", "remove", "behind", "--json"])?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["behind"])
    );
    assert!(!has_branch(&rig, "behind")?);
    rig.assert_legacy_unchanged()
}

pub(super) fn remove_resolves_directory_names_and_remote_integration_refs() -> Result {
    let rig = repository()?;
    let remote = rig
        .project
        .parent()
        .ok_or("fixture parent")?
        .join("fixture-origin.git");
    let remote_text = remote.to_str().ok_or("non-text fixture path")?;
    git(&rig, &["init", "--bare", remote_text])?;
    git(&rig, &["remote", "add", "origin", remote_text])?;
    // The worktree directory is the branch with `/` replaced by `-`.
    ok(
        &rig,
        &[
            "worktree",
            "create",
            "fix/shipped",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    let shipped = rig.project.join(".worktrees/fix-shipped");
    let shipped_text = shipped.to_str().ok_or("non-text fixture path")?;
    fs::write(shipped.join("shipped.txt"), "work")?;
    git(&rig, &["-C", shipped_text, "add", "shipped.txt"])?;
    git(&rig, &["-C", shipped_text, "commit", "-qm", "shipped"])?;
    // Local main stays behind; the work is only on the remote-tracking ref.
    let tip = git(&rig, &["rev-parse", "fix/shipped"])?;
    git(&rig, &["push", "origin", &format!("{tip}:refs/heads/main")])?;
    git(&rig, &["update-ref", "refs/remotes/origin/main", &tip])?;

    let removed = ok(
        &rig,
        &[
            "worktree",
            "remove",
            "fix-shipped",
            "--integration",
            "origin/main",
            "--json",
        ],
    )?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["fix/shipped"])
    );
    assert!(!shipped.exists());
    assert!(!has_branch(&rig, "fix/shipped")?);

    // The merged sweep accepts the same remote-tracking integration ref.
    let swept = branch_commit(&rig, "swept")?;
    let swept_tip = git(&rig, &["rev-parse", "swept"])?;
    // A merge of the branch into the remote's main: contained, and not on the
    // first-parent history, so the sweep may delete it.
    let head = git(&rig, &["rev-parse", "HEAD"])?;
    let merge_tree = git(&rig, &["rev-parse", &format!("{head}^{{tree}}")])?;
    let next = git(
        &rig,
        &[
            "commit-tree",
            &merge_tree,
            "-p",
            &head,
            "-p",
            &swept_tip,
            "-m",
            "merge swept",
        ],
    )?;
    git(&rig, &["update-ref", "refs/remotes/origin/main", &next])?;
    git(
        &rig,
        &[
            "push",
            "--force",
            "origin",
            &format!("{next}:refs/heads/main"),
        ],
    )?;
    let stale = git(&rig, &["rev-parse", "HEAD"])?;
    git(
        &rig,
        &[
            "worktree",
            "remove",
            "--force",
            swept.to_str().ok_or("path")?,
        ],
    )?;
    let result = ok(
        &rig,
        &[
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "origin/main",
            "--branch",
            "swept",
            "--json",
        ],
    )?;
    assert_eq!(result["data"]["pruned_refs"], serde_json::json!(["swept"]));
    assert!(!has_branch(&rig, "swept")?);
    assert_eq!(git(&rig, &["rev-parse", "HEAD"])?, stale);
    rig.assert_legacy_unchanged()
}

pub(super) fn remove_by_name_follows_the_branch_merge_state() -> Result {
    let rig = repository()?;
    let merged = branch_commit(&rig, "merged")?;
    git(&rig, &["merge", "--no-ff", "-qm", "Merge merged", "merged"])?;
    let unmerged = branch_commit(&rig, "unmerged")?;
    let dirty = branch_commit(&rig, "dirty")?;
    let dirty_head = git(&rig, &["rev-parse", "dirty"])?;
    fs::write(dirty.join("precious"), "uncommitted work")?;

    let refused = command(
        &rig,
        &["worktree", "remove", "dirty", "--discard", "--json"],
    )?;
    assert_eq!(code(&refused), 1);
    assert_eq!(json(&refused)?["status"], "refused");
    assert!(has_branch(&rig, "dirty")?);
    assert!(dirty.join("precious").is_file());

    let removed = ok(&rig, &["worktree", "remove", "merged", "--json"])?;
    assert_eq!(
        removed["data"]["pruned_refs"],
        serde_json::json!(["merged"])
    );
    assert!(!merged.exists());
    assert!(!has_branch(&rig, "merged")?);

    let kept = ok(&rig, &["worktree", "remove", "unmerged", "--json"])?;
    assert!(
        kept["data"]["pruned_refs"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert_eq!(kept["data"]["retained"][0]["branch"], "unmerged");
    assert_eq!(kept["data"]["retained"][0]["reason"], "not_merged");
    assert!(!unmerged.exists());
    assert!(has_branch(&rig, "unmerged")?);

    fs::remove_file(dirty.join("precious"))?;
    let discarded = ok(
        &rig,
        &["worktree", "remove", "dirty", "--discard", "--json"],
    )?;
    assert_eq!(discarded["data"]["discarded"][0]["branch"], "dirty");
    assert_eq!(discarded["data"]["discarded"][0]["head"], dirty_head);
    assert!(!dirty.exists());
    assert!(!has_branch(&rig, "dirty")?);
    rig.assert_legacy_unchanged()
}

pub(super) fn merged_pruning_deletes_a_merged_branch_without_a_worktree() -> Result {
    let rig = repository()?;
    let stale = branch_commit(&rig, "stale")?;
    let stale_text = stale.to_str().ok_or("non-text fixture path")?;
    git(&rig, &["worktree", "remove", "--force", stale_text])?;
    git(&rig, &["merge", "--no-ff", "-qm", "Merge stale", "stale"])?;
    let pruned = ok(
        &rig,
        &[
            "worktree",
            "remove",
            "--merged",
            "--integration",
            "main",
            "--branch",
            "stale",
            "--json",
        ],
    )?;
    assert_eq!(pruned["data"]["pruned_refs"], serde_json::json!(["stale"]));
    assert!(!has_branch(&rig, "stale")?);
    rig.assert_legacy_unchanged()
}

pub(super) fn dispatch_into_git_worktree_without_prior_state() -> Result {
    git_worktree_dispatch_is_accepted(false)
}

pub(super) fn dispatch_into_git_worktree_beside_managed_tree() -> Result {
    git_worktree_dispatch_is_accepted(true)
}

pub(super) fn cross_project_writer_names_both_projects() -> Result {
    let selected = repository()?;
    let foreign = repository()?;
    ok(
        &foreign,
        &["worktree", "create", "foreign", "--base", "HEAD", "--json"],
    )?;
    let worktree = foreign.project.join(".worktrees/foreign");
    assert!(worktree.exists());
    let (_, context) = writer_context(&selected)?;
    let error =
        provenant_run::resolve_writer(context.state_root.as_path(), &selected.project, &worktree)
            .err()
            .ok_or("cross-project writer was accepted")?;
    let provenant_run::RunError::WriterProjectMismatch {
        worktree: actual,
        project,
        selected: selected_project,
    } = &error
    else {
        return Err(format!("wrong refusal: {error}").into());
    };
    assert_eq!(actual, &worktree);
    assert_eq!(project, &foreign.project);
    assert_eq!(selected_project, &selected.project);
    let message = error.to_string();
    for project in [&selected.project, &foreign.project] {
        let name = project.to_str().ok_or("project path")?;
        assert!(message.contains(name), "{message} does not name {name}");
    }
    Ok(())
}

pub(super) fn git_created_and_recreated_worktrees_are_dispatchable() -> Result {
    let rig = repository()?;
    ok(
        &rig,
        &["worktree", "create", "stale", "--base", "HEAD", "--json"],
    )?;
    git(&rig, &["worktree", "remove", ".worktrees/stale"])?;
    git(
        &rig,
        &["worktree", "add", "-qb", "stale-again", ".worktrees/stale"],
    )?;
    let (_, context) = writer_context(&rig)?;
    let request = provenant_run::resolve_writer(
        context.state_root.as_path(),
        &rig.project,
        Path::new(".worktrees/stale"),
    )?;
    let writer =
        provenant_run::prepare_writer(&request, context.state_root.as_path(), context.project_id)?;
    assert_eq!(writer.worktree.path(), rig.project.join(".worktrees/stale"));
    assert_eq!(writer.branch.as_str(), "refs/heads/stale-again");
    Ok(())
}
