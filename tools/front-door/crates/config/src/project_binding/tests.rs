use super::*;
use provenant_core::Roots;

use std::fs;
use std::os::unix::fs::PermissionsExt;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn canonical_base(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().canonicalize().unwrap()
}

fn roots(temp: &tempfile::TempDir) -> Roots {
    let inputs = provenant_test_support::test_roots(temp.path()).unwrap();
    inputs
        .resolve(&provenant_test_support::IdentityPathResolver)
        .unwrap()
}

#[test]
fn a_checkout_that_stops_being_git_keeps_its_existing_run_identity() {
    let temp = tempdir();
    let roots = roots(&temp);
    let common = canonical_base(&temp).join("bare");
    fs::create_dir_all(common.join("objects")).unwrap();
    fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
    let location = ProjectLocation::new(&common).unwrap();
    let before = bind_project(&roots, location.clone()).unwrap();
    assert_eq!(before.common_dir(), common);
    fs::remove_file(common.join("HEAD")).unwrap();
    assert_eq!(bind_project(&roots, location.clone()).unwrap(), before);
    assert_eq!(lookup_project(&roots, &location).unwrap(), Some(before));
    assert!(registered_gc_projects(&roots).unwrap().is_empty());
}

#[test]
fn registry_readers_skip_in_progress_stages_without_repair() {
    let temp = tempdir();
    let roots = roots(&temp);
    let common = canonical_base(&temp).join("repo/.git");
    fs::create_dir_all(common.join("objects")).unwrap();
    fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
    let binding = bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();
    let stage = roots
        .state
        .as_path()
        .join(PROJECTS_DIR)
        .join(format!("{STAGE_PREFIX}{}", ProjectId::from_bytes([31; 16])));
    fs::create_dir(&stage).unwrap();
    fs::write(stage.join(RECORD), "partial").unwrap();
    assert!(registered_gc_projects(&roots).unwrap().contains(&binding));

    let next = canonical_base(&temp).join("next/.git");
    fs::create_dir_all(next.join("objects")).unwrap();
    fs::write(next.join("HEAD"), "ref: refs/heads/main").unwrap();
    bind_project(&roots, ProjectLocation::new(&next).unwrap()).unwrap();
    assert_eq!(fs::read_to_string(stage.join(RECORD)).unwrap(), "partial");
}

#[test]
fn an_unknown_project_directory_does_not_break_the_inventory() {
    let temp = tempdir();
    let roots = roots(&temp);
    let common = canonical_base(&temp).join("repo/.git");
    fs::create_dir_all(common.join("objects")).unwrap();
    fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
    let binding = bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();

    // A stray directory named by an obsolete 64-hex ports key is skipped
    // with a warning; it does not invalidate every other binding.
    let stray_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let stray = roots.state.as_path().join(PROJECTS_DIR).join(stray_key);
    fs::create_dir_all(&stray).unwrap();
    fs::write(stray.join("ports.json"), "{}").unwrap();
    assert_eq!(registered_projects(&roots).unwrap(), vec![binding.clone()]);
    assert_eq!(
        registered_gc_projects(&roots).unwrap(),
        vec![binding.clone()]
    );
    let selection = registered_gc_project_selection(&roots).unwrap();
    assert_eq!(selection.project_ids, vec![binding.project_id()]);

    // A recognised binding whose record is corrupt still refuses.
    let binding_dir = project_binding_dir(roots.state.as_path(), binding.project_id());
    fs::write(binding_dir.join(RECORD), "not a binding record").unwrap();
    assert!(registered_projects(&roots).is_err());
}

#[test]
fn transient_roots_have_identity_without_persistent_registration() {
    let temp = tempdir();
    let roots = roots(&temp);
    for path in [
        roots.state.as_path().join("probe-one"),
        roots.state.as_path().join("attempts/run"),
        canonical_base(&temp).join(".provenant-staging.one/.git"),
        canonical_base(&temp).join("scratchpads/x"),
        canonical_base(&temp).join("not-git"),
    ] {
        fs::create_dir_all(&path).unwrap();
        let location = ProjectLocation::new(&path).unwrap();
        let first = bind_project(&roots, location.clone()).unwrap();
        assert_eq!(
            lookup_project(&roots, &location).unwrap(),
            Some(first.clone())
        );
        assert_eq!(bind_project(&roots, location).unwrap(), first);
    }
    assert!(!roots.state.as_path().join(PROJECTS_DIR).exists());
    assert!(registered_gc_projects(&roots).unwrap().is_empty());
}

