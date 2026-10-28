//! Host Git conformance with a linked worktree and a local bare remote.
use super::delivery::*;
use super::*;
use provenant_core::GitOid;
use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
use provenant_os::fs::Directory;
use std::{ffi::OsString, path::Path, time::Duration};

struct PrepareFixture {
    _temp: tempfile::TempDir,
    root: std::path::PathBuf,
    environment: Vec<(OsString, OsString)>,
    writer: WriterRequest,
    request: WriterDeliveryRequest,
}

#[test]
fn terminal_imports_private_objects_before_quarantine_reads_the_lane()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for corrupt in [false, true] {
        let fixture = PrepareFixture::new()?;
        let result = fixture.terminal(|| {
            let attempt_path = fixture.root.join("attempt");
            let scratch = attempt_path.join("tmp/candidate-0");
            let objects = scratch.join("git-objects");
            std::fs::create_dir_all(objects.join("info"))?;
            std::fs::write(
                objects.join("info/alternates"),
                format!(
                    "{}\n",
                    fixture.writer.project.join(".git/objects").display()
                ),
            )?;
            let executable = provenant_os::git::git_executable().ok_or("Git unavailable")?;
            let executor = SystemExecutor::new(1)?;
            let runtime =
                provenant_confinement::UtilityRuntime::new(&executable, fixture.root.join("home"))?;
            let repo = runtime.open_repo(
                &executor,
                &fixture.writer.project,
                provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?,
                &provenant_core::CancelToken::new(),
            )?;
            let worktree = provenant_confinement::verify_worktree(&repo, &fixture.writer.worktree)?;
            let base =
                crate::objects::resolve_ref(&executable, worktree.repo(), "refs/heads/lane")?;
            crate::objects::prepare_refs(
                &executable,
                worktree.repo(),
                &Directory::open_anchor(&scratch)?,
                "refs/heads/lane",
                base,
            )?;
            crate::objects::bind(&Directory::open_anchor(&attempt_path)?, &scratch)?;
            std::fs::write(
                fixture.writer.worktree.join("private.txt"),
                "private lane content\n",
            )?;
            for args in [
                vec!["add", "private.txt"],
                vec![
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "-qm",
                    "private commit",
                ],
            ] {
                let argv: Vec<OsString> = std::iter::once(executable.as_os_str().to_owned())
                    .chain(args.into_iter().map(Into::into))
                    .collect();
                let mut environment = fixture.environment.clone();
                environment.push((
                    "GIT_OBJECT_DIRECTORY".into(),
                    objects.clone().into_os_string(),
                ));
                environment.push((
                    "GIT_COMMON_DIR".into(),
                    scratch.join("git-common").into_os_string(),
                ));
                environment.push((
                    "GIT_DIR".into(),
                    scratch.join("git-common").into_os_string(),
                ));
                environment.push((
                    "GIT_WORK_TREE".into(),
                    fixture.writer.worktree.clone().into_os_string(),
                ));
                environment.push((
                    "GIT_INDEX_FILE".into(),
                    worktree
                        .repo()
                        .private_git_dir()
                        .join("index")
                        .into_os_string(),
                ));
                let mut options =
                    ExecOptions::new(&argv, &fixture.writer.worktree, Duration::from_secs(5));
                options.env = Some(&environment);
                let output = SystemExecutor::new(1)?.execute(options)?;
                assert_eq!(
                    output.termination,
                    Termination::Exit(0),
                    "{}",
                    String::from_utf8_lossy(&output.stderr.bytes)
                );
            }
            assert_eq!(
                crate::objects::resolve_ref(&executable, worktree.repo(), "refs/heads/lane")?,
                base
            );
            if corrupt {
                let fanout = std::fs::read_dir(&objects)?
                    .filter_map(|entry| entry.ok())
                    .find(|entry| entry.file_name().len() == 2)
                    .ok_or("loose fanout")?
                    .path();
                let object = std::fs::read_dir(fanout)?
                    .next()
                    .ok_or("loose object")??
                    .path();
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o600))?;
                std::fs::write(object, "corrupt")?;
            }
            Ok(())
        });
        if corrupt {
            assert!(result.is_err());
            assert!(!fixture.root.join("attempt/writer-terminal.json").exists());
        } else {
            assert!(!result?);
            fixture.lane_git(&["cat-file", "-e", "HEAD:private.txt"])?;
        }
    }
    Ok(())
}

#[test]
fn terminal_archives_a_verified_private_rewind_before_refusing_ancestry()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let previous = fixture.prepare()?.head(&fixture.writer)?;
    fixture.commit(&fixture.writer.worktree, "new writer base")?;
    let error = fixture
        .terminal(|| {
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
            let base =
                crate::objects::resolve_ref(&executable, worktree.repo(), "refs/heads/lane")?;
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
        })
        .expect_err("private rewind must fail ancestry certification");
    assert!(
        error
            .to_string()
            .contains("no longer descends from its recorded base"),
        "{error}"
    );
    assert!(
        fixture
            .root
            .join("attempt/quarantine/manifest.json")
            .is_file()
    );
    assert!(!fixture.root.join("attempt/writer-terminal.json").exists());
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
    assert!(host.head(&fixture.writer).is_err());
    assert!(host.commit_body(&fixture.writer, head, "delivery").is_err());
    fixture.git(&["branch", "integration"])?;
    let request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    assert!(host.integrate(&fixture.writer, &request, head).is_err());
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
#[allow(clippy::disallowed_methods)]
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
        let output = std::process::Command::new(&git)
            .env_clear()
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
    run(
        &git_dir,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    )?;
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
fn integration_refuses_default_fallbacks_remote_head_and_primary_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "-m", "primary-live"])?;
    for target in [
        "main",
        "master",
        "primary-live",
        "origin/main",
        "origin/primary-live",
    ] {
        fixture.request =
            HostDeliveryContext::destination(DeliveryMode::Integrate(target.into()), "lane")?
                .ok_or("destination")?;
        assert!(
            matches!(
                fixture.prepare(),
                Err(RunError::DeliveryPrepare {
                    step: "integration target",
                    ..
                })
            ),
            "{target}"
        );
    }
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ])?;
    for target in ["trunk", "origin/trunk"] {
        fixture.request =
            HostDeliveryContext::destination(DeliveryMode::Integrate(target.into()), "lane")?
                .ok_or("destination")?;
        assert!(
            matches!(
                fixture.prepare(),
                Err(RunError::DeliveryPrepare {
                    step: "integration target",
                    ..
                })
            ),
            "{target}"
        );
    }
    fixture.git(&["branch", "integration"])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "lane change")?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/integration",
    ])?;
    let head = host.head(&fixture.writer)?;
    assert!(matches!(
        host.integrate(&fixture.writer, &fixture.request, head),
        Err(RunError::Delivery(WriterDeliveryStage::Integrate))
    ));
    fixture.git(&[
        "symbolic-ref",
        "refs/heads/alias",
        "refs/heads/primary-live",
    ])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("alias".into()), "lane")?
            .ok_or("destination")?;
    assert!(matches!(
        fixture.prepare(),
        Err(RunError::DeliveryPrepare {
            step: "integration target",
            ..
        })
    ));
    Ok(())
}

#[test]
fn integration_refuses_the_default_branch_of_a_non_origin_remote()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "-m", "primary-live"])?;
    fixture.git(&[
        "remote",
        "add",
        "upstream",
        fixture.root.join("remote.git").to_str().ok_or("remote")?,
    ])?;
    for (remote, branch) in [("origin", "trunk"), ("upstream", "release")] {
        fixture.git(&[
            "symbolic-ref",
            &format!("refs/remotes/{remote}/HEAD"),
            &format!("refs/remotes/{remote}/{branch}"),
        ])?;
    }
    fixture.git(&["branch", "release"])?;
    fixture.git(&["config", "branch.release.remote", "upstream"])?;
    fixture.git(&["config", "branch.release.merge", "refs/heads/release"])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("release".into()), "lane")?
            .ok_or("destination")?;
    assert!(matches!(
        fixture.prepare(),
        Err(RunError::DeliveryPrepare {
            step: "integration target",
            ..
        })
    ));
    assert!(matches!(
        HostDeliveryContext::validate_integration_target(&fixture.writer.project, "release"),
        Err(RunError::DeliveryPrepare {
            step: "integration target",
            ..
        })
    ));
    Ok(())
}

