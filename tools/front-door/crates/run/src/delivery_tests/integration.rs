use super::*;

#[test]
fn terminal_settles_a_private_rewind() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let previous = fixture.prepare()?.head(&fixture.writer)?;
    fixture.git(&[
        "update-ref",
        "refs/remotes/origin/main",
        &previous.to_string(),
    ])?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ])?;
    fixture.commit(&fixture.writer.worktree, "new writer base")?;
    let review = fixture.terminal(|| {
        let attempt = fixture.root.join("attempt");
        let scratch = attempt.join("tmp/candidate-0");
        std::fs::create_dir_all(&scratch)?;
        let executable = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        let runtime =
            provenant_confinement::UtilityRuntime::new(&executable, fixture.root.join("home"))?;
        let repo = runtime.open_repo(
            &SystemExecutor::new(1)?,
            &fixture.writer.project,
            provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
            &provenant_core::CancelToken::new(),
        )?;
        let worktree = provenant_confinement::verify_worktree(&repo, &fixture.writer.worktree)?;
        let base = crate::objects::resolve_ref(&executable, worktree.repo(), "refs/heads/lane")?;
        crate::objects::prepare_bound(
            &executable,
            worktree.repo(),
            &Directory::open_anchor(&attempt)?,
            &scratch,
            "refs/heads/lane",
            base,
        )?;
        std::fs::write(
            scratch.join("git-common/refs/heads/lane"),
            format!("{previous}\n"),
        )?;
        Ok(())
    })?;
    assert!(!review);
    assert!(
        fixture
            .root
            .join("attempt/quarantine/manifest.json")
            .is_file()
    );
    assert!(fixture.root.join("attempt/writer-terminal.json").exists());
    assert_eq!(fixture.prepare()?.head(&fixture.writer)?, previous);
    Ok(())
}

#[test]
fn delivery_refuses_corrupt_reachable_content_before_log_or_integration()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    std::fs::write(fixture.writer.worktree.join("blob"), "original content\n")?;
    fixture.lane_git(&["add", "blob"])?;
    fixture.commit(&fixture.writer.worktree, "lane blob")?;
    let head = host.head(&fixture.writer)?;
    let blob = host.command(
        &fixture.writer.worktree,
        "git",
        &["rev-parse", "HEAD:blob"],
        WriterDeliveryStage::Head,
    )?;
    let object = fixture
        .writer
        .project
        .join(".git/objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o600))?;
    std::fs::write(object, "corrupt object content")?;
    assert!(matches!(
        host.head(&fixture.writer),
        Err(RunError::Delivery(WriterDeliveryStage::Head))
    ));
    assert!(matches!(
        host.commit_body(&fixture.writer, head, "delivery"),
        Err(RunError::Delivery(WriterDeliveryStage::Head))
    ));
    fixture.git(&["branch", "integration"])?;
    let request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    assert!(matches!(
        host.integrate(&fixture.writer, &request, head),
        Err(RunError::Delivery(WriterDeliveryStage::Integrate))
    ));
    Ok(())
}