#[test]
fn scheduled_projects_exclude_missing_non_git_and_old_transient_bindings() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);
    let valid = base.join("repository/.git");
    fs::create_dir_all(valid.join("objects")).unwrap();
    fs::write(valid.join("HEAD"), "ref: refs/heads/main").unwrap();
    let good = bind_project(&roots, ProjectLocation::new(&valid).unwrap()).unwrap();
    let stale = base.join("stale/.git");
    fs::create_dir_all(&stale).unwrap();
    bind_project(&roots, ProjectLocation::new(&stale).unwrap()).unwrap();
    fs::remove_dir(&stale).unwrap();
    let mut excluded = Vec::new();
    let projects = existing_projects(&roots).unwrap().unwrap();
    for relative in ["scratchpads/repo/.git", ".provenant-staging.repo/.git"] {
        let common = base.join(relative);
        fs::create_dir_all(common.join("objects")).unwrap();
        fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
        // Simulate a record created before transient registration filtering.
        create_binding(
            &projects,
            &roots.state.as_path().join(PROJECTS_DIR),
            common.clone(),
        )
        .unwrap();
        excluded.push(common);
    }
    // Existing identity records are preserved for indexed run evidence.
    assert_eq!(registered_projects(&roots).unwrap().len(), 4);
    assert_eq!(registered_gc_projects(&roots).unwrap(), vec![good.clone()]);
    let selection = registered_gc_project_selection(&roots).unwrap();
    let mut selected = Vec::new();
    let mut skipped_transient = Vec::new();
    let mut vanished = Vec::new();
    for project in selection.project_ids {
        match lookup_gc_project(&roots, project).unwrap() {
            GcProjectStatus::Git(binding) => selected.push(binding),
            GcProjectStatus::Transient(path) => skipped_transient.push(path),
            GcProjectStatus::Vanished { binding, .. } => {
                vanished.push(binding.common_dir().to_owned())
            }
            GcProjectStatus::Excluded => {}
        }
    }
    assert_eq!(selected, vec![good]);
    assert_eq!(vanished, vec![stale]);
    skipped_transient.sort();
    excluded.sort();
    assert_eq!(skipped_transient, excluded);
}

#[test]
fn forgetting_a_binding_removes_only_its_record() {
    let temp = tempdir();
    let roots = roots(&temp);
    let common = canonical_base(&temp).join("repository/.git");
    fs::create_dir_all(common.join("objects")).unwrap();
    fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
    let location = ProjectLocation::new(&common).unwrap();
    let binding = bind_project(&roots, location.clone()).unwrap();
    let directory = project_binding_dir(roots.state.as_path(), binding.project_id());
    assert!(directory.exists());

    assert!(forget_project(&roots, binding.project_id()).unwrap());
    assert_eq!(lookup_project(&roots, &location).unwrap(), None);
    assert!(registered_projects(&roots).unwrap().is_empty());
    assert!(
        registered_gc_project_selection(&roots)
            .unwrap()
            .project_ids
            .is_empty()
    );
    assert!(
        directory.exists(),
        "the host-project directory stays for a later bind"
    );
    assert!(!forget_project(&roots, binding.project_id()).unwrap());

    let rebound = bind_project(&roots, location).unwrap();
    assert_eq!(rebound.project_id(), binding.project_id());
    assert_eq!(registered_projects(&roots).unwrap(), vec![rebound]);
}

#[test]
fn gc_retires_a_binding_whose_common_directory_has_been_gone_past_the_idle_lifetime() {
    let temp = tempdir();
    let roots = roots(&temp);
    let common = canonical_base(&temp).join("repository/.git");
    fs::create_dir_all(common.join("objects")).unwrap();
    fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
    let binding = bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();
    fs::remove_dir_all(&common).unwrap();
    assert!(!binding.common_dir_present());
    assert!(matches!(
        lookup_gc_project(&roots, binding.project_id()).unwrap(),
        GcProjectStatus::Vanished { .. }
    ));
    let retirement = retire_vanished_projects(&roots, 3600, false).unwrap();
    assert!(retirement.warnings.is_empty());
    assert!(retirement.retired.is_empty());

    let record = project_binding_dir(roots.state.as_path(), binding.project_id()).join(RECORD);
    fs::File::open(&record)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
    assert_eq!(
        retire_vanished_projects(&roots, 3600, true)
            .unwrap()
            .retired
            .iter()
            .map(ProjectBinding::project_id)
            .collect::<Vec<_>>(),
        vec![binding.project_id()],
        "a preview names the binding without removing it"
    );
    assert!(registered_projects(&roots).unwrap().contains(&binding));
    assert_eq!(
        retire_vanished_projects(&roots, 3600, false)
            .unwrap()
            .retired,
        vec![binding]
    );
    assert!(registered_projects(&roots).unwrap().is_empty());
}

#[test]
fn a_vanished_primary_checkout_is_absent_but_a_common_only_binding_is_present() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);
    let common = base.join("external.git");
    fs::create_dir_all(common.join("objects")).unwrap();
    fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
    let checkout = base.join("checkout");
    fs::create_dir_all(&checkout).unwrap();
    let binding = bind_project(
        &roots,
        ProjectLocation::from_verified_primary_checkout(&common, &checkout).unwrap(),
    )
    .unwrap();
    assert!(binding.common_dir_present());
    assert!(binding.primary_checkout_present());

    fs::remove_dir_all(&checkout).unwrap();
    assert!(
        binding.common_dir_present(),
        "the separate common dir survives"
    );
    assert!(!binding.primary_checkout_present());

    let bare = base.join("bare.git");
    fs::create_dir_all(bare.join("objects")).unwrap();
    fs::write(bare.join("HEAD"), "ref: refs/heads/main").unwrap();
    let common_only = bind_project(&roots, ProjectLocation::new(&bare).unwrap()).unwrap();
    assert_eq!(common_only.primary_checkout_hint(), None);
    assert!(
        common_only.primary_checkout_present(),
        "a binding with no checkout hint has no checkout to have vanished"
    );
}

