use super::*;

/// Resolve the structured path emitted by `worktree create` without assuming
/// how a branch name maps to a directory.
fn created_worktree_path(value: &Value) -> Result<PathBuf> {
    let wire = &value["data"]["path"];
    let root = wire["root"].as_str().ok_or("worktree path root")?;
    let relative = wire["relative"]["path"]
        .as_str()
        .ok_or("worktree relative path")?;
    Ok(Path::new(root).join(relative))
}

pub(super) fn status_and_worktree_list_report_stranded_work() -> Result {
    let rig = repository()?;
    let created = ok(
        &rig,
        &["worktree", "create", "fix/3650-stranded-work", "--json"],
    )?;
    let path = created_worktree_path(&created)?;
    ok(&rig, &["worktree", "create", "fix/3546-clean", "--json"])?;
    let file = path.join("stranded.txt");
    fs::write(&file, "one\ntwo\nthree\n")?;
    let older = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(2 * 86_400))
        .ok_or("age arithmetic")?;
    fs::File::open(&file)?.set_modified(older)?;

    let status = ok(&rig, &["status", "--json"])?;
    let rows = status["data"]["stranded_work"]
        .as_array()
        .ok_or("status has no stranded_work array")?;
    assert_eq!(rows.len(), 1, "{status}");
    let row = &rows[0];
    assert_eq!(row["branch"], "fix/3650-stranded-work");
    assert_eq!(row["issue"], "#3650");
    assert_eq!(row["files"], 1);
    assert_eq!(row["lines"], 3);
    assert_eq!(row["age"], "2d");
    assert!(
        row["path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("3650-stranded-work")
    );

    let listing = ok(&rig, &["worktree", "list", "--json"])?;
    assert_eq!(listing["data"].as_array().map(Vec::len), Some(3));

    let human_status = command(&rig, &["status"])?;
    let status_text = String::from_utf8(human_status.stdout.bytes)?;
    assert!(status_text.contains("stranded work:"), "{status_text}");
    assert!(status_text.contains("#3650"), "{status_text}");
    assert!(
        status_text.contains("1 file, 3 lines, 2d old"),
        "{status_text}"
    );
    assert!(
        status_text.contains("provenant run implement --worktree"),
        "{status_text}"
    );
    assert!(!status_text.contains("3546-clean"), "{status_text}");

    let human_list = command(&rig, &["worktree", "list"])?;
    let list_text = String::from_utf8(human_list.stdout.bytes)?;
    assert!(list_text.contains("stranded work:"), "{list_text}");
    assert!(list_text.contains("#3650"), "{list_text}");
    Ok(())
}

pub(super) fn status_omits_stranded_work_with_a_live_lane() -> Result {
    let rig = repository()?;
    let created = ok(
        &rig,
        &["worktree", "create", "fix/3650-stranded-work", "--json"],
    )?;
    let path = created_worktree_path(&created)?;
    fs::write(path.join("stranded.txt"), "one\ntwo\nthree\n")?;
    let (roots, _) = writer_context(&rig)?;
    let cancel = provenant_core::CancelToken::new();
    let clock = provenant_os::clock::SystemClock;
    provenant_confinement::worktrees::with_context(
        &roots,
        &rig.home,
        &rig.project,
        &clock,
        &cancel,
        |context| {
            // A held writer lease is a live lane. The survey must not call
            // this work stranded while the lease exists.
            let _guard =
                provenant_confinement::worktrees::cache_writer_guard(context.project_state, &path)
                    .map_err(|occupancy| {
                        provenant_confinement::worktrees::WorktreeError::UnknownOccupancy {
                            path: path.clone(),
                            detail: format!("fixture writer lease unavailable: {occupancy:?}"),
                        }
                    })?;
            let rows = crate::stranded_work::survey_in_context(
                context,
                &cancel,
                crate::stranded_work::now_micros(&clock),
            );
            assert!(
                rows.iter().all(|row| row.path != path),
                "a held lane was reported as stranded: {rows:?}"
            );
            Ok(())
        },
    )?;
    Ok(())
}