#[test]
fn graph_publication_recovers_deleted_marked_ref_and_refuses_conflicts()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let publication = host.graph_publication_branch(&fixture.writer.project, head)?;
    let reference = format!("refs/heads/{publication}");
    fixture.git(&["update-ref", "-d", &reference, &head.to_string()])?;
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );
    assert_eq!(
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", "--verify", &reference],
            WriterDeliveryStage::Push
        )?,
        head.to_string()
    );

    fixture.commit(&fixture.writer.worktree, "other graph head")?;
    let other = host.head(&fixture.writer)?;
    fixture.git(&[
        "update-ref",
        &reference,
        &other.to_string(),
        &head.to_string(),
    ])?;
    assert!(matches!(
        host.graph_publication_branch(&fixture.writer.project, head),
        Err(RunError::Delivery(WriterDeliveryStage::Push))
    ));
    assert_eq!(
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", "--verify", &reference],
            WriterDeliveryStage::Push
        )?,
        other.to_string()
    );

    let common = host.command(
        &fixture.writer.project,
        "git",
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        WriterDeliveryStage::Push,
    )?;
    let directory = provenant_os::fs::Directory::open_anchor(Path::new(&common))?;
    artifacts::seal(
        &directory,
        &format!("provenant-graph-publication-{other}.json"),
        publication.as_bytes(),
        RECORD_LIMIT,
    )?;
    assert!(matches!(
        host.graph_publication_branch(&fixture.writer.project, other),
        Err(RunError::Delivery(WriterDeliveryStage::Push))
    ));
    let absent = format!("refs/heads/provenant/graph/{other}");
    assert!(
        host.command(
            &fixture.writer.project,
            "git",
            &["show-ref", "--verify", "--quiet", &absent],
            WriterDeliveryStage::Push
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn graph_publication_recovers_a_ref_created_before_its_marker()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let publication = format!("provenant/graph/{head}");
    // Simulate a crash after update-ref but before the ownership marker.
    fixture.git(&["branch", &publication, &head.to_string()])?;
    let common = host.command(
        &fixture.writer.project,
        "git",
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        WriterDeliveryStage::Push,
    )?;
    let directory = provenant_os::fs::Directory::open_anchor(Path::new(&common))?;
    let marker = format!("provenant-graph-publication-{head}.json");
    assert!(artifacts::read_sealed(&directory, &marker, RECORD_LIMIT)?.is_none());
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );
    assert_eq!(
        artifacts::read_sealed(&directory, &marker, RECORD_LIMIT)?.as_deref(),
        Some(publication.as_bytes())
    );
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );

    fixture.commit(&fixture.writer.worktree, "different graph head")?;
    let head = host.head(&fixture.writer)?;
    let publication = format!("provenant/graph/{head}");
    fixture.git(&["branch", &publication])?;
    assert!(
        host.graph_publication_branch(&fixture.writer.project, head)
            .is_err()
    );
    fixture.git(&["branch", "-d", &publication])?;
    fixture.git(&[
        "symbolic-ref",
        &format!("refs/heads/{publication}"),
        "refs/heads/missing",
    ])?;
    assert!(
        host.graph_publication_branch(&fixture.writer.project, head)
            .is_err()
    );
    fixture.git(&[
        "symbolic-ref",
        "--delete",
        &format!("refs/heads/{publication}"),
    ])?;
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );
    assert_eq!(
        host.graph_publication_branch(&fixture.writer.project, head)?,
        publication
    );
    Ok(())
}

#[test]
fn delivery_records_dirty_and_checked_out_failures_after_start()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for dirty in [true, false] {
        let mut fixture = PrepareFixture::new()?;
        fixture.git(&["branch", "integration"])?;
        fixture.request = HostDeliveryContext::destination(
            DeliveryMode::Integrate("integration".into()),
            "lane",
        )?
        .ok_or("destination")?;
        let host = fixture.prepare()?;
        fixture.commit(&fixture.writer.worktree, "lane change")?;
        if dirty {
            std::fs::write(fixture.writer.worktree.join("dirty"), "keep")?;
        } else {
            fixture.git(&[
                "worktree",
                "add",
                fixture.root.join("checked-out").to_str().ok_or("tree")?,
                "integration",
            ])?;
        }
        let path = fixture.root.join("attempt");
        std::fs::create_dir(&path)?;
        let attempt = provenant_os::fs::Directory::open_anchor(&path)?;
        let receipt = deliver_once(
            &host,
            &fixture.writer,
            &fixture.request,
            &attempt,
            "Run fixture\nAttempt fixture",
        )?;
        assert!(matches!(receipt.state, WriterDeliveryState::Failed { .. }));
        let raw =
            artifacts::read_sealed(&attempt, "delivery.json", RECORD_LIMIT)?.ok_or("delivery")?;
        assert_eq!(db::decode::<WriterDeliveryReceipt>(&raw)?, receipt);
        assert!(artifacts::read_sealed(&attempt, "delivery-started.json", RECORD_LIMIT)?.is_some());
    }
    Ok(())
}

#[test]
fn two_lanes_contend_for_integration_without_losing_either_head()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "integration"])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    let first = fixture.prepare()?;
    let second_writer = WriterRequest {
        project: fixture.writer.project.clone(),
        worktree: fixture.root.join("second-lane"),
    };
    fixture.git(&[
        "worktree",
        "add",
        "-qb",
        "second-lane",
        second_writer.worktree.to_str().ok_or("tree")?,
    ])?;
    let second = HostDeliveryContext::new(fixture.environment.clone())
        .prepare(&second_writer, &fixture.request)?;
    fixture.commit(&fixture.writer.worktree, "first lane")?;
    fixture.commit(&second_writer.worktree, "second lane")?;
    let first_head = first.head(&fixture.writer)?;
    let second_head = second.head(&second_writer)?;
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            barrier.wait();
            first.integrate(&fixture.writer, &fixture.request, first_head)
        });
        let two = scope.spawn(|| {
            barrier.wait();
            second.integrate(&second_writer, &fixture.request, second_head)
        });
        one.join()
            .expect("first lane thread")
            .expect("first integration");
        two.join()
            .expect("second lane thread")
            .expect("second integration");
    });
    for head in [first_head, second_head] {
        first.command(
            &fixture.writer.project,
            "git",
            &[
                "merge-base",
                "--is-ancestor",
                &head.to_string(),
                "integration",
            ],
            WriterDeliveryStage::Verify,
        )?;
    }
    Ok(())
}

#[test]
fn graph_pr_body_uses_its_recorded_base_despite_primary_checkout_changes()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.commit(&fixture.writer.worktree, "Unrelated integration history")?;
    let host = fixture.prepare()?;
    let base = host.head(&fixture.writer)?;
    fixture.commit(&fixture.writer.worktree, "Graph work")?;
    fixture.git(&["branch", "integration", "lane"])?;
    fixture.commit(&fixture.writer.project, "Unrelated primary checkout")?;
    // Only credential/hosting boundaries are synthetic. Git publication uses
    // the real repository and a local bare remote, with no network access.
    fixture.git(&[
        "config",
        &format!(
            "url.{}.insteadOf",
            fixture.root.join("remote.git").display()
        ),
        "https://github.com/fixture/repo.git",
    ])?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let wrapper = fixture.root.join("git-wrapper");
    fixture.executable(&wrapper, &format!("#!/bin/sh\nif [ \"$5\" = remote ] && [ \"$6\" = get-url ]; then printf 'https://github.com/fixture/repo.git\\n'; exit 0; fi\nexec '{}' \"$@\"\n", git.display()))?;
    let gh = fixture.root.join("gh");
    fixture.executable(&gh, "#!/bin/sh\nif [ \"$2\" = view ]; then exit 1; fi\ncd \"$(dirname \"$0\")\" || exit 1\ncat > graph-body\nprintf 'https://github.com/fixture/repo/pull/1\\n'\n")?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    fixture.environment.retain(|(key, _)| key != "PATH");
    fixture.environment.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", fixture.root.display()).into(),
    ));
    let url = HostDeliveryContext::new(fixture.environment.clone()).deliver_graph_pr(
        &fixture.writer.project,
        "integration",
        base,
        "Graph fixture\nRun fixture\nAttempt fixture",
    )?;
    assert_eq!(url, "https://github.com/fixture/repo/pull/1");
    let body = std::fs::read_to_string(fixture.root.join("graph-body"))?;
    assert!(
        body.contains("Graph work") && body.contains("Attempt fixture"),
        "{body}"
    );
    assert!(
        !body.contains("Unrelated integration history")
            && !body.contains("Unrelated primary checkout")
            && !body.contains("Initial"),
        "{body}"
    );
    Ok(())
}

