use super::*;

pub(super) fn gc_refuses_bare_projects_and_their_linked_checkout_fallback() -> Result {
    for name in ["bare.git", ".git"] {
        let mut rig = repository()?;
        let container = rig.root.join("bare-container");
        fs::create_dir(&container)?;
        let bare = container.join(name);
        git(
            &rig,
            &[
                "clone",
                "--bare",
                "-q",
                rig.project.to_str().ok_or("path")?,
                bare.to_str().ok_or("path")?,
            ],
        )?;
        let linked = rig.root.join("bare-linked");
        git(
            &rig,
            &[
                "--git-dir",
                bare.to_str().ok_or("path")?,
                "worktree",
                "add",
                "-qb",
                "bare-linked",
                linked.to_str().ok_or("path")?,
            ],
        )?;
        let registration = git(
            &rig,
            &[
                "--git-dir",
                bare.to_str().ok_or("path")?,
                "worktree",
                "list",
                "--porcelain",
            ],
        )?;
        // The common configuration may get its bare identity from an
        // included file rather than from an entry in config itself.
        let included = rig.root.join("bare-include.conf");
        fs::write(&included, "[core]\n bare = true\n")?;
        git(
            &rig,
            &[
                "--git-dir",
                bare.to_str().ok_or("path")?,
                "config",
                "--unset",
                "core.bare",
            ],
        )?;
        rig.project = linked.clone();
        let missing = command(&rig, &["gc", "--apply", "--json"])?;
        assert_ne!(
            code(&missing),
            0,
            "unknown bare identity accepted as primary: {name}"
        );
        git(
            &rig,
            &[
                "--git-dir",
                bare.to_str().ok_or("path")?,
                "config",
                "--add",
                "include.path",
                included.to_str().ok_or("path")?,
            ],
        )?;
        let from_linked = command(&rig, &["gc", "--apply", "--json"])?;
        assert_ne!(
            code(&from_linked),
            0,
            "bare-backed linked checkout accepted as primary: {name}"
        );
        let value: Value = serde_json::from_slice(&from_linked.stdout.bytes)?;
        assert_eq!(value["error"]["code"], "GC");
        assert!(
            value["error"]["message"]
                .as_str()
                .ok_or("message")?
                .contains("bare repository")
        );
        rig.project = bare.clone();
        let from_bare = command(&rig, &["gc", "--apply", "--json"])?;
        assert_ne!(code(&from_bare), 0);
        assert!(bare.join("HEAD").is_file() && linked.join(".git").is_file());
        assert_eq!(
            git(&rig, &["worktree", "list", "--porcelain"])?,
            registration
        );
    }
    let rig = repository()?;
    let separate = rig.root.join("separate-git-dir");
    git(
        &rig,
        &[
            "init",
            "-q",
            "--separate-git-dir",
            separate.to_str().ok_or("path")?,
        ],
    )?;
    let included = rig.root.join("non-bare-include.conf");
    fs::write(&included, "[core]\n bare = false\n")?;
    git(&rig, &["config", "--unset", "core.bare"])?;
    git(
        &rig,
        &[
            "config",
            "--add",
            "include.path",
            included.to_str().ok_or("path")?,
        ],
    )?;
    ok(&rig, &["gc", "--pressure", "--apply", "--json"])?;
    Ok(())
}

pub(super) fn gc_pressure_collects_only_the_selected_primary_project() -> Result {
    let mut rig = repository()?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\ntarget/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["commit", "-qm", "ignore output"])?;
    let primary = rig.project.clone();
    let other = rig.root.join("other-project");
    git(
        &rig,
        &[
            "clone",
            "-q",
            primary.to_str().ok_or("path")?,
            other.to_str().ok_or("path")?,
        ],
    )?;
    rig.project = other;
    ok(&rig, &["worktree", "create", "other-idle", "--json"])?;
    let other_tree = rig.project.join(".worktrees/other-idle");
    let (_, other_context) = writer_context(&rig)?;
    fs::create_dir(other_tree.join("target"))?;
    fs::write(other_tree.join("target/artifact"), "other output")?;
    age_gc_output(&other_tree.join("target"))?;
    rig.project = primary;
    ok(&rig, &["worktree", "create", "idle", "--json"])?;
    let tree = rig.project.join(".worktrees/idle");
    let (_, context) = writer_context(&rig)?;
    fs::create_dir(tree.join("target"))?;
    fs::write(tree.join("target/artifact"), "selected output")?;
    age_gc_output(&tree.join("target"))?;
    let config = format!(
        "schema_version=1\n[gc]\nschedule=false\nfree_floor='1B'\n[gc.projects.'{}']\nregenerable=['target']\n[gc.projects.'{}']\nregenerable=['target']\n",
        context.project_id, other_context.project_id
    );
    fs::write(rig.state.join("config.toml"), &config)?;
    rig.project = tree.clone();
    let disabled = ok(&rig, &["gc", "--pressure", "--apply", "--json"])?;
    assert!(
        disabled["data"]["worktree_cleanup"]
            .as_array()
            .ok_or("cleanup")?
            .is_empty()
    );
    assert!(tree.join("target/artifact").is_file());
    let disabled_text = command(&rig, &["gc", "--pressure", "--apply"])?;
    assert_eq!(code(&disabled_text), 0);
    let disabled_text = String::from_utf8(disabled_text.stdout.bytes)?;
    assert!(
        disabled_text.contains("pressure collection skipped: gc.schedule=false"),
        "{disabled_text}"
    );
    fs::write(
        rig.state.join("config.toml"),
        config.replace("schedule=false", "schedule=true"),
    )?;
    let pressure = ok(&rig, &["gc", "--pressure", "--apply", "--json"])?;
    assert_eq!(
        pressure["data"]["worktree_cleanup"]
            .as_array()
            .ok_or("cleanup")?
            .len(),
        1
    );
    assert!(!tree.join("target").exists());
    assert!(other_tree.join("target/artifact").is_file());
    assert!(tree.is_dir() && other_tree.is_dir());
    let empty = command(&rig, &["gc", "--pressure", "--apply"])?;
    assert_eq!(code(&empty), 0);
    let empty = String::from_utf8(empty.stdout.bytes)?;
    assert!(
        empty.contains("pressure collection: no eligible data in scope"),
        "{empty}"
    );
    Ok(())
}