#[test]
fn integration_refreshes_forged_tracking_ref_and_fails_closed()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["push", "origin", "HEAD:refs/heads/integration"])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
    )?
    .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "lane change")?;
    let head = host.head(&fixture.writer)?;
    fixture.git(&[
        "update-ref",
        "refs/remotes/origin/integration",
        &head.to_string(),
    ])?;
    host.integrate(&fixture.writer, &fixture.request, head)?;
    let remote = host.command(
        &fixture.writer.worktree,
        "git",
        &[
            "ls-remote",
            "--exit-code",
            "origin",
            "refs/heads/integration",
        ],
        WriterDeliveryStage::Integrate,
    )?;
    assert!(remote.starts_with(&head.to_string()));

    fixture.commit(&fixture.writer.worktree, "another lane change")?;
    let next = host.head(&fixture.writer)?;
    fixture.git(&["push", "origin", &format!("{next}:refs/heads/integration")])?;
    // Restore the stale local value so fetch must update the locked ref.
    fixture.git(&[
        "update-ref",
        "refs/remotes/origin/integration",
        &head.to_string(),
    ])?;
    let remote = host.command(
        &fixture.writer.worktree,
        "git",
        &[
            "ls-remote",
            "--exit-code",
            "origin",
            "refs/heads/integration",
        ],
        WriterDeliveryStage::Integrate,
    )?;
    let lock = fixture
        .writer
        .project
        .join(".git/refs/remotes/origin/integration.lock");
    std::fs::write(lock, "stale lock")?;
    assert!(
        host.integrate(&fixture.writer, &fixture.request, next)
            .is_err()
    );
    assert_eq!(
        remote,
        host.command(
            &fixture.writer.worktree,
            "git",
            &[
                "ls-remote",
                "--exit-code",
                "origin",
                "refs/heads/integration"
            ],
            WriterDeliveryStage::Integrate
        )?
    );
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)] // Concurrent requests exercise cross-thread flock contention.
fn overlapping_origin_requests_share_a_fetch_even_when_it_starts_later()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    let temp = tempfile::tempdir()?;
    let common = Directory::open_anchor(temp.path())?;
    let start = Barrier::new(8);
    let fetches = AtomicUsize::new(0);
    std::thread::scope(|scope| -> crate::Result<()> {
        let mut workers = Vec::new();
        for _ in 0..8 {
            let common = &common;
            let start = &start;
            let fetches = &fetches;
            workers.push(scope.spawn(move || {
                let requested = OriginRefreshRequest::capture()?;
                start.wait();
                coalesced_origin_refresh(common, &requested, &CancelToken::new(), || {
                    // Origin refresh must never need or retain the config lock.
                    let _config = common.try_lock(
                        std::ffi::OsStr::new("provenant-lane-setup.lock"),
                        provenant_os::fs::LockMode::Exclusive,
                    )?;
                    fetches.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            }));
        }
        for worker in workers {
            worker.join().expect("refresh request panicked")?;
        }
        Ok(())
    })?;
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    // A fetch starting after the request counts even if it finishes before
    // that caller reaches the fetch lock (e.g. during transport validation).
    let requested = OriginRefreshRequest::capture()?;
    coalesced_origin_refresh(&common, &requested, &CancelToken::new(), || {
        fetches.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })?;
    coalesced_origin_refresh(&common, &requested, &CancelToken::new(), || {
        panic!("newer fetch already satisfies this request")
    })?;
    assert_eq!(fetches.load(Ordering::SeqCst), 2);
    // A caller arriving during an in-flight fetch also reuses its completion,
    // even if transport checks delay its acquisition of the fetch lock.
    let mut during_fetch = None;
    coalesced_origin_refresh(
        &common,
        &OriginRefreshRequest::capture()?,
        &CancelToken::new(),
        || {
            fetches.fetch_add(1, Ordering::SeqCst);
            during_fetch = Some(OriginRefreshRequest::capture()?);
            Ok(())
        },
    )?;
    coalesced_origin_refresh(
        &common,
        &during_fetch.ok_or("request")?,
        &CancelToken::new(),
        || panic!("overlapping fetch already completed"),
    )?;
    assert_eq!(fetches.load(Ordering::SeqCst), 3);
    let _held = common.try_lock(
        std::ffi::OsStr::new("provenant-lane-origin-refresh.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let cancelled = CancelToken::new();
    cancelled.cancel();
    let error = coalesced_origin_refresh(
        &common,
        &OriginRefreshRequest::capture()?,
        &cancelled,
        || panic!("cancelled request must not fetch"),
    )
    .expect_err("cancelled lock wait must stop");
    assert!(matches!(
        error,
        RunError::Filesystem(provenant_os::fs::FsError::Cancelled { .. })
    ));
    Ok(())
}

#[test]
fn origin_refresh_lock_timeout_allows_launch_but_fetch_failure_is_reported()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{Clock, WaitBudget};
    use provenant_os::{clock::SystemClock, fs::LockMode};
    let temp = tempfile::tempdir()?;
    let common = Directory::open_anchor(temp.path())?;
    let held = common.try_lock(
        std::ffi::OsStr::new("provenant-lane-origin-refresh.lock"),
        LockMode::Exclusive,
    )?;
    let requested = OriginRefreshRequest::capture()?;
    let cancel = CancelToken::new();
    let immediate = || WaitBudget::nonblocking(SystemClock::new().mono_now());
    coalesced_origin_refresh_with_budget(&common, &requested, &cancel, immediate(), || {
        panic!("busy refresh should use existing tracking refs")
    })?;
    drop(held);
    let error =
        coalesced_origin_refresh_with_budget(&common, &requested, &cancel, immediate(), || {
            Err(prepare_error("fetch", "remote unavailable"))
        })
        .expect_err("a failed fetch must retain its error");
    assert!(
        matches!(error, RunError::DeliveryPrepare { step: "fetch", reason } if reason == "remote unavailable")
    );
    Ok(())
}

#[test]
fn origin_refresh_ref_collision_does_not_refuse_launch_or_hide_transport_failure()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = OriginFixture::new()?;
    let previous = fixture.local(&["rev-parse", "refs/remotes/origin/main"])?;
    let newer = fixture.commit("newer")?;
    fixture.advertise(&[("main", &newer)], "main")?;
    let lock = fixture.project.join(".git/refs/remotes/origin/main.lock");
    std::fs::write(&lock, b"external Git transaction")?;
    fixture.refresh(None)?;
    assert_eq!(
        fixture.local(&["rev-parse", "refs/remotes/origin/main"])?,
        previous
    );
    assert_eq!(std::fs::read(&lock)?, b"external Git transaction");
    std::fs::remove_file(&lock)?;
    fixture.refresh(None)?;
    assert_eq!(
        fixture.local(&["rev-parse", "refs/remotes/origin/main"])?,
        newer
    );
    fixture.local(&[
        "remote",
        "set-url",
        "origin",
        fixture.root.join("absent.git").to_str().ok_or("path")?,
    ])?;
    assert!(matches!(
        fixture.refresh(None),
        Err(RunError::DeliveryPrepare { .. })
    ));
    Ok(())
}

#[test]
fn origin_refresh_updates_tracking_refs_and_reports_fetch_failure()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let project = root.join("project");
    let home = root.join("home");
    let remote = root.join("remote.git");
    std::fs::create_dir(&project)?;
    std::fs::create_dir(&home)?;
    let git_dir = project.join(".git");
    let git = provenant_os::git::git_executable().ok_or("git unavailable")?;
    let run = |directory: &Path,
               args: &[&str]|
     -> std::result::Result<String, Box<dyn std::error::Error>> {
        let output = provenant_os::fixture::git_command_at(&git)
            .env_clear()
            .envs(provenant_os::fixture::fixture_git_settings())
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_DIR", directory)
            .env("GIT_WORK_TREE", &project)
            .current_dir(&project)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    };
    run(&git_dir, &["init", "-q"])?;
    run(&git_dir, &["commit", "--allow-empty", "-qm", "initial"])?;
    run(&remote, &["init", "-q"])?;
    run(&remote, &["config", "--local", "core.bare", "true"])?;
    run(
        &git_dir,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().ok_or("remote path")?,
        ],
    )?;
    run(&git_dir, &["push", "-q", "origin", "HEAD:refs/heads/main"])?;
    let expected = run(&git_dir, &["rev-parse", "HEAD"])?;
    run(&git_dir, &["update-ref", "-d", "refs/remotes/origin/main"])?;
    let writer = WriterRequest {
        extra: Default::default(),
        project: project.clone(),
        worktree: project.clone(),
    };
    let host = || {
        HostDeliveryContext::new(vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), home.as_os_str().to_owned()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ])
    };
    host().refresh_origin_with(&writer, &git_dir)?;
    assert_eq!(
        run(&git_dir, &["rev-parse", "refs/remotes/origin/main"])?,
        expected
    );
    // Packed refs let the fixture advertise both spellings even on a
    // case-insensitive host. The new commit makes an accidental fetch visible.
    run(&git_dir, &["commit", "--allow-empty", "-qm", "newer"])?;
    let newer = run(&git_dir, &["rev-parse", "HEAD"])?;
    run(&remote, &["update-ref", "-d", "refs/heads/main"])?;
    std::fs::write(
        remote.join("packed-refs"),
        format!(
            "# pack-refs with: sorted\n{newer} refs/heads/Main\n{newer} refs/heads/feat+x\n{newer} refs/heads/main\n{newer} refs/heads/other\n"
        ),
    )?;
    run(&remote, &["symbolic-ref", "HEAD", "refs/heads/other"])?;
    host().refresh_origin_with(&writer, &git_dir)?;
    assert_eq!(
        run(&git_dir, &["rev-parse", "refs/remotes/origin/main"])?,
        expected
    );
    assert_eq!(
        run(&git_dir, &["rev-parse", "refs/remotes/origin/other"])?,
        newer
    );
    assert_eq!(
        run(&git_dir, &["rev-parse", "refs/remotes/origin/feat+x"])?,
        newer
    );
    run(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"])?;
    let Err(error) = host().refresh_origin_with(&writer, &git_dir) else {
        return Err("colliding default must fail".into());
    };
    assert!(matches!(error, RunError::OriginDefaultCollision));
    assert_eq!(
        run(&git_dir, &["rev-parse", "refs/remotes/origin/main"])?,
        expected
    );
    run(
        &git_dir,
        &[
            "remote",
            "set-url",
            "origin",
            root.join("missing.git").to_str().ok_or("missing path")?,
        ],
    )?;
    assert!(host().refresh_origin_with(&writer, &git_dir).is_err());
    assert_eq!(
        run(&git_dir, &["rev-parse", "refs/remotes/origin/main"])?,
        expected
    );
    Ok(())
}