#[test]
fn gc_retires_a_binding_whose_primary_checkout_vanished_while_its_common_dir_survives() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);
    let common = base.join("external.git");
    fs::create_dir_all(common.join("objects")).unwrap();
    fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
    let checkout = base.join("checkout");
    fs::create_dir_all(&checkout).unwrap();
    let binding = bind_project(
        &roots,
        ProjectLocation::from_verified_primary_checkout(&common, &checkout).unwrap(),
    )
    .unwrap();
    fs::remove_dir_all(&checkout).unwrap();
    assert!(binding.common_dir_present());
    assert!(!binding.primary_checkout_present());
    assert!(matches!(
        lookup_gc_project(&roots, binding.project_id()).unwrap(),
        GcProjectStatus::Vanished { .. }
    ));

    let record = project_binding_dir(roots.state.as_path(), binding.project_id()).join(RECORD);
    fs::File::open(&record)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
    assert_eq!(
        retire_vanished_projects(&roots, 3600, false)
            .unwrap()
            .retired,
        vec![binding],
        "the unregister command retires a vanished checkout with a surviving common dir"
    );
    assert!(registered_projects(&roots).unwrap().is_empty());
    assert!(common.exists());
}

#[test]
fn retirement_warns_about_an_unreadable_binding_and_retires_the_rest() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);

    let vanished = base.join("vanished/.git");
    fs::create_dir_all(vanished.join("objects")).unwrap();
    fs::write(vanished.join("HEAD"), "ref: refs/heads/main").unwrap();
    let retired = bind_project(&roots, ProjectLocation::new(&vanished).unwrap()).unwrap();
    fs::remove_dir_all(&vanished).unwrap();
    let record = project_binding_dir(roots.state.as_path(), retired.project_id()).join(RECORD);
    fs::File::open(&record)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();

    let corrupt = base.join("corrupt/.git");
    fs::create_dir_all(corrupt.join("objects")).unwrap();
    fs::write(corrupt.join("HEAD"), "ref: refs/heads/main").unwrap();
    let unreadable = bind_project(&roots, ProjectLocation::new(&corrupt).unwrap()).unwrap();
    fs::write(
        project_binding_dir(roots.state.as_path(), unreadable.project_id()).join(RECORD),
        "truncated",
    )
    .unwrap();

    let retirement = retire_vanished_projects(&roots, 3600, false).unwrap();
    assert_eq!(retirement.retired, vec![retired]);
    assert_eq!(retirement.warnings.len(), 1);
    assert!(
        retirement.warnings[0].contains(&unreadable.project_id().to_string()),
        "the warning names the unreadable binding: {:?}",
        retirement.warnings
    );
    assert!(matches!(
        lookup_gc_project(&roots, unreadable.project_id()),
        Err(ConfigError::InvalidProjectBinding)
    ));
}

// Spec: docs/specs/disk.md#bounded-sweeps
#[test]
fn scheduled_registry_scan_ids_resume_after_root_change_without_stale_bindings() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);
    for index in 0..140 {
        let common = base.join(format!("repository-{index:02}/.git"));
        fs::create_dir_all(common.join("objects")).unwrap();
        fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
        bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();
    }

    let mut visited = 0;
    let partial = registered_gc_project_selection_with_progress(
        &roots,
        None,
        "0000000000000001",
        true,
        &mut || {
            visited += 1;
            visited <= 135
        },
    )
    .unwrap();
    assert!(partial.interrupted_at.is_some());
    assert!(partial.project_ids.is_empty());
    let resume = partial
        .continuation
        .as_deref()
        .expect("bounded scan saves an epoch sidecar");
    let tick_path = roots.state.as_path().join("tick");
    assert!(tick_path.join(resume).exists());

    let replaced_common = base.join("repository-00/.git");
    let replacement = base.join("repository-replaced/.git");
    fs::create_dir_all(replacement.join("objects")).unwrap();
    fs::write(replacement.join("HEAD"), "ref: refs/heads/main").unwrap();
    let project = binding_id(&replaced_common);
    let registry = existing_projects(&roots).unwrap().unwrap();
    let project_directory = registry
        .open_verified_directory(OsStr::new(&project.to_string()))
        .unwrap();
    let replacement_record = ProjectBindingRecord {
        schema_version: 1,
        project,
        canonical_common_dir: encode_location(&replacement),
        canonical_primary_checkout_hint: None,
        extra: Default::default(),
    };
    let bytes = provenant_records::encode(&replacement_record).unwrap();
    project_directory
        .atomic_write_with_permissions(
            OsStr::new(RECORD),
            ".project-",
            &bytes,
            0o600,
            PublishMode::Replace,
        )
        .unwrap()
        .require_durable()
        .unwrap();

    let new_common = base.join("repository-new/.git");
    fs::create_dir_all(new_common.join("objects")).unwrap();
    fs::write(new_common.join("HEAD"), "ref: refs/heads/main").unwrap();
    bind_project(&roots, ProjectLocation::new(&new_common).unwrap()).unwrap();

    let resumed = registered_gc_project_selection_with_progress(
        &roots,
        Some(resume),
        "0000000000000002",
        true,
        &mut || true,
    )
    .unwrap();
    assert!(resumed.interrupted_at.is_none());
    assert_eq!(resumed.project_ids.len(), 141);
    let bindings = resumed
        .project_ids
        .iter()
        .filter_map(
            |project| match lookup_gc_project(&roots, *project).unwrap() {
                GcProjectStatus::Git(binding) => Some(binding),
                GcProjectStatus::Transient(_)
                | GcProjectStatus::Vanished { .. }
                | GcProjectStatus::Excluded => None,
            },
        )
        .collect::<Vec<_>>();
    assert!(
        !bindings
            .iter()
            .any(|binding| binding.common_dir() == replaced_common)
    );
    assert!(
        bindings
            .iter()
            .any(|binding| binding.common_dir() == new_common)
    );
    assert!(
        bindings
            .iter()
            .any(|binding| binding.common_dir() == replacement)
    );
    assert!(!roots.state.as_path().join("tick").join(resume).exists());
    assert!(
        !fs::read_dir(&tick_path)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains("gc-project-selection"))
    );
}