pub(super) fn gc_reports_transient_and_preserves_unobserved_project_array() -> Result {
    let mut rig = repository()?;
    let parent = rig.root.join("scratchpads");
    fs::create_dir(&parent)?;
    let moved = parent.join("repository");
    fs::rename(&rig.project, &moved)?;
    rig.project = moved;
    for (key, value) in &mut rig.env {
        if key == "PROVENANT_ROOT" {
            *value = rig.project.clone().into_os_string();
        }
    }
    let common = rig.project.join(".git");
    let outside = rig.root.join("outside");
    git(
        &rig,
        &[
            "worktree",
            "add",
            "-qb",
            "outside",
            outside.to_str().ok_or("path")?,
        ],
    )?;
    rig.project = outside;
    let preview = ok(&rig, &["gc", "--dry-run", "--json"])?;
    assert_eq!(
        preview["data"]["unobserved_projects"],
        serde_json::json!([])
    );
    assert_eq!(
        preview["data"]["skipped_transient"],
        serde_json::json!([common])
    );
    let output = command(&rig, &["gc", "--dry-run"])?;
    assert_eq!(code(&output), 0);
    let text = String::from_utf8(output.stdout.bytes)?;
    assert!(text.contains("skipped transient project"), "{text}");
    let mut normal = repository()?;
    ok(&normal, &["worktree", "create", "observe", "--json"])?;
    let scratch = normal.root.join("scratchpads");
    fs::create_dir(&scratch)?;
    let linked = scratch.join("linked");
    git(
        &normal,
        &[
            "worktree",
            "add",
            "-qb",
            "linked",
            linked.to_str().ok_or("path")?,
        ],
    )?;
    normal.project = linked;
    let normal_preview = ok(&normal, &["gc", "--dry-run", "--json"])?;
    assert_eq!(
        normal_preview["data"]["skipped_transient"],
        serde_json::json!([])
    );
    Ok(())
}

pub(super) fn gc_trims_lane_build_directories_to_the_cap_and_spares_held_locks() -> Result {
    use provenant_os::fs::{Directory, ForeignLock, LockMode};

    let rig = repository()?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\ntarget/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["commit", "-qm", "ignore output"])?;
    ok(&rig, &["worktree", "create", "idle", "--json"])?;
    let worktree = rig.project.join(".worktrees/idle");
    let cache = prepare_lane_cache(&rig, &worktree, "idle")?;
    let profile = |name: &str, age: u64| -> Result<PathBuf> {
        let dir = cache.join("target").join(name);
        fs::create_dir_all(dir.join("deps"))?;
        for lock in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
            fs::write(dir.join(lock), b"")?;
        }
        fs::write(dir.join("deps/payload"), vec![1; 65_536])?;
        let when = std::time::SystemTime::now() - Duration::from_secs(age);
        // Recency is the newest write anywhere below the directory, locks included.
        for lock in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
            fs::File::open(dir.join(lock))?.set_modified(when)?;
        }
        for path in [dir.join("deps/payload"), dir.join("deps"), dir.clone()] {
            fs::File::open(path)?.set_modified(when)?;
        }
        Ok(dir)
    };
    let stale = profile("debug", 3_000)?;
    let building = profile("release", 2_000)?;
    let fresh = profile("fast", 1_000)?;
    fs::write(
        rig.state.join("config.toml"),
        b"schema_version=1\n[gc]\nfree_floor='1B'\n[domains.fabric.lanes]\nbuild_dir_max_bytes=100000\n",
    )?;
    // A running Cargo holds its profile's lock: the directory is never taken.
    let held = Directory::open_anchor(&building)?;
    let ForeignLock::Acquired(_cargo) =
        held.try_flock_foreign(std::ffi::OsStr::new(".cargo-lock"), LockMode::Exclusive)?
    else {
        return Err("the fixture lock is already held".into());
    };
    let names = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|path| path.as_str()?.rsplit('/').next().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    // Over the cap, the least recently used goes first, the one Cargo holds
    // is skipped, and the next oldest goes until the cap is met.
    let preview = ok(&rig, &["gc", "--dry-run", "--json"])?;
    let dirs = &preview["data"]["build_dirs"];
    // Preview deliberately avoids surveying the Cargo profile trees.
    assert!(names(&dirs["removed"]).is_empty(), "{dirs}");
    assert!(
        preview["data"]["notice"]
            .as_str()
            .ok_or("preview notice")?
            .contains("build-profile surveys deferred")
    );
    assert!(stale.join("deps/payload").is_file() && fresh.is_dir());
    let applied = ok(&rig, &["gc", "--apply", "--json"])?;
    let dirs = &applied["data"]["build_dirs"];
    assert_eq!(names(&dirs["removed"]), ["debug", "fast"], "{dirs}");
    assert_eq!(names(&dirs["busy"]), ["release"], "{dirs}");
    assert!(dirs["bytes"].as_u64().ok_or("bytes")? >= 2 * 65_536);
    let remaining = dirs["cache_bytes"].as_u64().ok_or("cache_bytes")?;
    assert!((65_536..=100_000).contains(&remaining), "{dirs}");
    assert!(!stale.exists() && !fresh.exists() && building.is_dir());
    let text = command(&rig, &["gc", "--apply"])?;
    let text = String::from_utf8(text.stdout.bytes)?;
    assert!(text.contains("lane build director(ies)"), "{text}");
    assert!(text.contains("bytes of lane build output remain"), "{text}");
    Ok(())
}