#[test]
fn origin_refresh_never_writes_through_head_or_a_tracking_symref()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = OriginFixture::new()?;
    let kept = fixture.local(&["rev-parse", "refs/remotes/origin/main"])?;
    let newer = fixture.commit("newer")?;
    // A local tracking symref other than HEAD, as `git remote set-head` or a
    // user's alias would leave behind.
    fixture.local(&[
        "symbolic-ref",
        "refs/remotes/origin/alias",
        "refs/remotes/origin/main",
    ])?;
    // Folding HEAD: `head` aliases origin/HEAD on a case-insensitive host,
    // where writing it would move origin/main through the symref, and so does
    // `Head/x` (a path below it). Each is advertised alone so that collisions
    // among the advertised names cannot hide the reserved name.
    for aliasing in ["head", "Head/x"] {
        fixture.advertise(
            &[
                ("main", &kept),
                (aliasing, &newer),
                ("alias", &newer),
                ("other", &newer),
            ],
            "main",
        )?;
        fixture.refresh(None)?;
        assert_eq!(
            fixture.local(&["rev-parse", "refs/remotes/origin/main"])?,
            kept,
            "{aliasing}"
        );
        assert_eq!(
            fixture.local(&["rev-parse", "refs/remotes/origin/other"])?,
            newer
        );
        for symref in ["HEAD", "alias"] {
            assert_eq!(
                fixture.local(&["symbolic-ref", &format!("refs/remotes/origin/{symref}")])?,
                "refs/remotes/origin/main",
                "{aliasing}: {symref} must stay a symref to main"
            );
        }
        let reference = format!("refs/remotes/origin/{aliasing}");
        let exists = fixture.local(&["rev-parse", "--verify", "--quiet", &reference]);
        // On a case-insensitive host `head` names HEAD, which resolves to main.
        assert!(
            exists.as_ref().is_err() || exists.as_ref().is_ok_and(|oid| *oid == kept),
            "{aliasing}: {exists:?}"
        );
        fixture.local(&["update-ref", "-d", "refs/remotes/origin/other"])?;
    }

    // HEAD stays reserved even where no origin/HEAD symref exists yet: a later
    // `git remote set-head origin -a` must be able to create it.
    fixture.local(&["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"])?;
    fixture.advertise(
        &[("main", &kept), ("head", &newer), ("other", &newer)],
        "main",
    )?;
    fixture.refresh(None)?;
    assert!(
        fixture
            .local(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/head"
            ])
            .is_err(),
        "a branch named head must not be fetched"
    );
    assert!(
        !fixture
            .project
            .join(".git/refs/remotes/origin/head")
            .exists()
    );
    assert_eq!(
        fixture.local(&["rev-parse", "refs/remotes/origin/other"])?,
        newer
    );
    fixture.local(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ])?;

    // The advertised default itself aliasing HEAD cannot be refreshed.
    fixture.advertise(&[("main", &kept), ("head", &newer)], "head")?;
    let error = fixture
        .refresh(None)
        .err()
        .ok_or("default `head` must fail")?;
    assert!(matches!(error, RunError::OriginDefaultCollision), "{error}");
    assert_eq!(
        fixture.local(&["rev-parse", "refs/remotes/origin/main"])?,
        kept
    );
    Ok(())
}