#[test]
fn graph_pr_refreshes_forged_target_and_refuses_failed_fetch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    let base = fixture.prepare()?.head(&fixture.writer)?;
    fixture.commit(&fixture.writer.worktree, "Graph work")?;
    fixture.git(&["push", "origin", "lane:refs/heads/integration"])?;
    fixture.git(&[
        "update-ref",
        "refs/remotes/origin/integration",
        &base.to_string(),
    ])?;
    fixture.git(&[
        "config",
        &format!(
            "url.{}.insteadOf",
            fixture.root.join("remote.git").display()
        ),
        "https://github.com/fixture/repo.git",
    ])?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let wrapper = fixture.root.join("git-wrapper");
    fixture.executable(&wrapper, &format!("#!/bin/sh\nif [ \"$5\" = remote ] && [ \"$6\" = get-url ]; then printf 'https://github.com/fixture/repo.git\\n'; exit 0; fi\nexec '{}' \"$@\"\n", git.display()))?;
    let gh = fixture.root.join("gh");
    fixture.executable(&gh, "#!/bin/sh\nif [ \"$2\" = view ]; then exit 1; fi\ncd \"$(dirname \"$0\")\" || exit 1\ncat > graph-body\nprintf 'https://github.com/fixture/repo/pull/1\\n'\n")?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), wrapper.into_os_string()));
    fixture.environment.retain(|(key, _)| key != "PATH");
    fixture.environment.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", fixture.root.display()).into(),
    ));
    HostDeliveryContext::new(fixture.environment.clone()).deliver_graph_pr(
        &fixture.writer.project,
        "origin/integration",
        base,
        "Graph fixture",
    )?;
    assert!(std::fs::read_to_string(fixture.root.join("graph-body"))?.contains("Graph work"));

    fixture.git(&[
        "update-ref",
        "refs/remotes/origin/integration",
        &base.to_string(),
    ])?;
    std::fs::write(
        fixture
            .writer
            .project
            .join(".git/refs/remotes/origin/integration.lock"),
        "stale lock",
    )?;
    assert!(
        HostDeliveryContext::new(fixture.environment.clone())
            .deliver_graph_pr(
                &fixture.writer.project,
                "origin/integration",
                base,
                "Graph fixture",
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn commit_pr_body_is_bounded_and_contains_commit_messages()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "Commit subject\n\nCommit body")?;
    let body = host.commit_body(
        &fixture.writer,
        host.head(&fixture.writer)?,
        "Run fixture\nAttempt fixture",
    )?;
    assert!(
        body.contains("Commit subject")
            && body.contains("Commit body")
            && body.contains("Attempt fixture")
    );
    let capped = cap_pr_body("\u{1f600}".repeat(30_000));
    assert!(capped.len() <= 60_000 && capped.ends_with("[Commit summary truncated.]\n"));
    fixture.commit(
        &fixture.writer.worktree,
        &format!("Large commit\n\n{}", "\u{1f600}".repeat(20_000)),
    )?;
    let body = host.commit_body(
        &fixture.writer,
        host.head(&fixture.writer)?,
        "Run fixture\nAttempt fixture",
    )?;
    assert!(body.len() <= 60_000 && body.contains("[Commit summary truncated.]"));
    Ok(())
}

#[test]
fn remote_integration_refreshes_the_leased_destination()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["push", "origin", "HEAD:refs/heads/integration"])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
    )?
    .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "remote lane")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(&fixture.writer, &fixture.request, head)?;
    assert_eq!(
        host.command(
            &fixture.writer.worktree,
            "git",
            &["rev-parse", "refs/remotes/origin/integration"],
            WriterDeliveryStage::Verify
        )?,
        head.to_string()
    );
    host.integrate(&fixture.writer, &fixture.request, head)?;
    Ok(())
}

#[test]
fn remote_integration_refuses_a_symbolic_destination()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["push", "origin", "HEAD:refs/heads/main"])?;
    fixture.git(&[
        "--git-dir",
        fixture.root.join("remote.git").to_str().ok_or("remote")?,
        "symbolic-ref",
        "refs/heads/integration",
        "refs/heads/main",
    ])?;
    fixture.request = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
    )?
    .ok_or("destination")?;
    let host = fixture.prepare()?;
    let original = host.head(&fixture.writer)?;
    fixture.commit(&fixture.writer.worktree, "lane change")?;
    let head = host.head(&fixture.writer)?;
    assert!(matches!(
        host.integrate(&fixture.writer, &fixture.request, head),
        Err(RunError::Delivery(WriterDeliveryStage::Integrate))
    ));
    let remote = host.command(
        &fixture.writer.project,
        "git",
        &["ls-remote", "origin", "refs/heads/main"],
        WriterDeliveryStage::Verify,
    )?;
    assert_eq!(
        remote.split_whitespace().next(),
        Some(original.to_string().as_str())
    );
    Ok(())
}

#[test]
fn local_integration_fast_forwards_merges_and_replays()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture.git(&["branch", "integration"])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    let host = fixture.prepare()?;
    fixture.commit(&fixture.writer.worktree, "lane one")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(&fixture.writer, &fixture.request, head)?;
    assert_eq!(
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", "integration"],
            WriterDeliveryStage::Verify
        )?,
        head.to_string()
    );
    host.integrate(&fixture.writer, &fixture.request, head)?;
    // Divergent commits with a common tree still require a two-parent merge.
    fixture.commit(&fixture.writer.project, "integration side")?;
    fixture.git(&["branch", "-f", "integration", "HEAD"])?;
    fixture.commit(&fixture.writer.worktree, "lane two")?;
    let head = host.head(&fixture.writer)?;
    host.integrate(&fixture.writer, &fixture.request, head)?;
    let parents = host.command(
        &fixture.writer.project,
        "git",
        &["rev-list", "--parents", "-n", "1", "integration"],
        WriterDeliveryStage::Verify,
    )?;
    assert_eq!(parents.split_whitespace().count(), 3);
    assert!(parents.ends_with(&head.to_string()));
    Ok(())
}

#[test]
fn local_integration_conflict_dirty_and_checked_out_refuse_without_partial_state()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    std::fs::write(fixture.writer.project.join("conflict"), "integration")?;
    fixture.git(&["add", "conflict"])?;
    fixture.commit(&fixture.writer.project, "integration conflict")?;
    fixture.git(&["branch", "integration"])?;
    fixture.request =
        HostDeliveryContext::destination(DeliveryMode::Integrate("integration".into()), "lane")?
            .ok_or("destination")?;
    let host = fixture.prepare()?;
    let original = host.command(
        &fixture.writer.project,
        "git",
        &["rev-parse", "integration"],
        WriterDeliveryStage::Verify,
    )?;
    std::fs::write(fixture.writer.worktree.join("conflict"), "lane")?;
    fixture.lane_git(&["add", "conflict"])?;
    fixture.commit(&fixture.writer.worktree, "lane conflict")?;
    let head = host.head(&fixture.writer)?;
    assert!(matches!(
        host.integrate(&fixture.writer, &fixture.request, head),
        Err(RunError::Delivery(WriterDeliveryStage::Integrate))
    ));
    assert_eq!(
        host.command(
            &fixture.writer.project,
            "git",
            &["rev-parse", "integration"],
            WriterDeliveryStage::Verify
        )?,
        original
    );
    assert_eq!(host.head(&fixture.writer)?, head);
    assert!(!fixture.writer.project.join(".git/MERGE_HEAD").exists());
    std::fs::write(fixture.writer.worktree.join("dirty"), "keep")?;
    assert!(
        host.integrate(&fixture.writer, &fixture.request, head)
            .is_err()
    );
    std::fs::remove_file(fixture.writer.worktree.join("dirty"))?;
    let primary = std::fs::read_to_string(fixture.writer.project.join(".git/HEAD"))?
        .trim()
        .trim_start_matches("ref: refs/heads/")
        .to_owned();
    let request = HostDeliveryContext::destination(DeliveryMode::Integrate(primary), "lane")?
        .ok_or("destination")?;
    assert!(matches!(
        HostDeliveryContext::new(fixture.environment.clone()).prepare(&fixture.writer, &request),
        Err(RunError::DeliveryPrepare {
            step: "integration target",
            ..
        })
    ));
    Ok(())
}

#[test]
fn concise_delivery_preserves_alias_contract_and_refuses_injection() -> Result<()> {
    assert!(HostDeliveryContext::destination(DeliveryMode::None, "")?.is_none());
    let push = HostDeliveryContext::destination(DeliveryMode::Push, "lane")?
        .ok_or(RunError::Validation("push"))?;
    assert_eq!(
        (&*push.remote, &*push.branch, push.open_pr),
        ("origin", "lane", false)
    );
    assert!(
        HostDeliveryContext::destination(DeliveryMode::Pr, "lane")?
            .is_some_and(|request| request.open_pr)
    );
    let remote = HostDeliveryContext::destination(
        DeliveryMode::Integrate("origin/integration".into()),
        "lane",
    )?
    .ok_or(RunError::Validation("integration"))?;
    assert_eq!(
        (&*remote.remote, &*remote.branch),
        ("origin", "integration")
    );
    for invalid in [
        "integrate:",
        "integrate:--all",
        "integrate:main..other",
        "integrate:refs/heads/../main",
        "pr:main",
    ] {
        assert!(invalid.parse::<DeliveryMode>().is_err(), "{invalid}");
    }
    Ok(())
}