pub(super) fn scheduled_gc_runs_outside_a_checkout_and_obeys_disable_and_writer_locks() -> Result {
    let mut rig = repository()?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\ntarget/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["commit", "-qm", "ignore output"])?;
    ok(&rig, &["worktree", "create", "idle", "--json"])?;
    let worktree = rig.project.join(".worktrees/idle");
    let cache = prepare_lane_cache(&rig, &worktree, "idle")?;
    fn age(path: &Path) -> Result {
        if path.is_dir() {
            for entry in fs::read_dir(path)? {
                age(&entry?.path())?;
            }
        }
        fs::File::open(path)?.set_modified(std::time::UNIX_EPOCH)?;
        Ok(())
    }
    age(&cache)?;
    let project = fs::read_dir(rig.state.join("worktrees"))?
        .next()
        .ok_or("project")??
        .path();
    let registration = fs::read_dir(&project)?
        .find_map(|entry| {
            let path = entry.ok()?.path();
            path.join("writer.lock").is_file().then_some(path)
        })
        .ok_or("registration")?;
    // launchd starts without a checkout as its working directory.
    rig.project = rig.home.clone();
    fs::write(
        rig.state.join("config.toml"),
        b"schema_version=1\n[gc]\nfree_floor='1B'\nschedule=false\n",
    )?;
    let disabled = ok(&rig, &["gc", "--scheduled", "--idle-days", "1", "--json"])?;
    assert!(
        disabled["data"]["removed"]
            .as_array()
            .ok_or("removed")?
            .is_empty()
    );
    assert!(cache.is_dir());
    let pressure_disabled = ok(
        &rig,
        &[
            "gc",
            "--scheduled",
            "--pressure",
            "--apply",
            "--idle-days",
            "1",
            "--json",
        ],
    )?;
    assert!(
        pressure_disabled["data"]["removed"]
            .as_array()
            .ok_or("removed")?
            .is_empty()
    );
    assert!(cache.is_dir());
    fs::write(
        rig.state.join("config.toml"),
        b"schema_version=1\n[gc]\nfree_floor='1B'\n",
    )?;
    let held = provenant_os::fs::Directory::open_anchor(&registration)?.try_lock_existing(
        std::ffi::OsStr::new("writer.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let reserved = ok(&rig, &["gc", "--scheduled", "--idle-days", "1", "--json"])?;
    assert!(
        reserved["data"]["removed"]
            .as_array()
            .ok_or("removed")?
            .is_empty()
    );
    assert!(cache.is_dir());
    drop(held);
    let dry = ok(
        &rig,
        &[
            "gc",
            "--scheduled",
            "--idle-days",
            "1",
            "--dry-run",
            "--json",
        ],
    )?;
    assert_eq!(dry["data"]["removed"][0]["reason"], "idle");
    assert!(cache.is_dir());
    fs::write(cache.join("artifact"), b"recent build")?;
    let removed = ok(
        &rig,
        &[
            "gc",
            "--scheduled",
            "--idle-days",
            "1",
            "--free-floor",
            "18446744073709551615B",
            "--json",
        ],
    )?;
    assert_eq!(removed["data"]["removed"][0]["reason"], "free_floor");
    assert!(removed["data"]["free_bytes_before"].is_u64());
    assert!(removed["data"]["free_bytes_after"].is_u64());
    assert!(!cache.exists());
    let project_id = project
        .file_name()
        .ok_or("project id")?
        .to_str()
        .ok_or("project id")?;
    fs::write(
        rig.state.join("config.toml"),
        format!("schema_version=1\n[gc.projects.'{project_id}']\nregenerable=['target']\n"),
    )?;
    fs::create_dir(worktree.join("target"))?;
    fs::write(worktree.join("target/artifact"), "old output")?;
    age_gc_output(&worktree.join("target"))?;
    let pressure = ok(
        &rig,
        &[
            "gc",
            "--scheduled",
            "--pressure",
            "--apply",
            "--free-floor",
            "18446744073709551615B",
            "--json",
        ],
    )?;
    assert!(worktree.is_dir());
    assert!(!worktree.join("target").exists());
    assert_eq!(
        pressure["data"]["worktree_cleanup"][0]["idle_output"][0]["removed"],
        true
    );
    assert!(
        pressure["data"]["worktree_cleanup"][0]["worktrees"]["removed"]
            .as_array()
            .ok_or("removed")?
            .is_empty()
    );
    Ok(())
}

pub(super) fn gc_idle_keeps_live_and_unobservable_writers_and_removes_free_cache() -> Result {
    use provenant_os::fs::Directory;
    use provenant_os::fs::LockMode;
    use std::ffi::OsStr;

    fn make_idle(path: &Path) -> Result {
        if path.is_dir() {
            for entry in fs::read_dir(path)? {
                make_idle(&entry?.path())?;
            }
        }
        fs::File::open(path)?.set_modified(std::time::UNIX_EPOCH)?;
        Ok(())
    }

    for writer in [
        "live",
        "live_reserved",
        "free",
        "missing",
        "unsafe",
        "reserved",
    ] {
        let rig = repository()?;
        ok(&rig, &["worktree", "create", "idle", "--json"])?;
        let worktree = rig.project.join(".worktrees/idle");
        let cache = prepare_lane_cache(&rig, &worktree, "idle")?;
        let projects =
            fs::read_dir(rig.state.join("worktrees"))?.collect::<std::io::Result<Vec<_>>>()?;
        let project = projects.first().ok_or("project state")?.path();
        let states = fs::read_dir(project)?.collect::<std::io::Result<Vec<_>>>()?;
        let state = states
            .iter()
            .find(|entry| entry.path().join("writer.lock").is_file())
            .ok_or("worktree registration")?
            .path();
        let directory = Directory::open_anchor(&state)?;
        let lock = OsStr::new("writer.lock");
        let held = match writer {
            "live" | "live_reserved" => {
                let held = directory.try_lock_existing(lock, LockMode::Exclusive)?;
                if writer == "live_reserved" {
                    fs::write(state.join("reservation.json"), b"active writer")?;
                }
                Some(held)
            }
            "missing" => {
                fs::remove_file(state.join(lock))?;
                None
            }
            "unsafe" => {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(state.join(lock), fs::Permissions::from_mode(0o666))?;
                None
            }
            "reserved" => {
                fs::write(state.join("reservation.json"), b"unsettled writer")?;
                None
            }
            _ => None,
        };
        if writer == "live" {
            let refused = command(&rig, &["worktree", "remove", "idle", "--json"])?;
            let value = json(&refused)?;
            assert_eq!(value["status"], "refused", "{value}");
            assert_eq!(value["error"]["code"], "WORKTREE_LOCK_BUSY", "{value}");
        }
        make_idle(&cache)?;
        let listed = ok(&rig, &["worktree", "list", "--json"])?;
        let listed_writer = listed["data"]
            .as_array()
            .ok_or("worktrees")?
            .iter()
            .find(|info| info["branch"] == "refs/heads/idle")
            .ok_or("idle worktree")?["writer"]
            .as_str()
            .ok_or("writer state")?;
        assert_eq!(
            listed_writer,
            match writer {
                "live" | "live_reserved" => "busy",
                "free" | "missing" | "reserved" => "free",
                _ => "unknown",
            }
        );
        for dry_run in [true, false] {
            let mut args = vec!["gc", "--idle-days", "1", "--json"];
            if dry_run {
                args.push("--dry-run");
            }
            let result = ok(&rig, &args)?;
            assert_eq!(result["data"]["dry_run"], dry_run);
            if dry_run && writer == "missing" {
                assert!(
                    !state.join(lock).exists(),
                    "preview must not create writer state"
                );
            }
            if matches!(writer, "free" | "missing" | "reserved") {
                assert_eq!(result["data"]["removed"][0]["reason"], "idle");
                assert_eq!(cache.exists(), dry_run);
            } else {
                assert!(
                    result["data"]["removed"]
                        .as_array()
                        .ok_or("removed")?
                        .is_empty()
                );
                assert_eq!(
                    result["data"]["kept"][0]["reason"],
                    // Kernel contention is busy, independent of old records.
                    if writer.starts_with("live") {
                        "busy"
                    } else {
                        "unobservable"
                    }
                );
                assert!(cache.is_dir());
            }
        }
        drop(held);
        if writer.starts_with("live") {
            let _released = directory.try_lock_existing(lock, LockMode::Exclusive)?;
        }
        rig.assert_legacy_unchanged()?;
    }
    Ok(())
}

pub(super) fn gc_fetches_default_and_removes_only_clean_merged_worktrees() -> Result {
    use provenant_os::fs::Directory;
    use provenant_os::fs::LockMode;
    use std::ffi::OsStr;
    let rig = repository()?;
    fs::write(rig.project.join(".gitignore"), ".worktrees/\n**/target/\n")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["commit", "-qm", "ignore output"])?;
    let remote = rig.root.join("upstream.git");
    git(
        &rig,
        &[
            "init",
            "--bare",
            "--initial-branch=trunk",
            remote.to_str().ok_or("remote")?,
        ],
    )?;
    git(
        &rig,
        &["remote", "add", "origin", remote.to_str().ok_or("remote")?],
    )?;
    let stale_main = git(&rig, &["rev-parse", "main"])?;
    ok(&rig, &["worktree", "create", "fresh-old", "--json"])?;
    let merged = branch_commit(&rig, "merged")?;
    let merged_tip = git(&rig, &["rev-parse", "merged"])?;
    let tree = git(&rig, &["rev-parse", "merged^{tree}"])?;
    let tip = git(
        &rig,
        &[
            "commit-tree",
            &tree,
            "-p",
            &stale_main,
            "-p",
            &merged_tip,
            "-m",
            "merge lane",
        ],
    )?;
    git(
        &rig,
        &["push", "origin", &format!("{tip}:refs/heads/trunk")],
    )?;
    ok(
        &rig,
        &["worktree", "create", "fresh-tip", "--base", &tip, "--json"],
    )?;
    for name in [
        "named",
        "explicit",
        "dirty",
        "live",
        "queued",
        "unrecorded",
        "locked",
    ] {
        ok(
            &rig,
            &["worktree", "create", name, "--base", "merged", "--json"],
        )?;
    }
    let cache = prepare_lane_cache(&rig, &merged, "merged")?;
    git(&rig, &["worktree", "lock", ".worktrees/locked"])?;
    fs::write(rig.project.join(".worktrees/dirty/untracked"), "keep")?;
    let live = rig.project.join(".worktrees/live");
    let state_project = fs::read_dir(rig.state.join("worktrees"))?
        .next()
        .ok_or("project state")??
        .path();
    let state = state_project.join(provenant_confinement::worktrees::state_dir_name(&live));
    let state = Directory::open_anchor(&state)?;
    let _guard = state.try_lock_existing(OsStr::new("writer.lock"), LockMode::Exclusive)?;
    let unmerged = branch_commit(&rig, "unmerged")?;
    let (_, context) = writer_context(&rig)?;
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let merged_writer =
        provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &merged)?;
    let ended = writer_run(merged_writer, "gc-merged", 120, "finished")?;
    store.create(&ended, &call)?;
    store.cancel(ended.attempt_id, "finished", &call)?;
    fs::create_dir(merged.join("target"))?;
    fs::write(merged.join("target/artifact"), "old build")?;
    age_gc_output(&merged.join("target"))?;
    let output_config = format!(
        "[gc.projects.'{}']\nregenerable=['target']\n",
        context.project_id
    );
    fs::write(
        rig.state.join("config.toml"),
        format!("schema_version=1\n[gc]\nfree_floor='1B'\n{output_config}"),
    )?;
    let queued_path = rig.project.join(".worktrees/queued");
    let queued_writer =
        provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &queued_path)?;
    let queued = writer_run(queued_writer, "gc-queued", 121, "waiting")?;
    store.create(&queued, &call)?;
    fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\n{output_config}foreign_worktrees='merged-clean'\n"
        ),
    )?;
    let queued_preview = ok(&rig, &["gc", "--dry-run", "--json"])?;
    let cleanup = queued_preview["data"]["worktree_cleanup"]
        .as_array()
        .ok_or("worktree cleanup")?;
    assert!(
        cleanup
            .iter()
            .flat_map(|report| report["worktrees"]["retained"]
                .as_array()
                .into_iter()
                .flatten())
            .any(|entry| entry["branch"] == "queued"),
        "GC preview should report the queued branch as retained: {}",
        queued_preview["data"]["worktree_cleanup"]
    );
    assert!(queued_path.is_dir());
    fs::write(
        rig.state.join("config.toml"),
        format!("schema_version=1\n[gc]\nfree_floor='1B'\n{output_config}"),
    )?;
    let explicit = ok(
        &rig,
        &[
            "worktree",
            "remove",
            "explicit",
            "--integration",
            "main",
            "--json",
        ],
    )?;
    assert_eq!(explicit["data"]["retained"][0]["reason"], "not_merged");
    assert!(has_branch(&rig, "explicit")?);
    let named = ok(&rig, &["worktree", "remove", "named", "--json"])?;
    assert_eq!(named["data"]["pruned_refs"], serde_json::json!(["named"]));
    assert_eq!(git(&rig, &["rev-parse", "main"])?, stale_main);
    assert_eq!(git(&rig, &["rev-parse", "origin/trunk"])?, tip);
    // One broken registration must not stop the remaining sweep.
    git(
        &rig,
        &["worktree", "add", "-qb", "broken", ".worktrees/broken"],
    )?;
    fs::remove_dir_all(rig.project.join(".worktrees/broken"))?;
    let dry = ok(&rig, &["gc", "--dry-run", "--json"])?;
    assert_eq!(dry["data"]["dry_run"], true);
    assert!(merged.is_dir());
    assert!(cache.is_dir());
    assert!(dry["data"]["worktree_cleanup"][0]["dry_run"] == true);
    assert!(merged.join("target/artifact").is_file());
    // Preview does not fetch the upstream proof needed to remove a tree.
    assert!(
        dry["data"]["worktree_cleanup"][0]["worktrees"]["pruned_refs"]
            .as_array()
            .ok_or("refs")?
            .is_empty()
    );
    assert!(
        dry["data"]["notice"]
            .as_str()
            .ok_or("preview notice")?
            .contains("upstream refresh")
    );
    // Alert failure must not block reclamation, including on a full disk.
    fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[gc]\nfree_floor='1B'\nalert_below='18446744073709551615B'\n{output_config}"
        ),
    )?;
    fs::create_dir(rig.state.join("gc"))?;
    fs::set_permissions(
        rig.state.join("gc"),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    fs::create_dir(rig.state.join("gc/alert.lock"))?;
    let collected = ok(&rig, &["gc", "--scheduled", "--apply", "--json"])?;
    assert!(
        collected["data"]["alerts"][0]
            .as_str()
            .ok_or("alert failure")?
            .contains("disk alert failed")
    );
    assert!(
        collected["data"]["worktree_cleanup"][0]["worktrees"]["pruned_refs"]
            .as_array()
            .ok_or("refs")?
            .contains(&serde_json::json!("merged"))
    );
    assert!(!merged.exists());
    assert!(!cache.exists());
    assert!(!has_branch(&rig, "merged")?);
    assert!(rig.project.join(".worktrees/dirty").is_dir());
    assert!(live.is_dir());
    assert!(unmerged.is_dir());
    assert!(queued_path.is_dir());
    for name in ["fresh-old", "fresh-tip", "unrecorded"] {
        assert!(
            rig.project.join(".worktrees").join(name).is_dir(),
            "{name} was swept"
        );
        assert!(has_branch(&rig, name)?);
    }
    for (branch, reason) in [
        ("fresh-old", "no_unique_commit"),
        ("fresh-tip", "at_integration_tip"),
        ("locked", "locked_or_prunable"),
    ] {
        let retained = collected["data"]["worktree_cleanup"][0]["worktrees"]["retained"]
            .as_array()
            .ok_or("retained")?;
        assert!(
            retained
                .iter()
                .any(|entry| entry["branch"] == branch && entry["reason"] == reason),
            "expected {branch} with {reason}; got {retained:?}"
        );
    }
    assert!(
        !collected["data"]["worktree_cleanup"][0]["worktrees"]["retained"]
            .as_array()
            .ok_or("retained")?
            .iter()
            .any(|entry| entry["branch"] == "main")
    );
    assert!(has_branch(&rig, "unmerged")?);
    assert_eq!(git(&rig, &["rev-parse", "main"])?, stale_main);
    store.cancel(queued.attempt_id, "queue ended", &call)?;
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(!queued_path.exists());
    Ok(())
}