#[test]
fn origin_refresh_refuses_names_that_differ_only_in_unicode_normalisation()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = OriginFixture::new()?;
    let kept = fixture.local(&["rev-parse", "refs/remotes/origin/main"])?;
    let newer = fixture.commit("newer")?;
    let (nfc, nfd) = ("caf\u{e9}", "cafe\u{301}");
    let assert_untouched = |tracking: &str| -> std::result::Result<(), Box<dyn std::error::Error>> {
        assert_eq!(fixture.local(&["rev-parse", tracking])?, kept, "{tracking}");
        // Behind the symref: Git may have stored either spelling of the name.
        assert_eq!(
            fixture.local(&["rev-parse", "refs/remotes/origin/HEAD"])?,
            kept
        );
        Ok(())
    };

    // An NFD default, with `origin/HEAD` pointing at its tracking ref, and an
    // NFC sibling at another commit. With precomposed Unicode both would land
    // on one tracking ref, writing the sibling's commit behind `origin/HEAD`.
    let default_ref = format!("refs/remotes/origin/{nfd}");
    fixture.local(&["update-ref", &default_ref, &kept])?;
    fixture.local(&["symbolic-ref", "refs/remotes/origin/HEAD", &default_ref])?;
    fixture.advertise(&[(nfd, &kept), (nfc, &newer), ("other", &newer)], nfd)?;
    let error = fixture
        .refresh(None)
        .err()
        .ok_or("NFD default with an NFC sibling must be refused")?;
    assert!(
        matches!(error, RunError::OriginNormalisationCollision(_)),
        "{error}"
    );
    let error = error.to_string();
    for spelling in ["caf\\u{e9}", "cafe\\u{301}"] {
        assert!(error.contains(spelling), "{spelling} missing: {error}");
    }
    assert_untouched(&default_ref)?;
    // The refusal comes before any fetch, so unrelated branches stay put too.
    assert!(
        fixture
            .local(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/other"
            ])
            .is_err()
    );

    // The same pair, with the NFC spelling as the default and a case change.
    fixture.advertise(&[(nfd, &kept), ("Caf\u{c9}", &newer)], nfd)?;
    assert!(fixture.refresh(None).is_err());
    assert_untouched(&default_ref)?;

    // Two siblings that alias each other are refused as well, even where the
    // default is unaffected.
    fixture.advertise(&[("main", &kept), (nfd, &newer), (nfc, &newer)], "main")?;
    assert!(fixture.refresh(None).is_err());

    // A tracking ref already on disk under the other spelling is a destination
    // the advertised name would write through. Packed refs keep the stored
    // spelling on any filesystem.
    // Delete the loose ref first: deleting a ref also drops it from packed refs.
    fixture.local(&["update-ref", "-d", &default_ref])?;
    let local_packed = fixture.project.join(".git/packed-refs");
    std::fs::write(
        &local_packed,
        format!("# pack-refs with: sorted\n{kept} refs/remotes/origin/{nfd}\n"),
    )?;
    fixture.advertise(&[("main", &kept), (nfc, &newer)], "main")?;
    let error = fixture
        .refresh(None)
        .err()
        .ok_or("an existing NFD tracking ref must refuse an NFC branch")?
        .to_string();
    assert!(error.contains("cafe\\u{301}"), "{error}");
    std::fs::remove_file(&local_packed)?;

    // Plain spellings, and names that differ only by case, still refresh.
    fixture.advertise(&[("main", &kept), (nfc, &newer), ("other", &newer)], "main")?;
    fixture.refresh(None)?;
    assert_eq!(
        fixture.local(&["rev-parse", &format!("refs/remotes/origin/{nfc}")])?,
        newer
    );
    Ok(())
}