// Spec: docs/specs/disk.md#bounded-sweeps
#[test]
fn registry_id_scan_resumes_from_saved_directory_cookie() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);
    for index in 0..140 {
        let common = base.join(format!("project-{index:03}/.git"));
        fs::create_dir_all(common.join("objects")).unwrap();
        fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
        bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();
    }

    let mut checks = 0;
    let partial = registered_gc_project_selection_with_progress(
        &roots,
        None,
        "0000000000000011",
        true,
        &mut || {
            checks += 1;
            checks <= 70
        },
    )
    .unwrap();
    assert!(partial.interrupted_at.is_some());
    assert!(partial.project_ids.is_empty());
    let resume = partial.continuation.as_deref().unwrap();

    let mut resumed_checks = 0;
    let resumed = registered_gc_project_selection_with_progress(
        &roots,
        Some(resume),
        "0000000000000012",
        true,
        &mut || {
            resumed_checks += 1;
            true
        },
    )
    .unwrap();
    assert!(resumed.interrupted_at.is_none());
    assert_eq!(resumed.project_ids.len(), 140);
    assert!(resumed_checks < 75);
}

// Spec: docs/specs/disk.md#bounded-sweeps
#[test]
fn resumed_project_ids_resolve_child_records_fresh() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);
    let original = base.join("repository-original/.git");
    let other = base.join("repository-other/.git");
    for common in [&original, &other] {
        fs::create_dir_all(common.join("objects")).unwrap();
        fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
    }
    bind_project(&roots, ProjectLocation::new(&original).unwrap()).unwrap();
    bind_project(&roots, ProjectLocation::new(&other).unwrap()).unwrap();
    let registry = existing_projects(&roots).unwrap().unwrap();
    let root_stamp = registry.metadata().unwrap();

    let mut checks = 0;
    let partial = registered_gc_project_selection_with_progress(
        &roots,
        None,
        "0000000000000021",
        true,
        &mut || {
            checks += 1;
            checks <= 1
        },
    )
    .unwrap();
    assert!(partial.interrupted_at.is_some());
    let resume = partial.continuation.as_deref().unwrap();
    let saved: GcSelectionSnapshot =
        serde_json::from_slice(&fs::read(roots.state.as_path().join("tick").join(resume)).unwrap())
            .unwrap();
    let changed_id = *saved.project_ids.iter().next().unwrap();
    assert_eq!(saved.project_ids.len(), 1);

    let replacement = base.join("repository-replacement/.git");
    fs::create_dir_all(replacement.join("objects")).unwrap();
    fs::write(replacement.join("HEAD"), "ref: refs/heads/main").unwrap();
    let project_directory = registry
        .open_verified_directory(OsStr::new(&changed_id.to_string()))
        .unwrap();
    let replacement_record = ProjectBindingRecord {
        schema_version: 1,
        project: changed_id,
        canonical_common_dir: encode_location(&replacement),
        canonical_primary_checkout_hint: None,
        extra: Default::default(),
    };
    let bytes = provenant_records::encode(&replacement_record).unwrap();
    project_directory
        .atomic_write_with_permissions(
            OsStr::new(RECORD),
            ".project-",
            &bytes,
            0o600,
            PublishMode::Replace,
        )
        .unwrap()
        .require_durable()
        .unwrap();
    let unchanged_root = registry.metadata().unwrap();
    assert_eq!(root_stamp.device, unchanged_root.device);
    assert_eq!(root_stamp.inode, unchanged_root.inode);
    assert_eq!(root_stamp.modified_ns, unchanged_root.modified_ns);

    let resumed = registered_gc_project_selection_with_progress(
        &roots,
        Some(resume),
        "0000000000000022",
        true,
        &mut || true,
    )
    .unwrap();
    assert_eq!(resumed.project_ids.len(), 2);
    let GcProjectStatus::Git(binding) = lookup_gc_project(&roots, changed_id).unwrap() else {
        panic!("current child record is a Git project")
    };
    assert_eq!(binding.common_dir(), replacement);
    assert_ne!(binding.common_dir(), original);
}