pub(super) fn gc_output_requires_terminal_lanes_and_respects_keep_recent_dirty_and_links() -> Result
{
    let rig = repository()?;
    fs::write(
        rig.project.join(".gitignore"),
        ".worktrees/\n**/target/\n**/node_modules\n",
    )?;
    fs::create_dir_all(rig.project.join("tracked/target"))?;
    fs::write(rig.project.join("tracked/target/keep"), "tracked")?;
    git(&rig, &["add", ".gitignore"])?;
    git(&rig, &["add", "-f", "tracked/target/keep"])?;
    git(&rig, &["commit", "-qm", "ignore build output"])?;
    ok(&rig, &["worktree", "create", "ended", "--json"])?;
    let path = rig.project.join(".worktrees/ended");
    for output in [
        "old/target",
        "kept/target",
        "recent/target",
        "another/node_modules",
    ] {
        fs::create_dir_all(path.join(output))?;
        fs::write(path.join(output).join("artifact.lock"), "regenerate")?;
        age_gc_output(&path.join(output))?;
    }
    age_gc_output(&path.join("tracked/target"))?;
    fs::write(path.join("recent/target/new"), "new build")?;
    let external = rig.root.join("external");
    fs::create_dir(&external)?;
    fs::write(external.join("keep"), "external")?;
    std::os::unix::fs::symlink(&external, path.join("node_modules"))?;
    let (roots, context) = writer_context(&rig)?;
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let writer = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let prepared = writer_run(writer.clone(), "gc-ended", 101, "regenerate")?;
    let attempt = prepared.attempt_id;
    store.create(&prepared, &call)?;
    let config = format!(
        "schema_version=1\n[gc]\nfree_floor='1B'\n[gc.projects.'{}']\nregenerable=['target','node_modules']\nkeep=['kept/target']\n",
        context.project_id
    );
    fs::write(rig.state.join("config.toml"), &config)?;
    // Queued is live, even without a running process or held writer lease.
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(path.join("old/target").is_dir());
    store.cancel(attempt, "fixture ended", &call)?;
    let before_child =
        crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)?;
    assert!(before_child.is_current());
    assert_eq!(
        crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)?
            .get(&path),
        Some(&true)
    );
    // A queued child in the host graph also blocks output pruning.
    let child_store = open_writer_store(&context)?;
    let child_run = writer_run(writer, "gc-child", 111, "nested")?;
    child_store.create(&child_run, &call)?;
    assert!(!before_child.is_current());
    assert!(
        !crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)
            .is_ok_and(|states| states.get(&path) == Some(&true))
    );
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(path.join("old/target").is_dir());
    child_store.cancel(child_run.attempt_id, "child ended", &call)?;
    let snapshot =
        crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)?;
    assert!(snapshot.is_current());
    let writer = provenant_run::resolve_writer(context.state_root.as_path(), &rig.project, &path)?;
    let later = writer_run(writer, "gc-later", 112, "queued after snapshot")?;
    store.create(&later, &call)?;
    assert!(!snapshot.is_current());
    store.cancel(later.attempt_id, "later ended", &call)?;
    assert!(!snapshot.is_current(), "a changed snapshot stays invalid");
    let dry = ok(&rig, &["gc", "--dry-run", "--json"])?;
    assert_eq!(dry["data"]["dry_run"], true);
    assert!(path.join("old/target").is_dir());
    assert!(
        dry["data"]["worktree_cleanup"][0]["output"]
            .as_array()
            .ok_or("output")?
            .iter()
            .all(|entry| entry["removed"] == false)
    );
    fs::write(path.join("dirty"), "keep")?;
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(path.join("old/target").is_dir());
    fs::remove_file(path.join("dirty"))?;
    // Unreadable sealed lane state protects the worktree.
    let request = rig
        .state
        .join("attempts")
        .join(prepared.attempt_id.to_string())
        .join("request.json");
    let sealed = fs::read(&request)?;
    fs::write(&request, b"broken")?;
    assert!(
        !crate::gc_support::lane_states(&roots, &rig.home, &rig.project, context.project_id)
            .is_ok_and(|states| states.get(&path) == Some(&true))
    );
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(path.join("old/target").is_dir());
    fs::write(&request, sealed)?;
    // No declaration grants no output deletion.
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n",
    )?;
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(path.join("old/target").is_dir());
    fs::write(rig.state.join("config.toml"), config)?;
    let applied = ok(&rig, &["gc", "--scheduled", "--apply", "--json"])?;
    assert!(!path.join("old/target").exists(), "{applied}");
    assert!(!path.join("another/node_modules").exists());
    assert!(path.join("kept/target").is_dir());
    assert!(path.join("tracked/target/keep").is_file());
    assert!(path.join("recent/target/new").is_file());
    assert!(
        fs::symlink_metadata(path.join("node_modules"))?
            .file_type()
            .is_symlink()
    );
    assert!(external.join("keep").is_file());
    Ok(())
}