#[test]
fn origin_refresh_routes_advertisement_and_fetch_through_one_hardened_transport()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let fixture = OriginFixture::new()?;
    let newer = fixture.commit("newer")?;
    fixture.advertise(&[("main", &newer), ("other", &newer)], "main")?;
    // A recording stand-in for Git, outside the project.
    let bin = fixture.root.join("bin");
    std::fs::create_dir(&bin)?;
    let log = fixture.root.join("calls.log");
    let wrapper = bin.join("git-recorder");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            fixture.git.display()
        ),
    )?;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
    fixture.refresh(Some(&wrapper))?;
    let calls = std::fs::read_to_string(&log)?;
    for subcommand in ["ls-remote", "fetch"] {
        let call = calls
            .lines()
            .find(|line| line.contains(&format!(" {subcommand} ")))
            .ok_or_else(|| format!("no {subcommand} call in {calls}"))?;
        for setting in [
            "core.hooksPath=/dev/null",
            "core.fsmonitor=false",
            "protocol.allow=never",
            "protocol.ssh.allow=always",
            "protocol.https.allow=always",
            "protocol.http.allow=always",
            "protocol.file.allow=always",
        ] {
            assert!(
                call.contains(setting),
                "{subcommand} lacks {setting}: {call}"
            );
        }
    }
    // Fetches precompose Unicode so NFC and NFD spellings of a branch name
    // land on one tracking ref (which collision detection then sees) without
    // a normalisation dependency here.
    let fetch = calls
        .lines()
        .find(|line| line.contains(" fetch "))
        .ok_or("no fetch call")?;
    assert!(fetch.contains("core.precomposeunicode=true"), "{fetch}");

    // A lane-writable program is refused before any advertisement or fetch.
    let lane_area = fixture.project.join(".worktrees/lane-1");
    std::fs::create_dir_all(&lane_area)?;
    let lane_helper = lane_area.join("ssh");
    std::fs::write(&lane_helper, "#!/bin/sh\nexit 1\n")?;
    std::fs::set_permissions(&lane_helper, std::fs::Permissions::from_mode(0o755))?;
    fixture.local(&[
        "config",
        "core.sshCommand",
        lane_helper.to_str().ok_or("path")?,
    ])?;
    std::fs::write(&log, "")?;
    // `validate_transport_helpers` already refuses core.sshCommand for
    // delivery; the transport check is what covers the other programs.
    fixture.local(&["config", "--unset", "core.sshCommand"])?;
    fixture.local(&[
        "config",
        "remote.origin.uploadpack",
        lane_helper.to_str().ok_or("path")?,
    ])?;
    let error = fixture
        .refresh(Some(&wrapper))
        .err()
        .ok_or("lane helper must fail")?;
    assert!(
        matches!(
            error,
            RunError::DeliveryTransport {
                kind: crate::TransportRefusal::LaneProgram,
                ..
            }
        ),
        "{error}"
    );
    let calls = std::fs::read_to_string(&log)?;
    assert!(
        !calls
            .lines()
            .any(|line| line.contains(" ls-remote ") || line.contains(" fetch ")),
        "no network command may run: {calls}"
    );
    Ok(())
}