pub(super) fn status_reports_in_progress_merge_with_edits() -> Result {
    let rig = repository()?;
    let created = ok(&rig, &["worktree", "create", "fix/3650-merge", "--json"])?;
    let path = created_worktree_path(&created)?;
    fs::write(path.join("merged.txt"), "one\ntwo\n")?;
    let path_text = path.to_str().ok_or("worktree path")?.to_owned();
    let git_dir = git(&rig, &["-C", &path_text, "rev-parse", "--absolute-git-dir"])?;
    fs::write(Path::new(&git_dir).join("MERGE_HEAD"), "0".repeat(40))?;

    let status = ok(&rig, &["status", "--json"])?;
    let rows = status["data"]["stranded_work"]
        .as_array()
        .ok_or("status has no stranded_work array")?;
    assert_eq!(rows.len(), 1, "{status}");
    assert_eq!(rows[0]["operation"], "merge", "{status}");
    assert_eq!(rows[0]["files"], 1, "{status}");
    assert_eq!(rows[0]["lines"], 2, "{status}");

    let human = command(&rig, &["status"])?;
    let text = String::from_utf8(human.stdout.bytes)?;
    assert!(text.contains("(merge)"), "{text}");
    Ok(())
}

pub(super) fn status_reports_dirty_detached_worktree() -> Result {
    let rig = repository()?;
    git(
        &rig,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            ".worktrees/detached",
            "HEAD",
        ],
    )?;
    let path = rig.project.join(".worktrees/detached");
    fs::write(path.join("detached.txt"), "one\ntwo\nthree\n")?;

    let status = ok(&rig, &["status", "--json"])?;
    let rows = status["data"]["stranded_work"]
        .as_array()
        .ok_or("status has no stranded_work array")?;
    assert_eq!(rows.len(), 1, "{status}");
    assert!(rows[0]["branch"].is_null(), "{status}");
    assert_eq!(rows[0]["files"], 1, "{status}");
    assert_eq!(rows[0]["lines"], 3, "{status}");

    let human = command(&rig, &["status"])?;
    let text = String::from_utf8(human.stdout.bytes)?;
    assert!(text.contains("detached HEAD"), "{text}");
    Ok(())
}