#[test]
fn review_delivery_requires_acceptance_and_exact_head()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    struct Free;
    impl provenant_confinement::worktrees::WorkerObserver for Free {
        fn observe_roots(
            &self,
            roots: &[std::path::PathBuf],
        ) -> std::collections::BTreeMap<
            std::path::PathBuf,
            provenant_confinement::worktrees::WorkerState,
        > {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::WorkerState::Free))
                .collect()
        }
    }
    use provenant_core::{CancelToken, Clock, RootInputs};
    use provenant_os::clock::SystemClock;
    let fixture = PrepareFixture::new()?;
    let mut execution = tests::context(&fixture.root)?;
    let mut inputs = RootInputs::new(fixture.root.join("home"), fixture.root.join("checkout"));
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        roots.state.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    execution.project_id = provenant_confinement::worktrees::with_context(
        &roots,
        &fixture.root.join("home"),
        &fixture.writer.project,
        &clock,
        &cancel,
        |context| {
            let context = provenant_confinement::worktrees::WorktreeContext {
                workers: &Free,
                ..*context
            };
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                attempt_id: None,
                operation_id: None,
                branch: "lane".into(),
                base: String::new(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: clock
                    .wall_now()
                    .map_err(|_| provenant_confinement::worktrees::WorktreeError::InvalidOutput)?,
                origin: provenant_confinement::worktrees::WorktreeOrigin::Adopted,
                label: None,
            };
            let head = provenant_confinement::run_utility(
                context.executor,
                provenant_confinement::GitRead::RevParseHead,
                context.repo,
                context.utility,
                context.utility_budget,
                &cancel,
            )?;
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                base: String::from_utf8_lossy(&head.stdout.bytes).trim().into(),
                ..owner
            };
            provenant_confinement::worktrees::adopt(
                &context,
                &fixture.writer.worktree,
                &owner,
                &cancel,
            )?;
            Ok(context.binding.project_id())
        },
    )?;
    let call = Request {
        clock: &clock,
        budget: provenant_core::WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.tasks[0].writer = Some(fixture.writer.clone());
    input.request.tasks[0].deliver = Some(fixture.request.clone());
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    let path = provenant_records::PathWire::from_relative(&provenant_core::RelativePath::parse(
        ".github/workflows/ci.yml",
    )?)?;
    artifacts::seal(
        &attempt,
        "writer-terminal.json",
        &db::bytes(&writer::WriterTerminal {
            schema_version: 1,
            head,
            protected_paths: vec![path.clone()],
        })?,
        RECORD_LIMIT,
    )?;
    let exit = tests::exit();
    tests::seal_observation(&store, &input, &exit)?;
    let lock = attempt.lock(
        std::ffi::OsStr::new(provenant_records::AttemptFile::CustodyLock.name()),
        provenant_os::fs::LockMode::Exclusive,
        call.budget,
        &cancel,
        &clock,
    )?;
    drop(lock);
    let mut publication = tests::publication();
    publication.outcome = Outcome::ReviewRequired;
    publication.reason = Some("protected changes require review".into());
    store.publish_terminal(id, 1, &publication, &exit, &call)?;
    let deliver = |accept_review| {
        store.deliver(
            id,
            None,
            HostDeliveryContext::new(fixture.environment.clone()),
            accept_review,
            &call,
        )
    };
    let refusal = deliver(false).expect_err("review requires explicit acceptance");
    assert!(refusal.to_string().contains("--accept-review"));
    fixture.commit(&fixture.writer.worktree, "moved after review")?;
    assert!(matches!(
        deliver(true),
        Err(RunError::Validation(
            "worktree head differs from the settled receipt"
        ))
    ));
    fixture.lane_git(&["reset", "--hard", &head.to_string()])?;
    let receipt = deliver(true)?;
    assert!(matches!(
        receipt.state,
        WriterDeliveryState::Delivered { .. }
    ));
    let acceptance = receipt.review_acceptance.as_ref().ok_or("acceptance")?;
    assert_eq!(acceptance.protected_paths, vec![path]);
    assert_eq!(acceptance.actor, execution.actor_id);
    assert_eq!(
        store.delivered(id, &call)?.map(|(_, receipt)| receipt),
        Some(receipt)
    );
    Ok(())
}

#[test]
fn settled_delivery_refuses_dirty_or_changed_head_and_replays_success()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    struct Free;
    impl provenant_confinement::worktrees::WorkerObserver for Free {
        fn observe_roots(
            &self,
            roots: &[std::path::PathBuf],
        ) -> std::collections::BTreeMap<
            std::path::PathBuf,
            provenant_confinement::worktrees::WorkerState,
        > {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::WorkerState::Free))
                .collect()
        }
    }
    use provenant_core::{CancelToken, Clock, RootInputs};
    use provenant_os::clock::SystemClock;
    let fixture = PrepareFixture::new()?;
    let mut execution = tests::context(&fixture.root)?;
    let mut inputs = RootInputs::new(fixture.root.join("home"), fixture.root.join("checkout"));
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        roots.state.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    execution.project_id = provenant_confinement::worktrees::with_context(
        &roots,
        &fixture.root.join("home"),
        &fixture.writer.project,
        &clock,
        &cancel,
        |context| {
            let context = provenant_confinement::worktrees::WorktreeContext {
                workers: &Free,
                ..*context
            };
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                attempt_id: None,
                operation_id: None,
                branch: "lane".into(),
                base: String::new(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: clock
                    .wall_now()
                    .map_err(|_| provenant_confinement::worktrees::WorktreeError::InvalidOutput)?,
                origin: provenant_confinement::worktrees::WorktreeOrigin::Adopted,
                label: None,
            };
            let head = provenant_confinement::run_utility(
                context.executor,
                provenant_confinement::GitRead::RevParseHead,
                context.repo,
                context.utility,
                context.utility_budget,
                &cancel,
            )?;
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                base: String::from_utf8_lossy(&head.stdout.bytes).trim().into(),
                ..owner
            };
            provenant_confinement::worktrees::adopt(
                &context,
                &fixture.writer.worktree,
                &owner,
                &cancel,
            )?;
            Ok(context.binding.project_id())
        },
    )?;
    let call = Request {
        clock: &clock,
        budget: provenant_core::WaitBudget::cli(clock.mono_now(), 30)?,
        cancel: &cancel,
    };
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.tasks[0].writer = Some(fixture.writer.clone());
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let host = fixture.prepare()?;
    let head = host.head(&fixture.writer)?;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    artifacts::seal(
        &attempt,
        "writer-terminal.json",
        &db::bytes(&writer::WriterTerminal {
            schema_version: 1,
            head,
            protected_paths: vec![],
        })?,
        RECORD_LIMIT,
    )?;
    let exit = tests::exit();
    tests::seal_observation(&store, &input, &exit)?;
    let lock = attempt.lock(
        std::ffi::OsStr::new(provenant_records::AttemptFile::CustodyLock.name()),
        provenant_os::fs::LockMode::Exclusive,
        call.budget,
        &cancel,
        &clock,
    )?;
    drop(lock);
    store.publish_terminal(id, 1, &tests::publication(), &exit, &call)?;
    let deliver = || {
        store.deliver(
            id,
            Some(fixture.request.clone()),
            HostDeliveryContext::new(fixture.environment.clone()),
            false,
            &call,
        )
    };
    std::fs::write(fixture.writer.worktree.join("dirty"), "keep")?;
    assert!(deliver().is_err());
    std::fs::remove_file(fixture.writer.worktree.join("dirty"))?;
    fixture.commit(&fixture.writer.worktree, "moved after receipt")?;
    assert!(matches!(
        deliver(),
        Err(RunError::Validation(
            "worktree head differs from the settled receipt"
        ))
    ));
    fixture.lane_git(&["reset", "--hard", &head.to_string()])?;
    let key = provenant_core::hex(
        provenant_records::CanonicalV1::file_digest(&serde_json::to_vec(&(
            head,
            &fixture.request,
        ))?)
        .bytes(),
    );
    artifacts::seal(
        &attempt,
        &format!("delivery-{key}.json"),
        &db::bytes(&WriterDeliveryReceipt {
            schema_version: 1,
            remote: fixture.request.remote.clone(),
            branch: fixture.request.branch.clone(),
            head: Some(head),
            review_acceptance: None,
            state: WriterDeliveryState::Failed {
                stage: WriterDeliveryStage::Interrupted,
            },
        })?,
        RECORD_LIMIT,
    )?;
    let receipt = deliver()?;
    assert!(matches!(
        receipt.state,
        WriterDeliveryState::Delivered { .. }
    ));
    assert_eq!(
        store.delivered(id, &call)?.map(|(_, delivered)| delivered),
        Some(receipt.clone())
    );
    assert_eq!(deliver()?, receipt);
    attempt.unlink(std::ffi::OsStr::new("delivery-pickup.json"))?;
    assert_eq!(deliver()?, receipt);
    assert_eq!(
        store.delivered(id, &call)?.map(|(_, delivered)| delivered),
        Some(receipt.clone())
    );
    store.record_worktree_settlement(
        id,
        &serde_json::json!({"schema_version":1,"state":"pruned"}),
        &call,
    )?;
    store.record_worktree_settlement(
        id,
        &serde_json::json!({"schema_version":1,"state":"retained","reason":"racing observer"}),
        &call,
    )?;
    assert_eq!(
        store.worktree_settlement(id, &call)?.ok_or("settlement")?["state"],
        "pruned"
    );
    // Sealed successful effects replay even when the checkout has since moved.
    fixture.commit(&fixture.writer.worktree, "moved after delivery")?;
    assert_eq!(deliver()?, receipt);
    fixture.git(&[
        "worktree",
        "remove",
        "--force",
        fixture.writer.worktree.to_str().ok_or("lane")?,
    ])?;
    assert_eq!(deliver()?, receipt);
    Ok(())
}
impl PrepareFixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let project = root.join("project");
        let home = root.join("home");
        std::fs::create_dir(&project)?;
        std::fs::create_dir(&home)?;
        let fixture = Self {
            _temp: temp,
            root: root.clone(),
            environment: vec![
                ("HOME".into(), home.into_os_string()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ],
            writer: WriterRequest {
                project,
                worktree: root.join("lane"),
            },
            request: WriterDeliveryRequest {
                mode: Default::default(),
                remote: "origin".into(),
                branch: "published/lane".into(),
                open_pr: false,
            },
        };
        fixture.git(&["init", "-q"])?;
        fixture.git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "Initial",
        ])?;
        let remote = root.join("remote.git");
        fixture.git(&["init", "--bare", "-q", remote.to_str().ok_or("remote")?])?;
        fixture.git(&["remote", "add", "origin", remote.to_str().ok_or("remote")?])?;
        fixture.git(&[
            "worktree",
            "add",
            "-qb",
            "lane",
            fixture.writer.worktree.to_str().ok_or("lane")?,
        ])?;
        Ok(fixture)
    }
    fn git(&self, args: &[&str]) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let git = provenant_os::git::git_executable().ok_or("git executable")?;
        let argv: Vec<OsString> = std::iter::once(git.into_os_string())
            .chain(args.iter().map(OsString::from))
            .collect();
        let mut options = ExecOptions::new(&argv, &self.writer.project, Duration::from_secs(5));
        options.env = Some(&self.environment);
        let output = SystemExecutor::new(1)?.execute(options)?;
        assert_eq!(
            output.termination,
            Termination::Exit(0),
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(())
    }
    fn executable(&self, path: &Path, script: &str) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, script)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    fn prepare(&self) -> Result<HostDeliveryContext> {
        HostDeliveryContext::new(self.environment.clone()).prepare(&self.writer, &self.request)
    }
    fn lane_git(&self, args: &[&str]) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut command = vec!["-C", self.writer.worktree.to_str().ok_or("lane")?];
        command.extend_from_slice(args);
        self.git(&command)
    }
    fn commit(
        &self,
        path: &Path,
        message: &str,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        self.git(&[
            "-C",
            path.to_str().ok_or("commit path")?,
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            message,
        ])
    }
    fn terminal(
        &self,
        write: impl FnOnce() -> std::result::Result<(), Box<dyn std::error::Error>>,
    ) -> std::result::Result<bool, Box<dyn std::error::Error>> {
        self.terminal_via("origin", None, write)
    }
    fn terminal_via(
        &self,
        remote: &str,
        base_ref: Option<&str>,
        write: impl FnOnce() -> std::result::Result<(), Box<dyn std::error::Error>>,
    ) -> std::result::Result<bool, Box<dyn std::error::Error>> {
        use provenant_confinement::{GitRead, UtilityBudget, UtilityRuntime};
        use provenant_core::{CancelToken, Clock, WaitBudget};
        use provenant_os::fs::Directory;
        use provenant_supervisor::custody::{RecoveryState, WriterBinding};
        use std::ffi::OsStr;
        use std::os::unix::fs::PermissionsExt;

        let state = self.root.join("state");
        let attempt_path = self.root.join("attempt");
        std::fs::create_dir(&state)?;
        std::fs::create_dir(&attempt_path)?;
        for path in [&state, &attempt_path, &self.root.join("home")] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        let attempt = Directory::open_anchor(&attempt_path)?;
        let utility = UtilityRuntime::new(
            provenant_os::git::git_executable().ok_or("git executable")?,
            self.root.join("home"),
        )?;
        let executor = SystemExecutor::new(1)?;
        let cancel = CancelToken::new();
        let budget = UtilityBudget::bounded(Duration::from_secs(5))?;
        let repo = utility.open_repo(&executor, &self.writer.project, budget, &cancel)?;
        let worktree = provenant_confinement::verify_worktree(&repo, &self.writer.worktree)?;
        let head = provenant_confinement::run_utility(
            &executor,
            GitRead::RevParseHead,
            worktree.repo(),
            &utility,
            budget,
            &cancel,
        )?;
        assert_eq!(head.termination, Termination::Exit(0));
        let base = GitOid::parse(std::str::from_utf8(&head.stdout.bytes)?.trim())?;
        let launch = writer::WriterLaunch {
            worktree,
            branch: provenant_confinement::repo::GitRefName::new("refs/heads/lane")?,
            base,
            custody: None,
        };
        let scope = provenant_supervisor::custody::Scope {
            generation: 1,
            ..tests::exit().scope
        };
        launch.snapshot(&state, &attempt, scope)?;
        let binding = WriterBinding::from_verified(
            launch.worktree.repo().root_dir(),
            launch.worktree.repo().private_dir(),
            launch.branch.as_str(),
            base,
        )?;
        let clock = provenant_os::clock::SystemClock::new();
        let recovery = provenant_supervisor::fixture::acquire_writer_with_binding(
            Directory::open_anchor(&state)?,
            scope,
            binding,
            WaitBudget::cli(clock.mono_now(), 1)?,
            &cancel,
            &clock,
        )?
        .into_recovery()?;
        assert_eq!(recovery.observe(), RecoveryState::Absent);
        write()?;
        let review = launch.terminal(&state, &attempt, &recovery, remote, base_ref)?;
        let archive = attempt.open_verified_directory(OsStr::new("quarantine"))?;
        let manifest: serde_json::Value = serde_json::from_slice(
            &archive.read_bounded(OsStr::new("manifest.json"), RECORD_LIMIT)?,
        )?;
        assert_eq!(manifest["review_required"], review);
        let terminal: writer::WriterTerminal = serde_json::from_slice(
            &attempt.read_bounded(OsStr::new("writer-terminal.json"), RECORD_LIMIT)?,
        )?;
        assert_eq!(terminal.protected_paths.is_empty(), !review);
        Ok(review)
    }
}