pub(super) fn gc_reports_terminal_tmp_paths_and_bytes_in_json_and_text() -> Result {
    use std::os::unix::fs::DirBuilderExt;
    let rig = repository()?;
    ok(&rig, &["worktree", "create", "gc-tmp", "--json"])?;
    let (_, context) = writer_context(&rig)?;
    let store = open_writer_store(&context)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    let mut input = writer_run(
        provenant_run::WriterRequest {
            extra: Default::default(),
            project: rig.project.clone(),
            worktree: rig.project.join(".worktrees/gc-tmp"),
        },
        "gc-tmp",
        201,
        "fixture",
    )?;
    input.request.task.writer = None;
    input.created_at = provenant_core::WallTime::from_micros(1)?;
    store.create(&input, &call)?;
    let id = input.attempt_id;
    store.withdraw(
        id,
        provenant_run::AttemptState::Queued {
            extra: Default::default(),
        },
        provenant_run::Outcome::Cancelled,
        "fixture",
        provenant_core::WallTime::from_micros(2)?,
        &call,
    )?;
    let attempt = rig.state.join(format!("attempts/{id}"));
    let temporary = attempt.join("tmp");
    fs::DirBuilder::new().mode(0o700).create(&temporary)?;
    fs::write(temporary.join("payload"), vec![1; 8192])?;
    // Durable evidence and provider history must survive applied GC byte for byte.
    let mut retained = BTreeMap::new();
    for name in [
        "prompt.txt",
        "launch.json",
        "stdout.log",
        "stderr.log",
        "result.md",
        "process.json",
        "process-exit.json",
        "settlement.json",
        "terminal-pending.json",
        "terminal-commit.json",
        "release.json",
        "provider-history/candidate-0/sessions/session.jsonl",
        "provider-history/candidate-0/thread_history_1.sqlite",
    ] {
        let path = attempt.join(name);
        fs::create_dir_all(path.parent().ok_or("retained parent")?)?;
        if !path.exists() {
            fs::write(&path, format!("retained fixture: {name}\n\0"))?;
        }
        retained.insert(path.clone(), fs::read(path)?);
    }
    drop(store);
    let db = rusqlite::Connection::open(rig.state.join("run.sqlite3"))?;
    db.execute(
        "UPDATE attempts SET tmp_retained=1 WHERE id=?1",
        [id.to_string()],
    )?;
    let preview = ok(&rig, &["gc", "--dry-run", "--json"])?;
    let removal = &preview["data"]["provider_state"]["attempt_tmp"][0];
    assert_eq!(removal["path"], temporary.to_str().ok_or("tmp path")?);
    let bytes = removal["bytes"].as_u64().ok_or("tmp bytes")?;
    assert!(bytes >= 8192);
    assert!(temporary.exists());
    let text = command(&rig, &["gc", "--dry-run"])?;
    assert_eq!(code(&text), 0);
    let text = String::from_utf8(text.stdout.bytes)?;
    assert!(text.contains("would remove attempt tmp"), "{text}");
    assert!(
        text.contains(temporary.to_str().ok_or("tmp path")?),
        "{text}"
    );
    assert!(text.contains(&format!("{bytes} bytes")), "{text}");
    assert!(temporary.exists());
    let applied = ok(&rig, &["gc", "--apply", "--json"])?;
    assert_eq!(
        applied["data"]["provider_state"]["attempt_tmp"][0],
        *removal
    );
    assert!(!temporary.exists());
    for (path, bytes) in retained {
        assert_eq!(
            fs::read(&path)?,
            bytes,
            "{} changed during GC",
            path.display()
        );
    }
    let retained: bool = db.query_row(
        "SELECT tmp_retained FROM attempts WHERE id=?1",
        [id.to_string()],
        |row| row.get(0),
    )?;
    assert!(!retained);
    Ok(())
}