pub(super) fn naming_and_post_create_ports_are_advisory_and_evidenced() -> Result {
    let rig = repository()?;
    let (roots, _execution) = writer_context(&rig)?;
    let marker_path = rig.state.join("post-create-runs");
    let marker = marker_path.to_str().ok_or("marker path")?.to_owned();
    let mut ports = provenant_config::PortTable::builtin();
    ports.naming = provenant_config::Port {
        source: provenant_records::Layer::Project,
        backend: provenant_config::PortBackend::Command {
            command: provenant_records::PortCommand {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    r#"printf 'derived-%s\n' "$1""#.into(),
                    "--".into(),
                ],
                exit_codes: provenant_records::CommandExitCodes {
                    success: vec![0],
                    unknown: vec![],
                    other: provenant_records::ExitDisposition::Refused,
                },
            },
        },
    };
    ports.post_create = provenant_config::Port {
        source: provenant_records::Layer::Project,
        backend: provenant_config::PortBackend::Command {
            command: provenant_records::PortCommand {
                argv: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    r#"printf '%s\n' "$3" >> "$1""#.into(),
                    "--".into(),
                    marker.clone(),
                ],
                exit_codes: provenant_records::CommandExitCodes {
                    success: vec![0],
                    unknown: vec![],
                    other: provenant_records::ExitDisposition::Refused,
                },
            },
        },
    };
    fs::create_dir_all(rig.project.join(".provenant"))?;
    fs::write(
        rig.project.join(provenant_config::PORTS_CONFIG),
        serde_json::to_vec(&ports)?,
    )?;
    let created = ok(
        &rig,
        &["worktree", "create", "setup", "--base", "HEAD", "--json"],
    )?;
    let path = rig.project.join(".worktrees/derived-setup");
    let marker_bytes = fs::read(&marker_path)?;
    assert_eq!(
        fs::canonicalize(String::from_utf8(marker_bytes.clone())?.trim())?,
        fs::canonicalize(&path)?
    );
    let context = provenant_cli::fabric::Context::open(roots, &rig.home, &rig.project)?;
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    // Writer provisioning reuses this checkout without executing the hook again.
    provenant_cli::fabric::graph_relay::provision_writer(
        &context,
        ".worktrees/derived-setup",
        "derived-setup",
        created["data"]["head"].as_str().ok_or("created head")?,
        provenant_core::WaitBudget::cli(clock.mono_now(), 30)?,
        &cancel,
    )?;
    assert_eq!(fs::read(&marker_path)?, marker_bytes);
    let inventory = provenant_config::port_command_receipts(
        &rig.state,
        &provenant_core::SweepControl::default(),
    )?;
    assert!(inventory.complete, "{:?}", inventory.issues);
    for (owner, phase) in [("naming", "derive"), ("post_create", "create")] {
        let receipt = &inventory
            .entries
            .iter()
            .find(|entry| entry.receipt.owner == owner && entry.receipt.phase == phase)
            .ok_or("missing setup command receipt")?
            .receipt;
        assert_eq!(receipt.source, provenant_records::Layer::Project);
        assert_eq!(
            receipt.backend,
            provenant_records::PortCommandBackend::RepositoryCommand
        );
        assert_eq!(
            receipt.lifetime,
            provenant_records::PortReceiptLifetime::AgeDays {
                days: provenant_config::DEFAULT_PORT_COMMAND_RECEIPT_AGE_DAYS,
            }
        );
    }

    ports.naming.backend = provenant_config::PortBackend::Command {
        command: provenant_records::PortCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'bad\\nbranch\\n'".into(),
            ],
            exit_codes: provenant_records::CommandExitCodes {
                success: vec![0],
                unknown: vec![],
                other: provenant_records::ExitDisposition::Refused,
            },
        },
    };
    ports.post_create.backend = provenant_config::PortBackend::Command {
        command: provenant_records::PortCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'port failure\n' >&2; exit 7".into(),
            ],
            exit_codes: provenant_records::CommandExitCodes {
                success: vec![0],
                unknown: vec![],
                other: provenant_records::ExitDisposition::Refused,
            },
        },
    };
    fs::write(
        rig.project.join(provenant_config::PORTS_CONFIG),
        serde_json::to_vec(&ports)?,
    )?;
    // Omit --base to exercise the default-remote discovery re-entry.
    let remote = rig
        .project
        .parent()
        .ok_or("fixture parent")?
        .join("naming-origin.git");
    let remote_text = remote.to_str().ok_or("fixture remote path")?;
    git(
        &rig,
        &["init", "--bare", "--initial-branch=main", remote_text],
    )?;
    git(&rig, &["remote", "add", "origin", remote_text])?;
    git(&rig, &["push", "-u", "origin", "main"])?;
    let failed = ok(&rig, &["worktree", "create", "setup-failure", "--json"])?;
    let warnings = failed["data"]["warnings"]
        .as_array()
        .ok_or("worktree warnings are not an array")?;
    assert!(warnings.iter().any(|warning| {
        warning
            .as_str()
            .is_some_and(|text| text.contains("naming hook returned an invalid derived branch"))
    }));
    assert!(warnings.iter().any(|warning| {
        warning
            .as_str()
            .is_some_and(|text| text.contains("post-create command returned Refused"))
    }));
    let retained = rig.project.join(".worktrees/setup-failure");
    assert!(retained.join(".git").is_file());
    assert!(
        !git(
            &rig,
            &[
                "-C",
                retained.to_str().ok_or("worktree path")?,
                "rev-parse",
                "HEAD"
            ]
        )?
        .is_empty()
    );
    Ok(())
}

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
        b"legacy lane build output",
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
    assert!(
        orphan.exists(),
        "recent orphan remains within the idle window"
    );
    age_gc_output(&orphan)?;
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(!orphan.exists(), "gc must sweep idle orphaned caches");
    rig.assert_v1_root_untouched()
}