#[test]
fn origin_refresh_refuses_includes_that_name_a_lane_area_before_they_exist()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = OriginFixture::new()?;
    let newer = fixture.commit("newer")?;
    fixture.advertise(&[("main", &newer), ("other", &newer)], "main")?;
    let lane_file = fixture.project.join(".worktrees/lane-1/not-yet.cfg");
    let lane_file = lane_file.to_str().ok_or("path")?;
    let shared = fixture.root.join("shared.cfg");
    // Git reads none of these targets today: the file is missing, or the
    // `includeIf` condition is false. A lane could create the file, or make
    // the condition true, before Git rereads the configuration for the fetch.
    for (key, value) in [
        ("include.path", lane_file),
        ("include.path", "../.worktrees/lane-1/not-yet.cfg"),
        ("includeIf.onbranch:never-checked-out.path", lane_file),
        (
            "includeIf.gitdir:/nowhere/.path",
            "../.worktrees/lane-1/not-yet.cfg",
        ),
        ("includeIf.hasconfig:remote.*.url:never://*.path", lane_file),
    ] {
        fixture.local(&["config", key, value])?;
        let error = fixture
            .refresh(None)
            .err()
            .ok_or_else(|| format!("{key}={value} must be refused"))?;
        assert!(
            matches!(
                error,
                RunError::DeliveryTransport {
                    kind: crate::TransportRefusal::LaneConfig,
                    ..
                }
            ),
            "{key}={value}: {error}"
        );
        fixture.local(&["config", "--unset", key])?;
    }
    // The same through a file outside the lane areas, which Git does read.
    std::fs::write(&shared, format!("[include]\n\tpath = {lane_file}\n"))?;
    fixture.local(&["config", "include.path", shared.to_str().ok_or("path")?])?;
    assert!(fixture.refresh(None).is_err());
    std::fs::write(&shared, "[user]\n\tname = Someone\n")?;
    fixture.refresh(None)?;
    Ok(())
}

#[test]
fn fetch_destinations_that_are_symrefs_are_refused()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    let worktree = &fixture.writer.worktree;
    fixture.git(&["push", "-q", "origin", "HEAD:refs/heads/lane-base"])?;
    fixture.git(&["fetch", "-q", "origin"])?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/lane-base",
    ])?;
    // The real ref and an absent one are fine; the symref is not, because a
    // fetch into it would move lane-base instead.
    for (reference, allowed) in [
        ("refs/remotes/origin/lane-base", true),
        ("refs/remotes/origin/absent", true),
        ("refs/remotes/origin/HEAD", false),
    ] {
        let result =
            host.refuse_symref_destination(worktree, reference, crate::WriterDeliveryStage::Fetch);
        assert_eq!(result.is_ok(), allowed, "{reference}: {result:?}");
    }
    Ok(())
}