#[test]
fn terminal_ignores_protected_changes_present_at_lane_start()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ])?;
    std::fs::create_dir(fixture.writer.worktree.join(".github"))?;
    std::fs::write(
        fixture.writer.worktree.join(".github/ci.yml"),
        "PR policy\n",
    )?;
    fixture.lane_git(&["add", ".github"])?;
    fixture.commit(&fixture.writer.worktree, "Existing PR policy")?;
    assert!(!fixture.terminal(|| {
        std::fs::write(
            fixture.writer.worktree.join("source.txt"),
            "writer source\n",
        )?;
        fixture.lane_git(&["add", "source.txt"])?;
        fixture.commit(&fixture.writer.worktree, "Writer source")
    })?);
    Ok(())
}

#[test]
fn terminal_falls_back_when_delivery_target_cannot_be_resolved()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for target in ["missing_head", "dangling_head", "unrelated"] {
        for protected in [false, true] {
            let fixture = PrepareFixture::new()?;
            if target != "missing_head" {
                fixture.git(&[
                    "symbolic-ref",
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/main",
                ])?;
            }
            if target == "unrelated" {
                fixture.git(&["checkout", "--orphan", "unrelated"])?;
                fixture.commit(&fixture.writer.project, "Unrelated target")?;
                fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
            }
            let review = fixture.terminal(|| {
                let path = if protected { "AGENTS.md" } else { "source.txt" };
                std::fs::write(fixture.writer.worktree.join(path), "writer change\n")?;
                fixture.lane_git(&["add", path])?;
                fixture.commit(&fixture.writer.worktree, "Writer change")
            })?;
            assert_eq!(review, protected, "{target}");
        }
    }
    Ok(())
}

fn merge_upstream_then_edit(
    fixture: &PrepareFixture,
    remote_ref: &str,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    fixture.lane_git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "merge",
        "--no-edit",
        remote_ref,
    ])?;
    std::fs::write(
        fixture.writer.worktree.join("source.txt"),
        "writer source\n",
    )?;
    fixture.lane_git(&["add", "source.txt"])?;
    fixture.commit(&fixture.writer.worktree, "Writer source")
}

fn commit_protected_on(
    fixture: &PrepareFixture,
    remote_ref: &str,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(fixture.writer.project.join(".github"))?;
    std::fs::write(
        fixture.writer.project.join(".github/ci.yml"),
        "upstream policy\n",
    )?;
    fixture.git(&["add", ".github"])?;
    fixture.commit(&fixture.writer.project, "Upstream policy")?;
    fixture.git(&["update-ref", remote_ref, "HEAD"])
}

#[test]
fn terminal_uses_the_base_branch_on_a_delivery_remote_without_head()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    commit_protected_on(&fixture, "refs/remotes/fork/main")?;
    // The delivery remote "fork" has no HEAD, but it carries the base branch.
    let review = fixture.terminal_via("fork", Some("main"), || {
        merge_upstream_then_edit(&fixture, "fork/main")
    })?;
    assert!(!review);
    Ok(())
}

#[test]
fn terminal_never_exempts_a_commit_reachable_only_from_another_remote()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    // origin is a fork with an unreviewed protected commit and a HEAD; the
    // delivery remote "upstream" has neither HEAD nor the base branch.
    commit_protected_on(&fixture, "refs/remotes/origin/main")?;
    fixture.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ])?;
    let review = fixture.terminal_via("upstream", Some("main"), || {
        merge_upstream_then_edit(&fixture, "origin/main")
    })?;
    assert!(review);
    Ok(())
}