pub(super) fn existing_branch_behind_base_warns_and_keeps_its_tip() -> Result {
    // Spec: docs/specs/run.md#lane-git-and-import -- an existing branch
    // behind its base warns BRANCH_BEHIND_BASE and proceeds; --base records the
    // merge target and never resets the branch.
    let rig = repository()?;
    git(&rig, &["branch", "existing"])?;
    let tip = git(&rig, &["rev-parse", "existing"])?;
    git(&rig, &["commit", "--allow-empty", "-qm", "advance"])?;
    let json_output = ok(
        &rig,
        &["worktree", "create", "existing", "--base", "HEAD", "--json"],
    )?;
    let warnings = json_output["data"]["warnings"]
        .as_array()
        .ok_or("worktree warnings are not an array")?;
    assert!(
        warnings.iter().any(|warning| {
            warning.as_str().is_some_and(|text| {
                text.contains("branch existing at")
                    && text.contains("does not contain base")
                    && text.contains("merge target")
            })
        }),
        "{json_output}"
    );
    assert_eq!(
        git(&rig, &["rev-parse", "existing"])?,
        tip,
        "--base must not move the branch"
    );
    let head = git(&rig, &["rev-parse", "HEAD"])?;
    assert_eq!(json_output["data"]["base"].as_str(), Some(head.as_str()));
    Ok(())
}