pub(super) fn gc_disk_alert_is_once_per_dip_and_manual_preview_does_not_latch() -> Result {
    let rig = repository()?;
    ok(&rig, &["worktree", "create", "idle", "--json"])?;
    let set_floor = |floor: &str| -> Result {
        fs::write(
            rig.state.join("config.toml"),
            format!("schema_version=1\n[gc]\nfree_floor='{floor}'\nalert_below='1B'\n"),
        )?;
        Ok(())
    };
    set_floor("18446744073709551615B")?;
    let dry = ok(&rig, &["gc", "--dry-run", "--json"])?;
    assert_eq!(dry["data"]["alerts"].as_array().ok_or("alerts")?.len(), 1);
    assert!(!rig.state.join("gc/low-disk.json").exists());
    let first = ok(&rig, &["gc", "--apply", "--json"])?;
    assert_eq!(first["data"]["alerts"].as_array().ok_or("alerts")?.len(), 1);
    let again = ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(
        again["data"]["alerts"]
            .as_array()
            .ok_or("alerts")?
            .is_empty()
    );
    set_floor("0B")?;
    ok(&rig, &["gc", "--apply", "--json"])?;
    assert!(!rig.state.join("gc/low-disk.json").exists());
    set_floor("18446744073709551615B")?;
    let second = ok(&rig, &["gc", "--apply", "--json"])?;
    assert_eq!(
        second["data"]["alerts"].as_array().ok_or("alerts")?.len(),
        1
    );
    let (roots, context) = writer_context(&rig)?;
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let request = provenant_fabric_store::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = provenant_fabric_store::Store::open(&roots.state, &request)?;
    let events = store.events(context.project_id, 0, 100, &request)?;
    assert_eq!(
        events
            .items
            .iter()
            .filter(|event| event.record.entity.as_deref() == Some("gc.low_disk"))
            .count(),
        2
    );
    Ok(())
}