// Spec: docs/specs/disk.md#bounded-sweeps
#[test]
fn preview_registry_scan_does_not_publish_a_resume_sidecar() {
    let temp = tempdir();
    let roots = roots(&temp);
    let common = canonical_base(&temp).join("repository/.git");
    fs::create_dir_all(common.join("objects")).unwrap();
    fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
    bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();

    let partial = registered_gc_project_selection_with_progress(
        &roots,
        None,
        "0000000000000003",
        false,
        &mut || false,
    )
    .unwrap();
    assert!(partial.interrupted_at.is_some());
    assert!(partial.continuation.is_none());
    assert!(!roots.state.as_path().join("tick").exists());
}

// Competing no-replace publishers converge by validating the winning record.
// Spec: docs/specs/state.md#configuration
#[test]
fn binding_creation_and_lookup_share_private_state_authority() {
    for mode in [0o700, 0o750, 0o755, 0o777] {
        let temp = tempdir();
        let roots = roots(&temp);
        let common = canonical_base(&temp).join("project/.git");
        fs::create_dir_all(&common).unwrap();
        let location = ProjectLocation::new(common).unwrap();
        fs::set_permissions(roots.state.as_path(), fs::Permissions::from_mode(mode)).unwrap();
        let created = bind_project(&roots, location.clone());
        let found = lookup_project(&roots, &location);
        if mode & 0o077 == 0 {
            assert_eq!(found.unwrap(), Some(created.unwrap()));
        } else {
            assert!(matches!(
                created,
                Err(ConfigError::ProjectBindingDirectoryUnsafe {
                    mode: observed,
                    owner_matches: true,
                    ..
                }) if observed == mode
            ));
            assert!(matches!(
                found,
                Err(ConfigError::ProjectBindingDirectoryUnsafe {
                    mode: observed,
                    owner_matches: true,
                    ..
                }) if observed == mode
            ));
            assert_eq!(
                fs::metadata(roots.state.as_path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                mode
            );
            assert!(!roots.state.as_path().join(PROJECTS_DIR).exists());
        }
    }
}

#[test]
fn concurrent_first_bindings_converge_on_one_atomic_record() {
    let temp = tempdir();
    let roots = roots(&temp);
    let common = canonical_base(&temp).join("repo/.git");
    fs::create_dir_all(&common).unwrap();
    let location = ProjectLocation::new(&common).unwrap();
    let barrier = std::sync::Barrier::new(8);
    let results = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let roots = &roots;
                let location = location.clone();
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    bind_project(roots, location)
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(results.iter().all(|binding| binding == &results[0]));
    assert_eq!(
        registered_projects(&roots).unwrap(),
        vec![results[0].clone()]
    );
}

// Spec: docs/specs/state.md#configuration
#[test]
fn first_publication_waits_for_binding_mutations() {
    let temp = tempdir();
    let roots = roots(&temp);
    let common = canonical_base(&temp).join("repo/.git");
    fs::create_dir_all(&common).unwrap();
    let state = Directory::open_anchor(roots.state.as_path()).unwrap();
    let projects = open_or_create_projects(&state).unwrap();
    let projects_path = roots.state.as_path().join(PROJECTS_DIR);
    let name = OsString::from(binding_id(&common).to_string());
    projects.mkdir(&name, 0o700).unwrap();
    let directory = projects.open_verified_directory(&name).unwrap();
    let guard = directory
        .try_lock(OsStr::new(BINDING_LOCK), LockMode::Exclusive)
        .unwrap();
    let (started, ready) = std::sync::mpsc::channel();
    let (finished, result) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            started.send(()).unwrap();
            finished
                .send(create_binding(&projects, &projects_path, common))
                .unwrap();
        });
        ready.recv().unwrap();
        assert!(
            result
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err()
        );
        assert!(
            directory
                .metadata_at(OsStr::new(RECORD), false)
                .unwrap_err()
                .is_not_found()
        );
        drop(guard);
        result
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
    });
}