// Spec: docs/specs/run-launch.md#writer-binding
pub(super) fn create_reuses_a_clean_worktree_and_names_an_existing_conflict() -> Result {
    let rig = repository()?;
    let base = git(&rig, &["rev-parse", "HEAD"])?;
    let created = ok(
        &rig,
        &[
            "worktree",
            "create",
            "feature/reuse",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    let path = created_worktree_path(&created)?;
    assert_eq!(created["data"]["head"], base);

    let reused = ok(
        &rig,
        &[
            "worktree",
            "create",
            "feature/reuse",
            "--base",
            "HEAD",
            "--json",
        ],
    )?;
    assert_eq!(reused["data"]["path"], created["data"]["path"]);
    assert_eq!(reused["data"]["head"], base);
    let warnings = reused["data"]["warnings"]
        .as_array()
        .ok_or("reuse warning array")?;
    assert!(
        warnings.iter().any(|warning| {
            warning
                .as_str()
                .is_some_and(|text| text.contains("reusing") && text.contains("feature/reuse"))
        }),
        "{reused}"
    );
    let listed = ok(&rig, &["worktree", "list", "--json"])?;
    assert_eq!(listed["data"].as_array().map(Vec::len), Some(2));

    // A checkout advanced past --base is admitted on the occupied path just
    // as it would be on a free path: the base is an ancestor of its HEAD.
    let path_text = path.to_str().ok_or("worktree path text")?.to_owned();
    git(
        &rig,
        &[
            "-C",
            &path_text,
            "commit",
            "--allow-empty",
            "-qm",
            "advance",
        ],
    )?;
    let tip = git(&rig, &["-C", &path_text, "rev-parse", "HEAD"])?;
    assert_ne!(tip, base);
    let advanced = ok(
        &rig,
        &[
            "worktree",
            "create",
            "feature/reuse",
            "--base",
            &base,
            "--json",
        ],
    )?;
    assert_eq!(advanced["data"]["path"], created["data"]["path"]);
    assert_eq!(advanced["data"]["head"], tip);
    assert_eq!(advanced["data"]["reused"], true);

    // A base that is not an ancestor of the checkout HEAD stays refused and
    // names both the path and the repair.
    git(&rig, &["commit", "--allow-empty", "-qm", "sibling"])?;
    let sibling = git(&rig, &["rev-parse", "HEAD"])?;
    let conflict = command(
        &rig,
        &[
            "worktree",
            "create",
            "feature/reuse",
            "--base",
            &sibling,
            "--json",
        ],
    )?;
    assert_ne!(code(&conflict), 0);
    let value = json(&conflict)?;
    let message = value["error"]["message"].as_str().ok_or("error message")?;
    assert!(message.contains("feature/reuse"), "{value}");
    assert!(message.contains(path_text.as_str()), "{value}");
    let cause = value["error"]["cause"].as_str().ok_or("named cause")?;
    assert!(cause.contains("omit --base"), "{value}");
    let repair = value["error"]["repair"].as_str().ok_or("repair")?;
    assert!(repair.contains("matching base"), "{value}");

    fs::write(path.join("uncommitted"), "work in progress")?;
    let dirty = command(
        &rig,
        &[
            "worktree",
            "create",
            "feature/reuse",
            "--base",
            &base,
            "--json",
        ],
    )?;
    assert_ne!(code(&dirty), 0);
    let value = json(&dirty)?;
    let message = value["error"]["message"].as_str().ok_or("error message")?;
    assert!(message.contains("feature/reuse"), "{value}");
    let cause = value["error"]["cause"].as_str().ok_or("named cause")?;
    assert!(cause.contains("uncommitted changes"), "{value}");
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
        rig.assert_v1_root_untouched()?;
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
    rig.assert_v1_root_untouched()
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
    rig.assert_v1_root_untouched()
}

// Spec: docs/specs/supervisor.md#shared-occupancy-removal-never-forces
pub(super) fn remove_by_name_follows_the_branch_merge_state() -> Result {
    let rig = repository()?;
    let merged = branch_commit(&rig, "merged")?;
    git(&rig, &["merge", "--no-ff", "-qm", "Merge merged", "merged"])?;
    let unmerged = branch_commit(&rig, "unmerged")?;
    let dirty = branch_commit(&rig, "dirty")?;
    let dirty_head = git(&rig, &["rev-parse", "dirty"])?;
    fs::write(dirty.join("precious"), "uncommitted work")?;

    let refused = command(&rig, &["worktree", "remove", "dirty", "--json"])?;
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

    // Work held only by its own branch is not proven preserved elsewhere, so
    // removal without discard refuses and leaves the tree and branch intact.
    let kept = command(&rig, &["worktree", "remove", "unmerged", "--json"])?;
    assert_eq!(code(&kept), 1);
    assert_eq!(json(&kept)?["status"], "refused");
    assert!(unmerged.exists());
    assert!(has_branch(&rig, "unmerged")?);

    let discarded = ok(
        &rig,
        &["worktree", "remove", "dirty", "--discard", "--json"],
    )?;
    assert_eq!(discarded["data"]["discarded"][0]["branch"], "dirty");
    assert_eq!(discarded["data"]["discarded"][0]["head"], dirty_head);
    let archive = discarded["data"]["archives"]
        .as_array()
        .and_then(|archives| archives.iter().find(|archive| archive["tip"] == dirty_head))
        .and_then(|archive| archive["path"].as_str())
        .ok_or("discard must preserve the dirty work in an archive")?;
    let untracked = fs::read(Path::new(archive).join("untracked.tar"))?;
    assert!(
        untracked
            .windows(b"uncommitted work".len())
            .any(|bytes| bytes == b"uncommitted work")
    );
    assert!(!dirty.exists());
    assert!(!has_branch(&rig, "dirty")?);
    rig.assert_v1_root_untouched()
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
    rig.assert_v1_root_untouched()
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