/// Publish `tip` as the origin's default branch, so the sweep can prove landing.
fn publish_upstream(rig: &TestRig, tip: &str) -> Result {
    let remote = rig.root.join("upstream.git");
    let remote = remote.to_str().ok_or("remote")?;
    git(rig, &["init", "--bare", "--initial-branch=trunk", remote])?;
    git(rig, &["remote", "add", "origin", remote])?;
    git(
        rig,
        &["push", "-q", "origin", &format!("{tip}:refs/heads/trunk")],
    )?;
    Ok(())
}

/// Merge the lane's tip into the base as a second parent, so it has landed
/// without being on the target's first-parent line.
fn merged_upstream(rig: &TestRig, lane: &Path) -> Result<String> {
    let lane = lane.to_str().ok_or("lane path")?;
    let base = git(rig, &["rev-parse", "HEAD"])?;
    let tip = git(rig, &["-C", lane, "rev-parse", "HEAD"])?;
    let tree = git(rig, &["rev-parse", "HEAD^{tree}"])?;
    git(
        rig,
        &[
            "commit-tree",
            &tree,
            "-p",
            &base,
            "-p",
            &tip,
            "-m",
            "merge lane",
        ],
    )
}

fn settlement_receipt(fixture: &crate::fabric::peer_tests::SettlementFixture) -> Result<Value> {
    let clock = provenant_os::clock::SystemClock;
    let cancel = provenant_core::CancelToken::new();
    let call = writer_call(&clock, &cancel)?;
    Ok(fixture
        .store
        .worktree_settlement(fixture.settled.attempt_id, &call)?
        .ok_or("settlement receipt")?)
}