// Spec: docs/specs/state.md#configuration
// Writers bind with a primary checkout hint, and a hint update replaces
// the record by rename while others read it. No reader or binder may see
// that replacement as invalid state.
#[test]
fn concurrent_first_bindings_with_hints_never_read_invalid_state() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);
    let common = base.join("repo/.git");
    fs::create_dir_all(&common).unwrap();
    let checkouts = [base.join("repo"), base.join("moved")];
    let plain = ProjectLocation::new(&common).unwrap();
    let threads = 12;
    let rounds = 40;
    let barrier = std::sync::Barrier::new(threads);
    let results = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|worker| {
                let (roots, plain, barrier) = (&roots, &plain, &barrier);
                let checkouts = &checkouts;
                let common = &common;
                scope.spawn(move || -> Result<Vec<ProjectId>, ConfigError> {
                    barrier.wait();
                    let mut seen = Vec::new();
                    for round in 0..rounds {
                        let binding = match (worker + round) % 4 {
                            // The hint moves every few rounds, so binders
                            // replace the record while others read it.
                            0 | 1 => bind_project(
                                roots,
                                ProjectLocation::from_verified_primary_checkout(
                                    common,
                                    &checkouts[(round / 4 + worker % 2) % 2],
                                )?,
                            )?,
                            2 => bind_project(roots, plain.clone())?,
                            _ => match lookup_project(roots, plain)? {
                                Some(binding) => binding,
                                None => continue,
                            },
                        };
                        seen.push(binding.project_id());
                        // GC reads the same record together with its metadata.
                        let _ = lookup_gc_project(roots, binding.project_id())?;
                        for listed in registered_projects(roots)? {
                            seen.push(listed.project_id());
                        }
                    }
                    Ok(seen)
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let expected = bind_project(&roots, plain.clone()).unwrap().project_id();
    for result in results {
        let seen = result.expect("a concurrent binder or reader saw invalid state");
        assert!(seen.iter().all(|project| *project == expected));
    }
    assert_eq!(registered_projects(&roots).unwrap().len(), 1);
}

#[test]
fn bind_project_assigns_distinct_ids_to_distinct_common_directories() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);
    let first_location = base.join("one/.git");
    let second_location = base.join("two/.git");
    fs::create_dir_all(&first_location).unwrap();
    fs::create_dir_all(&second_location).unwrap();

    let first = bind_project(&roots, ProjectLocation::new(first_location).unwrap()).unwrap();
    let second = bind_project(&roots, ProjectLocation::new(second_location).unwrap()).unwrap();

    assert_ne!(first.project, second.project);
    fs::set_permissions(roots.state.as_path(), fs::Permissions::from_mode(0o700)).unwrap();
    let listed = registered_projects(&roots).unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&first));
    assert!(listed.contains(&second));
}

#[test]
fn unrelated_duplicate_binding_does_not_block_lookup_or_bind() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);
    let target_dir = base.join("target/.git");
    let duplicated_dir = base.join("duplicated/.git");
    let new_dir = base.join("new/.git");
    fs::create_dir_all(&target_dir).unwrap();
    fs::create_dir_all(&duplicated_dir).unwrap();
    fs::create_dir_all(&new_dir).unwrap();

    let target = bind_project(&roots, ProjectLocation::new(&target_dir).unwrap()).unwrap();
    let duplicated = bind_project(&roots, ProjectLocation::new(&duplicated_dir).unwrap()).unwrap();
    fs::set_permissions(roots.state.as_path(), fs::Permissions::from_mode(0o700)).unwrap();
    let projects = roots.state.as_path().join(PROJECTS_DIR);
    let duplicate_id = ProjectId::from_bytes([11; 16]);
    let duplicate = projects.join(duplicate_id.to_string());
    fs::create_dir(&duplicate).unwrap();
    fs::set_permissions(&duplicate, fs::Permissions::from_mode(0o700)).unwrap();
    let record = ProjectBindingRecord {
        schema_version: 1,
        project: duplicate_id,
        canonical_common_dir: encode_location(&duplicated_dir),
        canonical_primary_checkout_hint: None,
        extra: Default::default(),
    };
    fs::write(
        duplicate.join(RECORD),
        provenant_records::encode(&record).unwrap(),
    )
    .unwrap();
    fs::set_permissions(duplicate.join(RECORD), fs::Permissions::from_mode(0o600)).unwrap();
    assert_ne!(target.project_id(), duplicate_id);
    assert_ne!(duplicated.project_id(), duplicate_id);
    assert_eq!(registered_projects(&roots).unwrap().len(), 3);

    assert_eq!(
        lookup_project(&roots, &ProjectLocation::new(&target_dir).unwrap()).unwrap(),
        Some(target)
    );
    let new = bind_project(&roots, ProjectLocation::new(&new_dir).unwrap()).unwrap();
    assert_eq!(new.common_dir(), new_dir);
    assert_ne!(duplicated.project_id(), duplicate_id);

    let duplicate_target_id = ProjectId::from_bytes([12; 16]);
    let duplicate_target = projects.join(duplicate_target_id.to_string());
    fs::create_dir(&duplicate_target).unwrap();
    fs::set_permissions(&duplicate_target, fs::Permissions::from_mode(0o700)).unwrap();
    let record = ProjectBindingRecord {
        schema_version: 1,
        project: duplicate_target_id,
        canonical_common_dir: encode_location(&target_dir),
        canonical_primary_checkout_hint: None,
        extra: Default::default(),
    };
    fs::write(
        duplicate_target.join(RECORD),
        provenant_records::encode(&record).unwrap(),
    )
    .unwrap();
    fs::set_permissions(
        duplicate_target.join(RECORD),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let target_location = ProjectLocation::new(&target_dir).unwrap();
    assert!(lookup_project(&roots, &target_location).is_err());
    assert!(bind_project(&roots, target_location).is_err());
}