#[test]
fn terminal_names_a_writer_left_off_the_lane_branch()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let error = fixture
        .terminal(|| {
            fixture.lane_git(&["checkout", "--detach"])?;
            fixture.commit(&fixture.writer.worktree, "Detached work")
        })
        .expect_err("detached writer HEAD")
        .to_string();
    assert!(
        error.contains("writer HEAD is not the lane branch"),
        "{error}"
    );
    Ok(())
}

#[test]
fn terminal_exempts_merged_upstream_protected_changes()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for inherited in ["none", "addition", "deletion"] {
        for protected in [false, true] {
            let fixture = PrepareFixture::new()?;
            if inherited == "addition" {
                std::fs::create_dir(fixture.writer.worktree.join(".github"))?;
                std::fs::write(
                    fixture.writer.worktree.join(".github/pr.yml"),
                    "PR policy\n",
                )?;
                fixture.lane_git(&["add", ".github"])?;
                fixture.commit(&fixture.writer.worktree, "Existing PR addition")?;
            } else if inherited == "deletion" {
                std::fs::create_dir(fixture.writer.project.join(".github"))?;
                std::fs::write(
                    fixture.writer.project.join(".github/old.yml"),
                    "old policy\n",
                )?;
                fixture.git(&["add", ".github"])?;
                fixture.commit(&fixture.writer.project, "Shared policy")?;
                fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
                fixture.lane_git(&["merge", "--ff-only", "origin/main"])?;
                fixture.lane_git(&["rm", ".github/old.yml"])?;
                fixture.commit(&fixture.writer.worktree, "Existing PR deletion")?;
            }
            let review = fixture.terminal(|| {
                std::fs::create_dir_all(fixture.writer.project.join(".github"))?;
                std::fs::write(
                    fixture.writer.project.join(".github/ci.yml"),
                    "main policy\n",
                )?;
                fixture.git(&["add", ".github"])?;
                fixture.commit(&fixture.writer.project, "Upstream policy")?;
                fixture.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])?;
                fixture.git(&[
                    "symbolic-ref",
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/main",
                ])?;
                std::fs::write(
                    fixture.writer.worktree.join("source.txt"),
                    "writer source\n",
                )?;
                fixture.lane_git(&["add", "source.txt"])?;
                fixture.commit(&fixture.writer.worktree, "Writer source")?;
                fixture.lane_git(&[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "merge",
                    "--no-edit",
                    "origin/main",
                ])?;
                if protected {
                    std::fs::write(
                        fixture.writer.worktree.join(".github/ci.yml"),
                        "writer policy\n",
                    )?;
                    fixture.lane_git(&["add", ".github"])?;
                    fixture.commit(&fixture.writer.worktree, "Writer policy")?;
                }
                Ok(())
            })?;
            assert_eq!(review, protected, "{inherited}");
        }
    }
    Ok(())
}

#[test]
fn prepare_labels_unsupported_ssh_command() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    fixture
        .environment
        .push(("GIT_SSH_COMMAND".into(), "ssh -v".into()));
    let reason = fixture
        .prepare()
        .expect_err("unsupported SSH command")
        .to_string();
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (environment): GIT_SSH_COMMAND is not supported; use host SSH config"
    );
    Ok(())
}

#[test]
fn delivery_diagnostics_redact_url_userinfo() {
    for (input, expected) in [
        ("https://u:pa/ss@h/repo", "https://[REDACTED]@h/repo"),
        ("https://u:pa?ss@h/repo", "https://[REDACTED]@h/repo"),
        ("https://u:a@b@h/repo", "https://[REDACTED]@h/repo"),
        ("u:pa/ss@h:repo", "[REDACTED]@h:repo"),
        ("u:pa?ss@h:repo", "[REDACTED]@h:repo"),
        ("u:a@b@h:repo", "[REDACTED]@h:repo"),
        ("https://truncated-secret/path", "https://[REDACTED]/path"),
    ] {
        assert_eq!(super::delivery::redact_url_userinfo(input), expected);
        let error = super::delivery::prepare_error("test", input).to_string();
        assert!(error.ends_with(expected), "{error}");
    }
}

#[test]
fn prepare_refuses_remote_urls_with_embedded_credentials()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    for url in [
        "https://user:hunter2@example.invalid/repo.git",
        "https://hunter2@example.invalid/repo.git",
        "ssh://user:hunter2@example.invalid/repo.git",
        "https://example.invalid/repo.git?token=hunter2",
        "https://example.invalid/repo.git#hunter2",
    ] {
        fixture.git(&["remote", "set-url", "origin", url])?;
        let error = fixture.prepare().expect_err("credential-bearing URL");
        assert!(
            matches!(
                error,
                RunError::DeliveryPrepare {
                    step: "remote URL",
                    ..
                }
            ),
            "{url}"
        );
        assert!(!format!("{error:?}").contains("hunter2"), "{url}");
        assert!(!error.to_string().contains("hunter2"), "{url}");
    }
    Ok(())
}

#[test]
fn prepare_labels_environment_path_failures() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let mut fixture = PrepareFixture::new()?;
    for name in ["HOME", "GH_CONFIG_DIR"] {
        for (path, detail) in [
            (fixture.root.join("missing/config"), "not found"),
            (
                fixture.writer.worktree.clone(),
                "inside the writer worktree",
            ),
        ] {
            fixture.environment.retain(|(key, _)| key != name);
            fixture
                .environment
                .insert(0, (name.into(), path.into_os_string()));
            let reason = fixture
                .prepare()
                .expect_err("invalid host path")
                .to_string();
            assert_eq!(
                reason,
                format!("host writer delivery failed at Prepare (environment): {name}: {detail}")
            );
        }
        fixture.environment.retain(|(key, _)| key != name);
    }
    Ok(())
}

#[test]
fn prepare_labels_unavailable_gh_executable() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    let mut fixture = PrepareFixture::new()?;
    fixture.request.open_pr = true;
    fixture.environment.retain(|(key, _)| key != "PATH");
    let reason = fixture.prepare().expect_err("PATH is unset").to_string();
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (gh executable): gh is unavailable: PATH is unset"
    );
    fixture.environment.push(("PATH".into(), "".into()));
    let reason = fixture
        .prepare()
        .expect_err("gh is unavailable")
        .to_string();
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (gh executable): gh is unavailable on PATH"
    );
    Ok(())
}

#[test]
fn prepare_labels_git_executable_inside_worktree()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut fixture = PrepareFixture::new()?;
    let git = fixture.writer.worktree.join("git");
    fixture.executable(&git, "#!/bin/sh\nexit 0\n")?;
    fixture
        .environment
        .push(("PROVENANT_GIT".into(), git.into_os_string()));
    let reason = fixture
        .prepare()
        .expect_err("unsafe git executable")
        .to_string();
    assert_eq!(
        reason,
        "host writer delivery failed at Prepare (git executable): inside the writer worktree"
    );
    Ok(())
}

#[test]
fn prepare_accepts_absolute_shell_helper_outside_path()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let helper = fixture.root.join("gh");
    fixture.executable(&helper, "#!/bin/sh\nexit 0\n")?;
    fixture.git(&[
        "config",
        "credential.helper",
        &format!("!{} auth git-credential", helper.display()),
    ])?;
    fixture.prepare()?;
    Ok(())
}

#[test]
fn prepare_failures_name_step_and_bound_git_stderr()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for (pattern, step) in [
        ("remote get-url", "remote URL"),
        ("symbolic-ref --quiet", "HEAD"),
        ("rev-parse --absolute-git-dir", "git dir"),
        ("config --get-regexp", "credential helper"),
        ("fetch --no-auto-maintenance", "fetch"),
    ] {
        let mut fixture = PrepareFixture::new()?;
        let git = fixture.root.join("git");
        let real_git = provenant_os::git::git_executable().ok_or("git executable")?;
        fixture.executable(&git, &format!(
            "#!/bin/sh\ncase \"$*\" in\n*'{pattern}'*)\nprintf '%s' 'fixture git failure: private-token https://user:password@example.invalid/ https://secret-user@example.invalid/ scp-secret@example.invalid:repo https://malformed-secret:password/ {}' >&2\nexit 2;;\nesac\nexec '{}' \"$@\"\n", "é".repeat(5000), real_git.display()))?;
        fixture
            .environment
            .push(("PROVENANT_GIT".into(), git.into_os_string()));
        fixture
            .environment
            .push(("GH_TOKEN".into(), "private-token".into()));
        let reason = fixture.prepare().expect_err("git must fail").to_string();
        assert!(reason.contains("Prepare"), "{reason}");
        assert!(reason.contains(step), "{reason}");
        assert!(reason.contains("fixture git failure"), "{reason}");
        assert!(!reason.contains("private-token"), "{reason}");
        assert!(!reason.contains("user:password"), "{reason}");
        assert!(!reason.contains("secret-user"), "{reason}");
        assert!(!reason.contains("scp-secret"), "{reason}");
        assert!(!reason.contains("malformed-secret"), "{reason}");
        assert!(!reason.contains("receipt"), "{reason}");
        assert!(reason.len() < 1400, "diagnostic has {} bytes", reason.len());
    }
    Ok(())
}