fn files_under(root: &Path) -> Result<std::collections::BTreeSet<PathBuf>> {
    let mut files = std::collections::BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if fs::symlink_metadata(&path)?.is_dir() {
                pending.push(path.clone());
            }
            files.insert(path.strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(files)
}

pub(super) fn gc_collects_a_settled_lane_whose_completion_attempt_failed() -> Result {
    use crate::fabric::gc_cleanup::SPAWNED;
    let fixture = crate::fabric::peer_tests::SettlementFixture::resumed_with_lane_commit()?;
    let rig = &fixture.rig;
    let merged = merged_upstream(rig, &fixture.worktree)?;
    publish_upstream(rig, &merged)?;
    let before = files_under(&rig.state)?;
    SPAWNED.set(Err("detached collection could not start".into()));
    crate::fabric::settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
    SPAWNED.set(Ok(Vec::new()));
    let receipt = settlement_receipt(&fixture)?;
    assert_eq!(receipt["state"], "retained", "{receipt}");
    assert_eq!(receipt["cleanup_deferred"], true, "{receipt}");
    assert!(receipt.get("retryable").is_none(), "{receipt}");
    assert!(receipt.get("cleanup_warning").is_none(), "{receipt}");
    // The failed attempt leaves nothing behind outside the attempt's own receipt.
    let after = files_under(&rig.state)?;
    let added: Vec<_> = after.difference(&before).collect();
    assert!(
        added.iter().all(|path| path.starts_with("attempts")),
        "settlement wrote outside its attempt record: {added:?}"
    );
    assert!(fixture.worktree.exists());
    ok(rig, &["gc", "--apply", "--json"])?;
    assert!(
        !fixture.worktree.exists(),
        "the sweep must collect a lane its settlement attempt missed"
    );
    let receipt = settlement_receipt(&fixture)?;
    assert_eq!(receipt["state"], "pruned", "{receipt}");
    Ok(())
}

/// A lane with its own commits, landed by fast-forward so its head is on the
/// upstream's first-parent line, whose settlement collection found the
/// collector busy or never ran. The path-based sweep keeps such a lane; the
/// owner-bound pass for unsettled receipts collects it.
pub(super) fn gc_collects_a_fast_forwarded_lane_its_settlement_missed() -> Result {
    if crate::fabric::peer_tests::host_test(
        "gc_collects_a_fast_forwarded_lane_its_settlement_missed",
    )? {
        return Ok(());
    }
    use crate::fabric::gc_cleanup::SPAWNED;
    for busy in [true, false] {
        let fixture =
            crate::fabric::peer_tests::SettlementFixture::resumed_with_integrated_lane_commit()
                .map_err(|error| format!("fast-forward fixture creation failed: {error}"))?;
        let rig = &fixture.rig;
        let lane = fixture.worktree.to_str().ok_or("lane path")?;
        let lane_head = git(rig, &["-C", lane, "rev-parse", "HEAD"])?;
        publish_upstream(rig, &lane_head)?;
        let clock = provenant_os::clock::SystemClock;
        let cancel = provenant_core::CancelToken::new();
        let call = writer_call(&clock, &cancel)?;
        let delivered = fixture.store.deliver(
            fixture.settled.attempt_id,
            None,
            provenant_run::HostDeliveryContext::new(rig.env.clone()),
            false,
            &call,
        )?;
        assert!(
            matches!(
                &delivered.state,
                provenant_run::WriterDeliveryState::Delivered { .. }
            ),
            "{delivered:?}"
        );
        let integration = delivered
            .integration
            .as_ref()
            .ok_or("integration receipt")?;
        assert_eq!(integration.target, "refs/heads/trunk");
        assert_eq!(integration.integrated.to_string(), lane_head);
        if busy {
            SPAWNED.set(Ok(Vec::new()));
            crate::fabric::settle_owned_worktree(
                &fixture.settled,
                &fixture.roots,
                &FreeWorkers,
                None,
            )
            .map_err(|error| format!("busy-branch settlement failed: {error}"))?;
            let request = SPAWNED
                .replace(Ok(Vec::new()))
                .map_err(|error| error.to_string())?
                .pop()
                .ok_or("settlement did not start collection")?;
            // Only this lane is busy. Other GC items remain collectable.
            let state = provenant_os::fs::Directory::open_anchor(
                &fixture
                    .rig
                    .state
                    .join("worktrees")
                    .join(fixture.settled.project_id.to_string())
                    .join(provenant_confinement::worktrees::state_dir_name(
                        &fixture.worktree,
                    )),
            )?;
            let held = state.try_lock(
                std::ffi::OsStr::new("writer.lock"),
                provenant_os::fs::LockMode::Exclusive,
            )?;
            let pending_receipt = settlement_receipt(&fixture)?;
            crate::fabric::gc_cleanup::run(&request, &FreeWorkers).map_err(|error| {
                format!("busy settlement cleanup failed: {error}; receipt={pending_receipt}")
            })?;
            assert!(fixture.worktree.exists());
            drop(held);
        } else {
            SPAWNED.set(Err("detached collection could not start".into()));
            crate::fabric::settle_owned_worktree(
                &fixture.settled,
                &fixture.roots,
                &FreeWorkers,
                None,
            )
            .map_err(|error| format!("deferred-branch settlement failed: {error}"))?;
            SPAWNED.set(Ok(Vec::new()));
            assert_eq!(settlement_receipt(&fixture)?["cleanup_deferred"], true);
        }
        assert!(fixture.worktree.exists(), "busy={busy}");
        let swept = ok(rig, &["gc", "--apply", "--json"])
            .map_err(|error| format!("fast-forward lane GC sweep failed (busy={busy}): {error}"))?;
        assert!(
            !fixture.worktree.exists(),
            "the sweep must collect a fast-forwarded lane its settlement missed (busy={busy}); gc={swept}; settlement={}",
            settlement_receipt(&fixture)?
        );
        let receipt = settlement_receipt(&fixture)?;
        assert_eq!(receipt["state"], "pruned", "busy={busy}: {receipt}");
    }
    Ok(())
}

pub(super) fn completion_and_sweep_retain_unlanded_or_dirty_lanes() -> Result {
    use crate::fabric::gc_cleanup::SPAWNED;
    for dirty in [false, true] {
        let fixture = crate::fabric::peer_tests::SettlementFixture::resumed_with_lane_commit()?;
        let rig = &fixture.rig;
        if dirty {
            let merged = merged_upstream(rig, &fixture.worktree)?;
            publish_upstream(rig, &merged)?;
            fs::write(fixture.worktree.join("edit.txt"), "uncommitted")?;
        } else {
            publish_upstream(rig, &git(rig, &["rev-parse", "HEAD"])?)?;
        }
        SPAWNED.set(Ok(Vec::new()));
        crate::fabric::settle_owned_worktree(&fixture.settled, &fixture.roots, &FreeWorkers, None)?;
        let request = SPAWNED
            .replace(Ok(Vec::new()))
            .map_err(|error| error.to_string())?
            .pop()
            .ok_or("settlement did not start collection")?;
        crate::fabric::gc_cleanup::run(&request, &FreeWorkers)?;
        assert!(
            fixture.worktree.exists(),
            "settlement collection removed a kept lane (dirty={dirty})"
        );
        let receipt = settlement_receipt(&fixture)?;
        assert_eq!(receipt["state"], "retained", "{receipt}");
        ok(rig, &["gc", "--apply", "--json"])?;
        assert!(
            fixture.worktree.exists(),
            "the sweep removed a kept lane (dirty={dirty})"
        );
        if dirty {
            // The edit alone kept it: once it is gone the sweep collects.
            fs::remove_file(fixture.worktree.join("edit.txt"))?;
            ok(rig, &["gc", "--apply", "--json"])?;
            assert!(!fixture.worktree.exists());
        }
    }
    Ok(())
}

/// When the run store cannot be opened or listed during unsettled lane
/// collection, the sweep warns on stderr and carries on with the path-based pass.
pub(super) fn gc_reports_settled_lane_store_error_and_continues() -> Result {
    let fixture = crate::fabric::peer_tests::SettlementFixture::resumed_with_lane_commit()?;
    let rig = &fixture.rig;
    // Close the fixture's WAL-mode store first. While it is open, the WAL
    // frames hold page 1 and mask a corrupted main file, so the read-only open
    // would succeed. Removing the companions makes the corruption visible.
    drop(fixture.store);
    let run_db = rig.state.join("run.sqlite3");
    for companion in ["run.sqlite3-wal", "run.sqlite3-shm"] {
        match fs::remove_file(rig.state.join(companion)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    // Corrupt the run database file so open_disk_read_only fails.
    fs::write(&run_db, b"corrupted sqlite database")?;
    let output = command(rig, &["gc", "--apply", "--json"])?;
    // Provider retention reads the same unreadable store and reports it as an
    // incomplete retention, so the exit status is not asserted here.
    let stderr = String::from_utf8_lossy(&output.stderr.bytes);
    assert!(
        stderr.contains("warning: settled lane collection failed for"),
        "stderr must contain settled lane collection warning: {stderr}"
    );
    Ok(())
}