#[test]
fn project_binding_record_preserves_unknown_fields_with_bounded_decode() {
    let key = format!("future_{}", "field");
    let project = ProjectId::from_bytes([24; 16]);
    let mut payload = serde_json::json!({
        "schema_version": 1,
        "project": project,
        "canonical_common_dir": "2f746d702f7265706f",
    });
    let unknown = serde_json::json!({"nested": [true, 7]});
    payload
        .as_object_mut()
        .unwrap()
        .insert(key.clone(), unknown.clone());
    let bytes = serde_json::to_vec(&payload).unwrap();
    let decoded = decode_binding_record(&bytes).unwrap();
    assert_eq!(decoded.extra.get(&key), Some(&unknown));

    let encoded = provenant_records::encode(&decoded).unwrap();
    let round_trip = decode_binding_record(&encoded).unwrap();
    assert_eq!(round_trip.extra.get(&key), Some(&unknown));
    assert_eq!(round_trip.project, project);
}

// Spec: docs/specs/run-git.md#project-layout-ownership
#[test]
fn verified_checkout_hint_is_additive_to_the_common_directory_identity() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);
    let common = base.join("metadata/external.git");
    let checkout = base.join("checkout");
    fs::create_dir_all(&common).unwrap();
    fs::create_dir_all(&checkout).unwrap();
    // Persistent project bindings are reserved for Git-backed locations.
    // This registry-layer test supplies the minimal common-dir marker;
    // Git ownership of the checkout hint is proved by confinement callers.
    fs::write(common.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    fs::create_dir(common.join("objects")).unwrap();

    let before = bind_project(&roots, ProjectLocation::new(&common).unwrap()).unwrap();
    assert_eq!(before.primary_checkout_hint(), None);
    let updated = bind_project(
        &roots,
        ProjectLocation::from_verified_primary_checkout(&common, &checkout).unwrap(),
    )
    .unwrap();

    assert_eq!(updated.project_id(), before.project_id());
    assert_eq!(updated.common_dir(), before.common_dir());
    assert_eq!(updated.primary_checkout_hint(), Some(checkout.as_path()));
    assert_eq!(
        registered_projects(&roots).unwrap()[0].primary_checkout_hint(),
        Some(checkout.as_path())
    );
    assert_eq!(
        lookup_project(&roots, &ProjectLocation::new(&common).unwrap())
            .unwrap()
            .unwrap()
            .primary_checkout_hint(),
        Some(checkout.as_path())
    );
}

#[test]
fn project_location_rejects_noncanonical_path_shapes() {
    for path in [
        PathBuf::from("relative"),
        PathBuf::from("/"),
        PathBuf::from("/a/../b"),
    ] {
        assert!(ProjectLocation::new(path).is_err());
    }
}