#[test]
fn prepare_refuses_unavailable_and_unsafe_credential_helpers()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let fixture = PrepareFixture::new()?;
    let unavailable = fixture.root.join("missing/gh");
    let not_executable = fixture.root.join("not-executable");
    std::fs::write(&not_executable, "not executable")?;
    let lane_helper = fixture.writer.worktree.join("gh");
    fixture.executable(&lane_helper, "#!/bin/sh\nexit 0\n")?;
    let link = fixture.root.join("linked-gh");
    std::os::unix::fs::symlink(&lane_helper, &link)?;
    let opaque = fixture.root.join("gh;echo");
    fixture.executable(&opaque, "#!/bin/sh\nexit 0\n")?;
    for (helper, detail) in [
        (&unavailable, "gh: not found"),
        (&not_executable, "not-executable"),
        (&fixture.root, "unavailable"),
        (&lane_helper, "inside the writer worktree"),
        (&link, "inside the writer worktree"),
        (&opaque, "opaque shell credential helper"),
    ] {
        fixture.git(&[
            "config",
            "credential.helper",
            &format!("!{} auth git-credential", helper.display()),
        ])?;
        let reason = fixture.prepare().expect_err("unsafe helper").to_string();
        assert!(reason.contains("Prepare (credential helper)"), "{reason}");
        assert!(reason.contains(detail), "{reason}");
        assert!(!reason.contains("receipt"), "{reason}");
    }
    Ok(())
}