#[test]
fn bind_project_ignores_a_stage_symlink_without_touching_its_target() {
    let temp = tempdir();
    let roots = roots(&temp);
    let base = canonical_base(&temp);
    let projects = roots.state.as_path().join(PROJECTS_DIR);
    fs::create_dir(&projects).unwrap();
    fs::set_permissions(&projects, fs::Permissions::from_mode(0o700)).unwrap();

    let outside = base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"outside contents").unwrap();
    let project = ProjectId::from_bytes([9; 16]);
    let stage = projects.join(format!("{STAGE_PREFIX}{project}"));
    std::os::unix::fs::symlink(&outside, &stage).unwrap();

    let location = ProjectLocation::new(base.join("repository/.git")).unwrap();
    fs::create_dir_all(location.common_dir()).unwrap();

    assert!(bind_project(&roots, location).is_ok());
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"outside contents");
    assert!(
        fs::symlink_metadata(&stage)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn permission_error_is_unreachable_not_present() {
    let now = WallTime::from_unix_seconds(1_700_000_000).unwrap();
    let retry_at = now
        .checked_add(DurationMicros::from_seconds(PROJECT_RETRY_SECONDS).unwrap())
        .unwrap();

    // A permission or I/O failure is scheduled for a bounded retry, not
    // read as a false `Present` that keeps a vanished checkout on the tick.
    let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    assert_eq!(
        classify_project_presence(Err(denied), now, retry_at),
        ProjectPresence::Unreachable {
            error: "denied".to_owned(),
            retry_at,
        }
    );
    let missing = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
    assert_eq!(
        classify_project_presence(Err(missing), now, retry_at),
        ProjectPresence::Absent { since: now }
    );
}

#[test]
fn transient_project_failure_keeps_retry_deadline_until_recovery() {
    let temp = tempdir();
    let roots = roots(&temp);
    let common = canonical_base(&temp).join("repository/.git");
    fs::create_dir_all(common.join("objects")).unwrap();
    fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
    let binding = bind_project(&roots, ProjectLocation::new(common.clone()).unwrap()).unwrap();
    let retry_after = DurationMicros::from_seconds(PROJECT_RETRY_SECONDS).unwrap();
    let now = WallTime::from_unix_seconds(1_700_000_000).unwrap();
    let retry_at = now.checked_add(retry_after).unwrap();
    let mut probes = 0;

    let (first, incarnation) =
        project_presence_with_probe(&roots, &binding, now, retry_after, |_| {
            probes += 1;
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "temporarily unavailable",
            ))
        })
        .unwrap();
    assert_eq!(
        first,
        ProjectPresence::Unreachable {
            error: "temporarily unavailable".to_owned(),
            retry_at,
        }
    );

    let (between_ticks, next_incarnation) = project_presence_with_probe(
        &roots,
        &binding,
        now.checked_add(DurationMicros::from_seconds(10).unwrap())
            .unwrap(),
        retry_after,
        |_| panic!("the registered retry deadline has not elapsed"),
    )
    .unwrap();
    assert_eq!(between_ticks, first);
    assert_eq!(next_incarnation, incarnation);
    assert_eq!(probes, 1);

    let oversized_error = format!("{}édiscarded", "\u{1}".repeat(510));
    let bounded_error = format!("{}é", "\u{1}".repeat(510));
    let (second_failure, second_incarnation) =
        project_presence_with_probe(&roots, &binding, retry_at, retry_after, |_| {
            probes += 1;
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                oversized_error,
            ))
        })
        .unwrap();
    let second_retry_at = retry_at.checked_add(retry_after).unwrap();
    assert!(matches!(
        second_failure,
        ProjectPresence::Unreachable {
            retry_at: next_retry,
            ..
        } if next_retry == second_retry_at
    ));
    assert_eq!(second_incarnation, incarnation);
    assert_eq!(probes, 2);

    let retry_record_path =
        project_binding_dir(roots.state.as_path(), binding.project_id()).join(PROJECT_RETRY_RECORD);
    let retry_record_bytes = fs::read(&retry_record_path).unwrap();
    assert!(retry_record_bytes.len() <= PROJECT_RETRY_RECORD_LIMIT as usize);
    assert!(
        retry_record_bytes.len() > 3_000,
        "JSON escaping remains bounded"
    );
    let saved_retry: ProjectRetryRecord = provenant_records::decode(
        retry_record_bytes.as_slice(),
        provenant_records::Limits {
            max_bytes: PROJECT_RETRY_RECORD_LIMIT,
            ..provenant_records::Limits::CONFIG
        },
    )
    .unwrap();
    assert_eq!(saved_retry.error, bounded_error);
    assert_eq!(saved_retry.error.len(), PROJECT_RETRY_ERROR_BYTES);

    let (between_second_retry, _) = project_presence_with_probe(
        &roots,
        &binding,
        retry_at
            .checked_add(DurationMicros::from_seconds(10).unwrap())
            .unwrap(),
        retry_after,
        |_| panic!("the second retry deadline has not elapsed"),
    )
    .unwrap();
    assert_eq!(
        between_second_retry,
        ProjectPresence::Unreachable {
            error: bounded_error,
            retry_at: second_retry_at,
        }
    );

    let (recovered, final_incarnation) =
        project_presence_with_probe(&roots, &binding, second_retry_at, retry_after, |path| {
            probes += 1;
            fs::metadata(path)
        })
        .unwrap();
    assert_eq!(recovered, ProjectPresence::Present);
    assert_eq!(final_incarnation, incarnation);
    assert_eq!(probes, 3);
    assert!(
        !project_binding_dir(roots.state.as_path(), binding.project_id())
            .join(PROJECT_RETRY_RECORD)
            .exists()
    );
}

#[test]
fn absent_project_warns_once_per_incarnation() {
    let temp = tempdir();
    let roots = roots(&temp);
    let common = canonical_base(&temp).join("repository/.git");
    fs::create_dir_all(common.join("objects")).unwrap();
    fs::write(common.join("HEAD"), "ref: refs/heads/main").unwrap();
    let binding = bind_project(&roots, ProjectLocation::new(common.clone()).unwrap()).unwrap();
    let now = WallTime::from_unix_seconds(1_700_000_000).unwrap();
    let retry = DurationMicros::from_seconds(PROJECT_RETRY_SECONDS).unwrap();
    let (_, incarnation) = project_presence(&roots, &binding, now, retry).unwrap();

    // An absent checkout is reported with the same key on every
    // observation, so a once-per-change warning sink shows it once.
    fs::remove_dir_all(&common).unwrap();
    let (absent, second) = project_presence(&roots, &binding, now, retry).unwrap();
    assert_eq!(absent, ProjectPresence::Absent { since: now });
    assert_eq!(incarnation.key(), second.key());

    // Replacing the registration record produces a new incarnation and a
    // new report for the same path.
    let record = project_binding_dir(roots.state.as_path(), binding.project_id()).join(RECORD);
    fs::File::open(&record)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(123))
        .unwrap();
    let (_, third) = project_presence(&roots, &binding, now, retry).unwrap();
    assert_ne!(incarnation.key(), third.key());
}