#[test]
fn host_delivery_fetches_commits_pushes_and_records_upstream()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let project = root.join("project");
    let lane = root.join("lane");
    let remote = root.join("remote.git");
    for path in [&home, &project] {
        std::fs::create_dir(path)?;
    }
    let environment = vec![
        ("HOME".into(), home.into_os_string()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
    ];
    let host = HostDeliveryContext::new(environment.clone());
    let executor = SystemExecutor::new(1)?;
    let git_executable = provenant_os::git::git_executable().ok_or("git executable")?;
    let git =
        |cwd: &Path, args: &[&str]| -> std::result::Result<String, Box<dyn std::error::Error>> {
            let argv: Vec<OsString> = std::iter::once(git_executable.as_os_str().to_owned())
                .chain(args.iter().map(OsString::from))
                .collect();
            let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(5));
            options.env = Some(&environment);
            let output = executor.execute(options)?;
            assert_eq!(
                output.termination,
                Termination::Exit(0),
                "{}",
                String::from_utf8_lossy(&output.stderr.bytes)
            );
            Ok(String::from_utf8(output.stdout.bytes)?.trim().into())
        };
    git(&project, &["init", "-q"])?;
    git(&project, &["config", "user.name", "Fixture"])?;
    git(
        &project,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    git(&project, &["commit", "--allow-empty", "-qm", "Initial"])?;
    git(
        &project,
        &["init", "--bare", "-q", remote.to_str().ok_or("remote")?],
    )?;
    git(
        &project,
        &["remote", "add", "origin", remote.to_str().ok_or("remote")?],
    )?;
    // A narrowed fetch refspec that omits the delivered branch, as in a
    // main-only clone, must not defeat delivery.
    git(
        &project,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/base:refs/remotes/origin/base",
        ],
    )?;
    git(
        &project,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/injected/*",
        ],
    )?;
    git(&project, &["push", "-q", "origin", "HEAD:refs/heads/base"])?;
    let initial = git(&project, &["rev-parse", "HEAD"])?;
    git(&project, &["update-ref", "-d", "refs/remotes/origin/base"])?;
    git(
        &project,
        &[
            "worktree",
            "add",
            "-qb",
            "lane",
            lane.to_str().ok_or("lane")?,
        ],
    )?;
    let writer = WriterRequest {
        project,
        worktree: lane.clone(),
    };
    let request = WriterDeliveryRequest {
        mode: Default::default(),
        remote: "origin".into(),
        branch: "published/lane".into(),
        open_pr: false,
    };
    let host = host.prepare(&writer, &request)?;
    assert!(
        !git(
            &lane,
            &["for-each-ref", "--format=%(refname)", "refs/heads/injected"]
        )?
        .contains("injected")
    );
    assert_eq!(
        git(&lane, &["rev-parse", "refs/remotes/origin/base"])?,
        initial
    );
    // The writer's committed head, rather than any provider's prose, is delivered.
    std::fs::write(lane.join("change"), "committed by the writer")?;
    git(&lane, &["add", "change"])?;
    git(&lane, &["commit", "-qm", "Writer change"])?;
    git(&lane, &["config", "push.followTags", "true"])?;
    git(
        &lane,
        &["tag", "-a", "local-only", "-m", "Do not deliver this tag"],
    )?;
    let hook = writer.project.join(".git/hooks/pre-push");
    std::fs::write(&hook, "#!/bin/sh\nexit 42\n")?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700))?;
    let head = GitOid::parse(&git(&lane, &["rev-parse", "HEAD"])?)?;
    let head_text = head.to_string();
    let fixture = root.join("store");
    std::fs::create_dir(&fixture)?;
    let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
    let call = tests::call(&clock, &cancel);
    let store = RunStore::open(&tests::context(&fixture)?, &call)?;
    let mut input = tests::input();
    input.request.tasks[0].writer = Some(writer.clone());
    input.request.tasks[0].deliver = Some(request.clone());
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    // A lane that rewrites the stored destination after launch cannot steer
    // the host's push: the destination bound at prepare wins.
    let rewritten = WriterDeliveryRequest {
        branch: "main".into(),
        ..request.clone()
    };
    assert!(matches!(
        deliver_once(&host, &writer, &rewritten, &attempt, "fixture receipt"),
        Err(RunError::Validation(
            "delivery destination changed after launch"
        ))
    ));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/main"])?.is_empty());
    let receipt = deliver_once(&host, &writer, &request, &attempt, "fixture receipt")?;
    assert_eq!(receipt.head, Some(head));
    assert!(matches!(
        receipt.state,
        WriterDeliveryState::Delivered { pull_request: None }
    ));
    assert_eq!(
        git(&lane, &["rev-parse", "refs/remotes/origin/published/lane"])?,
        head_text.as_str()
    );
    assert_eq!(git(&lane, &["config", "branch.lane.remote"])?, "origin");
    assert_eq!(
        git(&lane, &["config", "branch.lane.merge"])?,
        "refs/heads/published/lane"
    );
    assert!(
        git(&lane, &["ls-remote", "origin", "refs/heads/published/lane"])?
            .starts_with(head_text.as_str())
    );
    assert!(git(&lane, &["ls-remote", "--tags", "origin"])?.is_empty());
    dispatch_tests::seal_exit(&store, &tests::exit())?;
    let record = store.publish_terminal(id, 1, &tests::publication(), &tests::exit(), &call)?;
    assert_eq!(record.delivery.as_ref(), Some(&receipt));
    assert_eq!(store.record(id, &call)?.head, Some(head));
    // A repeated terminal callback must not push a newer head or repeat PR creation.
    git(&lane, &["commit", "--allow-empty", "-qm", "Later"])?;
    assert_eq!(
        deliver_once(&host, &writer, &request, &attempt, "fixture receipt")?,
        receipt
    );
    assert!(
        git(&lane, &["ls-remote", "origin", "refs/heads/published/lane"])?
            .starts_with(head_text.as_str())
    );
    // A writer that made no commit reports `no_new_commits` and pushes nothing.
    let idle_lane = root.join("idle");
    git(
        &writer.project,
        &[
            "worktree",
            "add",
            "-qb",
            "idle",
            idle_lane.to_str().ok_or("idle")?,
        ],
    )?;
    let idle_writer = WriterRequest {
        project: writer.project.clone(),
        worktree: idle_lane.clone(),
    };
    let idle_request = WriterDeliveryRequest {
        branch: "published/idle".into(),
        ..request.clone()
    };
    let idle_host =
        HostDeliveryContext::new(environment.clone()).prepare(&idle_writer, &idle_request)?;
    let mut idle = tests::input();
    idle.run_id = provenant_core::RunId::from_bytes([17; 16]);
    idle.operation_id = provenant_core::OperationId::parse("idle-delivery")?;
    idle.attempts[0].attempt_id = provenant_core::AttemptId::from_bytes([18; 16]);
    idle.request.tasks[0].writer = Some(idle_writer.clone());
    idle.request.tasks[0].deliver = Some(idle_request.clone());
    store.create(&idle, &call)?;
    let idle_attempt = store.attempt_dir(idle.run_id, idle.attempts[0].attempt_id, false)?;
    let idle_receipt = deliver_once(
        &idle_host,
        &idle_writer,
        &idle_request,
        &idle_attempt,
        "fixture receipt",
    )?;
    assert_eq!(idle_receipt.state, WriterDeliveryState::NoNewCommits);
    assert_eq!(idle_receipt.head, Some(GitOid::parse(&initial)?));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/published/idle"])?.is_empty());
    // An interrupted delivery is recorded during terminal recovery, with no push.
    let mut interrupted = tests::input();
    interrupted.run_id = provenant_core::RunId::from_bytes([14; 16]);
    interrupted.operation_id = provenant_core::OperationId::parse("interrupted-delivery")?;
    interrupted.attempts[0].attempt_id = provenant_core::AttemptId::from_bytes([16; 16]);
    interrupted.request.tasks[0].writer = Some(writer);
    interrupted.request.tasks[0].deliver = Some(WriterDeliveryRequest {
        branch: "interrupted".into(),
        ..request
    });
    store.create(&interrupted, &call)?;
    let id = interrupted.attempts[0].attempt_id;
    let attempt = store.attempt_dir(interrupted.run_id, id, false)?;
    let pending = WriterDeliveryReceipt {
        branch: "interrupted".into(),
        state: WriterDeliveryState::Failed {
            stage: WriterDeliveryStage::Interrupted,
        },
        ..receipt
    };
    artifacts::seal(
        &attempt,
        "delivery-started.json",
        &db::bytes(&pending)?,
        RECORD_LIMIT,
    )?;
    let mut exit = tests::exit();
    exit.scope.run = interrupted.run_id;
    exit.scope.attempt = id;
    dispatch_tests::seal_exit(&store, &exit)?;
    artifacts::seal(
        &attempt,
        "result.md",
        tests::publication().result.as_bytes(),
        RESULT_LIMIT,
    )?;
    let record = store.recover_result_only(id, &tests::publication(), &exit, &call)?;
    assert_eq!(record.delivery, Some(pending));
    assert!(git(&lane, &["ls-remote", "origin", "refs/heads/interrupted"])?.is_empty());
    Ok(())
}

fn registered_landed_writer_fixture(
    fixture: &PrepareFixture,
) -> std::result::Result<ExecutionContext, Box<dyn std::error::Error>> {
    struct Free;
    impl provenant_confinement::worktrees::WorkerObserver for Free {
        fn observe_roots(
            &self,
            roots: &[std::path::PathBuf],
        ) -> std::collections::BTreeMap<
            std::path::PathBuf,
            provenant_confinement::worktrees::WorkerState,
        > {
            roots
                .iter()
                .cloned()
                .map(|root| (root, provenant_confinement::worktrees::WorkerState::Free))
                .collect()
        }
    }
    use provenant_core::{CancelToken, Clock, RootInputs};
    use provenant_os::clock::SystemClock;
    let mut execution = tests::context(&fixture.root)?;
    let mut inputs = RootInputs::new(fixture.root.join("home"), fixture.root.join("checkout"));
    inputs.state_dir = Some(execution.state_root.as_path().into());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    std::fs::create_dir_all(roots.state.as_path())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        roots.state.as_path(),
        std::fs::Permissions::from_mode(0o700),
    )?;
    let clock = SystemClock;
    let cancel = CancelToken::new();
    execution.project_id = provenant_confinement::worktrees::with_context(
        &roots,
        &fixture.root.join("home"),
        &fixture.writer.project,
        &clock,
        &cancel,
        |context| {
            let context = provenant_confinement::worktrees::WorktreeContext {
                workers: &Free,
                ..*context
            };
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                attempt_id: None,
                operation_id: None,
                branch: "lane".into(),
                base: String::new(),
                host: provenant_core::HostId::from_bytes([3; 16]),
                created_at: clock
                    .wall_now()
                    .map_err(|_| provenant_confinement::worktrees::WorktreeError::InvalidOutput)?,
                origin: provenant_confinement::worktrees::WorktreeOrigin::Adopted,
                label: None,
            };
            let head = provenant_confinement::run_utility(
                context.executor,
                provenant_confinement::GitRead::RevParseHead,
                context.repo,
                context.utility,
                context.utility_budget,
                &cancel,
            )?;
            let owner = provenant_confinement::worktrees::WorktreeOwner {
                base: String::from_utf8_lossy(&head.stdout.bytes).trim().into(),
                ..owner
            };
            provenant_confinement::worktrees::adopt(
                &context,
                &fixture.writer.worktree,
                &owner,
                &cancel,
            )?;
            Ok(context.binding.project_id())
        },
    )?;
    Ok(execution)
}

#[derive(Clone, Copy)]
enum LandedWriterFenceCase {
    Terminal,
    Nonterminal,
    WrongScope,
    WrongWriter,
    WrongAttempt,
    MissingQuarantine,
}

#[test]
fn land_settlement_clears_terminal_writer_reservation()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    landed_writer_fence_fixture(LandedWriterFenceCase::Terminal)
}

#[test]
fn land_settlement_refuses_nonterminal_writer()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    landed_writer_fence_fixture(LandedWriterFenceCase::Nonterminal)
}

#[test]
fn land_settlement_refuses_mismatched_or_incomplete_writer_evidence()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for case in [
        LandedWriterFenceCase::WrongScope,
        LandedWriterFenceCase::WrongWriter,
        LandedWriterFenceCase::WrongAttempt,
        LandedWriterFenceCase::MissingQuarantine,
    ] {
        landed_writer_fence_fixture(case)?;
    }
    Ok(())
}

fn landed_writer_fence_fixture(
    case: LandedWriterFenceCase,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use provenant_os::fs::Directory;
    use provenant_supervisor::custody::{self, WriterBinding};
    let fixture = PrepareFixture::new()?;
    let execution = registered_landed_writer_fixture(&fixture)?;
    let (clock, cancel) = (tests::clock(), provenant_core::CancelToken::new());
    let call = tests::call(&clock, &cancel);
    let store = RunStore::open(&execution, &call)?;
    let mut input = tests::input();
    input.request.tasks[0].writer = Some(fixture.writer.clone());
    store.create(&input, &call)?;
    let id = input.attempts[0].attempt_id;
    let record = store.mark_starting(
        &store.record(id, &call)?,
        &tests::route(),
        &[],
        &Default::default(),
        None,
        &call,
    )?;
    let scope = RunStore::scope(&record);
    let attempt = store.attempt_dir(input.run_id, id, false)?;
    if !matches!(case, LandedWriterFenceCase::Nonterminal) {
        let mut exit = tests::exit();
        exit.scope = scope;
        tests::seal_observation(&store, &input, &exit)?;
        store.publish_terminal(id, record.revision, &tests::publication(), &exit, &call)?;
    }
    if !matches!(case, LandedWriterFenceCase::MissingQuarantine) {
        let archive = artifacts::subdir(&attempt, std::ffi::OsStr::new("quarantine"), true)?;
        artifacts::seal(
            &archive,
            "manifest.json",
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "scope": scope,
                "kind": "quarantine_manifest",
            }))?,
            RECORD_LIMIT,
        )?;
    }
    let mut launch = crate::prepare_writer(
        &fixture.writer,
        &store.writer_registry,
        execution.project_id,
    )?;
    let binding = WriterBinding::from_verified(
        launch.worktree.repo().root_dir(),
        launch.worktree.repo().private_dir(),
        launch.branch.as_str(),
        launch.base,
    )?;
    let directory = launch.custody.take().ok_or("writer custody")?.0;
    drop(provenant_supervisor::fixture::acquire_writer_with_binding(
        directory,
        scope,
        binding,
        call.budget,
        &cancel,
        &clock,
    )?);
    let reservation = store
        .writer_registry
        .join("worktrees")
        .join(execution.project_id.to_string())
        .join(provenant_confinement::worktrees::state_dir_name(
            &fixture.writer.worktree,
        ))
        .join("reservation.json");
    let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&reservation)?)?;
    let metadata = attempt.metadata()?;
    value["launch_state"] = "never_spawned".into();
    value["unresolved_cleanup"] = false.into();
    value["census_incomplete"] = false.into();
    value["attempt_device"] = metadata.device.into();
    value["attempt_inode"] = metadata.inode.into();
    match case {
        LandedWriterFenceCase::WrongScope => {
            value["scope"]["generation"] = (scope.generation + 1).into();
        }
        LandedWriterFenceCase::WrongWriter => {
            value["binding"]["branch"] = "refs/heads/other".into();
        }
        LandedWriterFenceCase::WrongAttempt => {
            value["attempt_inode"] = (metadata.inode + 1).into();
        }
        _ => {}
    }
    std::fs::write(&reservation, serde_json::to_vec(&value)?)?;
    let before = std::fs::read(&reservation)?;
    let recovery = custody::acquire_recovery(
        Directory::open_anchor(reservation.parent().ok_or("custody directory")?)?,
        call.budget,
        &cancel,
        &clock,
    )?;
    let result = store.settle_landed_writer(recovery, &call);
    if matches!(case, LandedWriterFenceCase::Terminal) {
        result?;
        assert!(!reservation.exists());
        assert!(reservation.with_file_name("settlement.json").is_file());
        assert!(artifacts::read_sealed(&attempt, "release.json", RECORD_LIMIT)?.is_some());
        assert!(matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));
    } else {
        assert!(result.is_err());
        assert_eq!(std::fs::read(&reservation)?, before);
        if !matches!(case, LandedWriterFenceCase::WrongAttempt) {
            assert!(artifacts::read_sealed(&attempt, "release.json", RECORD_LIMIT)?.is_none());
        }
    }
    assert!(fixture.writer.worktree.exists());
    Ok(())
}
