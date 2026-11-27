use super::{
    GitRefName, GitRevision, GitRevisionError, GitdirCustody, RepoError, RepoObjectFormat,
    RepoProbeData, SiblingTrackingKeys, VerifiedRepo, verify_worktree, verify_worktree_with,
};
use provenant_records::policy::{Facts, Policy};
use std::fs;

/// Each row is `git check-ref-format --branch <name>` (git-check-ref-format(1),
/// "--branch"), observed with Git 2.53: true when it exits 0. `@{` forms
/// are excluded because `--branch` treats them as shorthand to expand.
const BRANCH_TABLE: &[(&str, bool)] = &[
    ("feat/a+b", true),
    ("fix#12", true),
    ("user@x", true),
    ("@", true),
    ("@a", true),
    ("a@", true),
    ("a/@", true),
    ("@/a", true),
    ("@@", true),
    ("head", true),
    ("caf\u{e9}", true),
    ("a\"b", true),
    ("a{b", true),
    ("a}b", true),
    ("a.lock.b", true),
    ("lock", true),
    ("x/y.z", true),
    ("v1.0", true),
    ("a/-b", true),
    ("", false),
    ("HEAD", false),
    ("-x", false),
    ("--x", false),
    ("/x", false),
    ("x/", false),
    ("x//y", false),
    (".", false),
    ("..", false),
    (".x", false),
    ("x/.y", false),
    ("x.", false),
    ("a/b.", false),
    ("a..b", false),
    ("a@{b", false),
    ("a b", false),
    ("a~b", false),
    ("a^b", false),
    ("a:b", false),
    ("a?b", false),
    ("a*b", false),
    ("a[b", false),
    ("a\\b", false),
    ("a\u{7f}b", false),
    ("a\nb", false),
    ("a\tb", false),
    ("x.lock", false),
    ("x.lock/y", false),
    ("a/b.lock", false),
    (".lock", false),
    ("a/..", false),
];

#[test]
fn sibling_tracking_keys_follow_the_checked_out_head() {
    let branch = SiblingTrackingKeys::for_head(b"ref: refs/heads/main\n");
    assert!(branch.matches(b"branch.lane.remote"));
    assert!(branch.matches(b"branch.lane.merge"));
    assert!(!branch.matches(b"branch.main.remote"));
    assert!(!branch.matches(b"remote.origin.url"));
    let detached = format!("{}\n", "a".repeat(40));
    assert!(SiblingTrackingKeys::for_head(detached.as_bytes()).matches(b"branch.main.merge"));
    let malformed = SiblingTrackingKeys::for_head(b"garbage\n");
    assert!(!malformed.matches(b"branch.lane.remote"));
    assert!(!SiblingTrackingKeys::for_head(b"").matches(b"branch.lane.remote"));
}

#[test]
fn branch_names_follow_git_ref_format() {
    for (name, accepted) in BRANCH_TABLE {
        assert_eq!(GitRefName::branch(name).is_ok(), *accepted, "{name:?}");
    }
}

/// The table above can drift from Git; ask the installed Git directly.
#[test]
fn branch_names_match_the_installed_git() {
    let Some(git) = provenant_os::git::git_executable() else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        provenant_os::fixture::git_command_at(&git)
            .current_dir(temp.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(args)
            .output()
            .unwrap()
    };
    assert!(run(&["init", "--quiet"]).status.success());
    for (name, accepted) in BRANCH_TABLE {
        let output = run(&["check-ref-format", "--branch", name]);
        assert_eq!(output.status.success(), *accepted, "{name:?}");
    }
}

#[test]
fn verify_worktree_refuses_the_primary_checkout() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join(".git")).unwrap();
    fs::write(temp.path().join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
    let repo = VerifiedRepo::open(temp.path(), |_| {
        Ok::<_, RepoError>(RepoProbeData::without_includes(
            RepoObjectFormat::Sha1,
            String::new(),
            Vec::new(),
        ))
    })
    .unwrap();

    let error = verify_worktree(&repo, temp.path())
        .err()
        .expect("primary checkout is not a lane");
    assert!(matches!(error, RepoError::InvalidRegistration));
    let policy = Policy::default();
    let rule = policy.rule("REPO_VERIFY_WORKTREE_GIT_ISOLATION");
    assert_eq!(
        rule.rule.boundary,
        provenant_records::policy::Boundary::OtherWork
    );
    assert_eq!(
        rule.rule.strength,
        provenant_records::policy::Strength::Refuse
    );
    assert!(matches!(
        policy.check(
            "REPO_VERIFY_WORKTREE_GIT_ISOLATION",
            Facts::violation("guard failed"),
        ),
        provenant_records::policy::Effect::Refuse(_)
    ));
}

#[test]
fn git_revision_accepts_frozen_base_refs_and_refuses_option_injection() {
    assert_eq!(
        GitRevision::new("origin/main").unwrap().as_str(),
        "origin/main"
    );
    assert_eq!(GitRevision::new("HEAD~2").unwrap().as_str(), "HEAD~2");
    assert!(matches!(
        GitRevision::new("--upload-pack=evil"),
        Err(GitRevisionError)
    ));
    assert!(matches!(
        GitRevision::new("origin/main\nHEAD"),
        Err(GitRevisionError)
    ));
}

#[test]
fn normalisation_reuses_only_the_exact_successful_snapshot_and_executable() {
    use std::cell::Cell;
    let temp = tempfile::tempdir().unwrap();
    let git = temp.path().join("git");
    let other_git = temp.path().join("other-git");
    fs::write(&git, b"parser").unwrap();
    fs::hard_link(&git, &other_git).unwrap();
    let calls = Cell::new(0);
    let parse = || {
        calls.set(calls.get() + 1);
        Ok(b"core.bare\nfalse\0".to_vec())
    };
    let cache = super::ConfigNormalisationCache::default();
    let first = cache.normalise(&git, b"first", parse).unwrap();
    assert_eq!(cache.normalise(&git, b"first", parse).unwrap(), first);
    assert_eq!(calls.get(), 1);
    cache.normalise(&git, b"second", parse).unwrap();
    // A single-entry cache evicts the first snapshot.
    cache.normalise(&git, b"first", parse).unwrap();
    assert_eq!(calls.get(), 3);
    cache.normalise(&other_git, b"first", parse).unwrap();
    assert_eq!(calls.get(), 4);
    fs::remove_file(&other_git).unwrap();
    fs::write(&other_git, b"replacement parser").unwrap();
    cache.normalise(&other_git, b"first", parse).unwrap();
    assert_eq!(calls.get(), 5);
    for _ in 0..2 {
        assert!(matches!(
            cache.normalise(&other_git, b"changed", || {
                calls.set(calls.get() + 1);
                Err(RepoError::ConfigNormalisation("parser timed out"))
            }),
            Err(RepoError::ConfigNormalisation(_))
        ));
    }
    assert_eq!(calls.get(), 7);
    // A parser replacement during a miss must not publish under its old identity.
    assert!(matches!(
        cache.normalise(&other_git, b"changed", || {
            fs::rename(&other_git, temp.path().join("old-git")).unwrap();
            fs::write(&other_git, b"new parser").unwrap();
            parse()
        }),
        Err(RepoError::ConfigNormalisation(_))
    ));
    cache.normalise(&other_git, b"changed", parse).unwrap();
    assert_eq!(calls.get(), 9);
    assert!(matches!(
        cache.normalise(&git, &vec![0; super::CONFIG_LIMIT as usize + 1], parse),
        Err(RepoError::MetadataTooLarge)
    ));
    assert_eq!(calls.get(), 9);
    assert!(matches!(
        cache.normalise(&git, b"oversized", || Ok(vec![
            0;
            super::CONFIG_LIMIT as usize
                * 2
                + 1
        ])),
        Err(RepoError::ConfigNormalisation(_))
    ));
    cache.normalise(&git, b"oversized", parse).unwrap();
    assert_eq!(calls.get(), 10);
}

#[test]
fn concurrent_cache_misses_do_not_hold_the_cache_lock() {
    use std::sync::mpsc;
    use std::time::Duration;
    let temp = tempfile::tempdir().unwrap();
    let git = temp.path().join("git");
    fs::write(&git, b"parser").unwrap();
    let cache = super::ConfigNormalisationCache::default();
    let (entered, parsing) = mpsc::channel();
    std::thread::scope(|scope| {
        let mut releases = Vec::new();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (release, wait) = mpsc::channel();
                releases.push(release);
                let entered = entered.clone();
                let cache = &cache;
                let git = &git;
                scope.spawn(move || {
                    cache
                        .normalise(git, b"snapshot", || {
                            entered.send(()).unwrap();
                            wait.recv_timeout(Duration::from_secs(5)).unwrap();
                            Ok(b"core.bare\nfalse\0".to_vec())
                        })
                        .unwrap()
                })
            })
            .collect();
        // A blocked parser must not keep other misses from entering. Release
        // every worker before asserting, so a regression fails without hanging.
        let all_entered = (0..8).all(|_| parsing.recv_timeout(Duration::from_secs(2)).is_ok());
        for release in releases {
            let _ = release.send(());
        }
        for handle in handles {
            assert_eq!(handle.join().unwrap(), b"core.bare\nfalse\0");
        }
        assert!(all_entered, "cache readers queued behind a parser");
    });
    assert_eq!(
        cache
            .normalise(&git, b"snapshot", || panic!("warm cache miss"))
            .unwrap(),
        b"core.bare\nfalse\0"
    );
}

#[test]
fn warm_normalisation_still_refuses_unsafe_git_metadata() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let git = temp.path().join(".git");
    fs::create_dir(&git).unwrap();
    fs::write(git.join("config"), b"[core]\nrepositoryformatversion = 0\n").unwrap();
    fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    let repo = VerifiedRepo::open(temp.path(), |_| {
        Ok::<_, RepoError>(RepoProbeData::without_includes(
            RepoObjectFormat::Sha1,
            String::new(),
            Vec::new(),
        ))
    })
    .unwrap();
    fs::hard_link(git.join("config"), git.join("alias")).unwrap();
    assert!(matches!(
        repo.revalidate(),
        Err(RepoError::InvalidGitDirectory)
    ));
    fs::remove_file(git.join("alias")).unwrap();
    fs::rename(git.join("config"), git.join("saved-config")).unwrap();
    std::os::unix::fs::symlink("saved-config", git.join("config")).unwrap();
    assert!(matches!(
        repo.revalidate(),
        Err(RepoError::InvalidGitDirectory)
    ));
    fs::remove_file(git.join("config")).unwrap();
    fs::rename(git.join("saved-config"), git.join("config")).unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o720)).unwrap();
    assert!(matches!(
        repo.revalidate(),
        Err(RepoError::InvalidGitMarker)
    ));
}

#[test]
fn normalisation_failures_refuse_without_claiming_unsafe_metadata() {
    use provenant_os::exec::{CapturedStream, ExecError, ExecOutcome, Termination};
    let complete = ExecOutcome {
        termination: Termination::Exit(0),
        stdout: CapturedStream {
            bytes: b"core.bare\nfalse\0".to_vec(),
            ..Default::default()
        },
        stderr: CapturedStream::default(),
        timed_out: false,
        drain_truncated: false,
        overflow_stopped: false,
        cancelled: false,
        elapsed_micros: 1,
    };
    assert_eq!(
        super::normalised_config_output(Ok(complete.clone())).unwrap(),
        complete.stdout.bytes
    );
    let mut failures = Vec::new();
    let mut timed_out = complete.clone();
    timed_out.termination = Termination::Signal(15);
    timed_out.timed_out = true;
    failures.push(timed_out);
    let mut read_failed = complete.clone();
    read_failed.stdout.read_failed = true;
    failures.push(read_failed);
    let mut truncated = complete.clone();
    truncated.stdout.truncated = true;
    failures.push(truncated);
    failures.push(ExecOutcome {
        drain_truncated: true,
        ..complete.clone()
    });
    failures.push(ExecOutcome {
        termination: Termination::Exit(128),
        ..complete
    });
    for failure in failures {
        assert!(matches!(
            super::normalised_config_output(Ok(failure)),
            Err(RepoError::ConfigNormalisation(_))
        ));
    }
    assert!(matches!(
        super::normalised_config_output(Err(ExecError::Io {
            operation: "spawn",
            source: std::io::Error::from(std::io::ErrorKind::Interrupted),
        })),
        Err(RepoError::ConfigNormalisationExecution(ExecError::Io {
            operation: "spawn",
            ..
        }))
    ));
}

#[test]
fn git_directory_still_refuses_wrong_owner_writable_modes_and_wrong_kinds() {
    use provenant_os::fs::{FileKind, Metadata};
    let temp = tempfile::tempdir().unwrap();
    let safe = Metadata::from_std(fs::metadata(temp.path()).unwrap());
    assert!(super::verify_git_directory(safe, safe.uid).is_ok());
    for unsafe_metadata in [
        Metadata {
            uid: safe.uid.wrapping_add(1),
            ..safe
        },
        Metadata {
            mode: safe.mode | 0o020,
            ..safe
        },
        Metadata {
            mode: safe.mode | 0o002,
            ..safe
        },
        Metadata {
            kind: FileKind::Regular,
            ..safe
        },
        Metadata {
            kind: FileKind::Symlink,
            ..safe
        },
        Metadata {
            kind: FileKind::Other,
            ..safe
        },
    ] {
        assert!(matches!(
            super::verify_git_directory(unsafe_metadata, safe.uid),
            Err(RepoError::InvalidGitDirectory)
        ));
    }
}

#[test]
fn repo_revalidation_detects_config_changes() {
    let temp = tempfile::tempdir().unwrap();
    let git = temp.path().join(".git");
    fs::create_dir(&git).unwrap();
    fs::write(git.join("config"), b"[core]\nrepositoryformatversion = 0\n").unwrap();
    fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    let repo = VerifiedRepo::open(temp.path(), |_| {
        Ok::<_, RepoError>(RepoProbeData::without_includes(
            RepoObjectFormat::Sha1,
            String::new(),
            Vec::new(),
        ))
    })
    .unwrap();
    fs::write(
        git.join("config"),
        b"[filter.poison]\nclean = touch /tmp/pwned\n",
    )
    .unwrap();

    assert!(matches!(repo.revalidate(), Err(RepoError::IdentityChanged)));
}

#[test]
fn repo_revalidation_accepts_a_tracked_sibling_worktree() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    fs::create_dir(&root).unwrap();
    let git = |args: &[&str]| {
        let result = provenant_os::fixture::git_command()
            .current_dir(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["commit", "--allow-empty", "-qm", "fixture"]);
    git(&["remote", "add", "origin", "https://example.invalid/repo"]);
    git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "first",
        temp.path().join("first").to_str().unwrap(),
        "HEAD",
    ]);
    let repo = VerifiedRepo::open(&root, |_| {
        Ok::<_, RepoError>(RepoProbeData::without_includes(
            RepoObjectFormat::Sha1,
            String::new(),
            Vec::new(),
        ))
    })
    .unwrap();
    let tree = verify_worktree(&repo, temp.path().join("first")).unwrap();
    git(&[
        "worktree",
        "add",
        "-q",
        "--track",
        "-b",
        "second",
        temp.path().join("second").to_str().unwrap(),
        "origin/main",
    ]);
    assert!(tree.repo().revalidate().is_ok());
    git(&["config", "branch.third.remote", "origin"]);
    assert!(tree.repo().revalidate().is_ok());
    let partial = VerifiedRepo::open(&root, |_| {
        Ok::<_, RepoError>(RepoProbeData::without_includes(
            RepoObjectFormat::Sha1,
            String::new(),
            Vec::new(),
        ))
    })
    .unwrap();
    let partial_tree = verify_worktree(&partial, temp.path().join("first")).unwrap();
    git(&["config", "branch.third.merge", "refs/heads/main"]);
    assert!(partial_tree.repo().revalidate().is_ok());
    assert!(tree.repo().revalidate().is_ok());
    // Writer delivery records a sibling lane's push remote too.
    git(&["config", "branch.third.pushRemote", "origin"]);
    assert!(partial_tree.repo().revalidate().is_ok());
    assert!(tree.repo().revalidate().is_ok());
    // Any variable recorded for a sibling branch leaves the digest unchanged.
    git(&["config", "branch.third.rebase", "true"]);
    assert!(partial_tree.repo().revalidate().is_ok());
    assert!(tree.repo().revalidate().is_ok());
    let delivered = VerifiedRepo::open(&root, |_| {
        for (key, value) in [
            ("remote", "origin"),
            ("merge", "refs/heads/delivered"),
            ("pushRemote", "origin"),
            ("rebase", "true"),
        ] {
            git(&[
                "config",
                "--local",
                &format!("branch.delivered.{key}"),
                value,
            ]);
        }
        Ok::<_, RepoError>(RepoProbeData::without_includes(
            RepoObjectFormat::Sha1,
            String::new(),
            Vec::new(),
        ))
    });
    assert!(delivered.is_ok());
    let opened = VerifiedRepo::open(&root, |_| {
        git(&[
            "worktree",
            "add",
            "-q",
            "--track",
            "-b",
            "fourth",
            temp.path().join("fourth").to_str().unwrap(),
            "origin/main",
        ]);
        Ok::<_, RepoError>(RepoProbeData::without_includes(
            RepoObjectFormat::Sha1,
            String::new(),
            Vec::new(),
        ))
    });
    assert!(opened.is_ok());
    git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "dotted.branch",
        temp.path().join("dotted").to_str().unwrap(),
        "HEAD",
    ]);
    let dotted_tree = verify_worktree(&repo, temp.path().join("dotted")).unwrap();
    let config_path = root.join(".git/config");
    let config = fs::read(&config_path).unwrap();
    // Git decodes quoted subsection names before the comparison.
    let mut escaped = config.clone();
    escaped.extend_from_slice(
        b"[branch \"sibling\\\"quoted\"]\n remote = origin\n merge = refs/heads/main\n",
    );
    fs::write(&config_path, &escaped).unwrap();
    assert!(tree.repo().revalidate().is_ok());
    escaped.extend_from_slice(b"[branch \"first\"]\n remote = elsewhere\n");
    fs::write(&config_path, &escaped).unwrap();
    assert!(tree.repo().revalidate().is_err());
    fs::write(&config_path, b"[broken\n").unwrap();
    assert!(tree.repo().revalidate().is_err());
    // A two-part branch key with no subsection still invalidates the digest.
    let mut auto_setup = config.clone();
    auto_setup.extend_from_slice(b"[branch]\n autosetupmerge = always\n");
    fs::write(&config_path, auto_setup).unwrap();
    assert!(matches!(
        tree.repo().revalidate(),
        Err(RepoError::IdentityChanged)
    ));
    fs::write(&config_path, &config).unwrap();
    // The checked-out branch's own rebase still matters.
    let mut own_rebase = config.clone();
    own_rebase.extend_from_slice(b"[branch \"first\"]\n rebase = true\n");
    fs::write(&config_path, own_rebase).unwrap();
    assert!(matches!(
        tree.repo().revalidate(),
        Err(RepoError::IdentityChanged)
    ));
    fs::write(&config_path, &config).unwrap();
    // The checked-out branch's own push remote still matters.
    let mut own_push = config.clone();
    own_push.extend_from_slice(b"[branch \"first\"]\n pushRemote = elsewhere\n");
    fs::write(&config_path, own_push).unwrap();
    assert!(matches!(
        tree.repo().revalidate(),
        Err(RepoError::IdentityChanged)
    ));
    fs::write(&config_path, &config).unwrap();
    // A dotted branch name matches its own keys and ignores sibling keys.
    let mut dotted = config.clone();
    dotted.extend_from_slice(b"[branch \"dotted.branch\"]\n rebase = true\n");
    fs::write(&config_path, &dotted).unwrap();
    assert!(tree.repo().revalidate().is_ok());
    assert!(matches!(
        dotted_tree.repo().revalidate(),
        Err(RepoError::IdentityChanged)
    ));
    fs::write(&config_path, &config).unwrap();
    let mut dotted_siblings = config.clone();
    dotted_siblings.extend_from_slice(
        b"[branch \"dotted.branch.extra\"]\n rebase = true\n[branch \"dotted\"]\n rebase = true\n",
    );
    fs::write(&config_path, dotted_siblings).unwrap();
    assert!(dotted_tree.repo().revalidate().is_ok());
    assert!(tree.repo().revalidate().is_ok());
    fs::write(&config_path, &config).unwrap();
    let mut poisoned = config.clone();
    poisoned.extend_from_slice(b"[core]\n\thooksPath = /tmp/poison\n");
    fs::write(&config_path, poisoned).unwrap();
    assert!(matches!(
        tree.repo().revalidate(),
        Err(RepoError::IdentityChanged)
    ));
    fs::write(&config_path, config).unwrap();
    let marker = temp.path().join("first/.git");
    let marker_bytes = fs::read(&marker).unwrap();
    fs::write(&marker, b"gitdir: /tmp/not-the-original\n").unwrap();
    assert!(tree.repo().revalidate().is_err());
    assert!(tree.repo().revalidate_identity().is_err());
    fs::write(&marker, marker_bytes).unwrap();
    fs::rename(temp.path().join("first"), temp.path().join("moved")).unwrap();
    assert!(tree.repo().revalidate().is_err());
}

/// A real registered worktree, for the Git-registration repair tests.
fn git_fixture() -> Option<(tempfile::TempDir, std::path::PathBuf, std::path::PathBuf)> {
    let git = provenant_os::git::git_executable()?;
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    let run = |args: &[&str], cwd: &std::path::Path| {
        let output = provenant_os::fixture::git_command_at(&git)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["init", "-q"], &project);
    run(
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "commit",
            "--allow-empty",
            "-qm",
            "init",
        ],
        &project,
    );
    let lane = temp.path().join("lane");
    let lane_arg = lane.to_str().unwrap();
    run(
        &["worktree", "add", "-q", "-b", "lane", lane_arg, "HEAD"],
        &project,
    );
    Some((temp, project, lane))
}

fn stub_repo(project: &std::path::Path) -> VerifiedRepo {
    VerifiedRepo::open(project, |_| {
        Ok::<_, RepoError>(RepoProbeData::without_includes(
            RepoObjectFormat::Sha1,
            String::new(),
            Vec::new(),
        ))
    })
    .unwrap()
}

#[test]
fn alternates_warn_and_proceed_without_refusing() {
    let Some((temp, project, _lane)) = git_fixture() else {
        return;
    };
    let info = project.join(".git/objects/info");
    fs::create_dir_all(&info).unwrap();
    fs::write(
        info.join("alternates"),
        format!("{}\n", temp.path().join("borrowed/objects").display()),
    )
    .unwrap();
    let (_repo, warnings) = provenant_records::warning::capture(|| stub_repo(&project));
    assert!(
        warnings
            .iter()
            .any(|warning| warning.code.as_ref() == "REPO_REJECT_ALTERNATES_DATA_INTEGRITY"),
        "a borrowed object store must warn and proceed, got {warnings:?}"
    );
    // The opened handle is usable: the alternates declaration is not a
    // boundary and does not poison the repository for history reads.
    let repo = stub_repo(&project);
    assert!(repo.revalidate().is_ok());
}

#[test]
fn stale_gitdir_repairs_only_own_backlink() {
    let Some((temp, project, old_lane)) = git_fixture() else {
        return;
    };
    let repo = stub_repo(&project);
    let lane = temp.path().join("moved-lane");
    fs::rename(&old_lane, &lane).unwrap();
    let backlink = project.join(".git/worktrees/lane/gitdir");
    let stale = fs::read(&backlink).unwrap();

    let (result, warnings) = provenant_records::warning::capture(|| verify_worktree(&repo, &lane));
    assert!(
        result.is_ok(),
        "stale registration should repair: {:?}",
        result.as_ref().err()
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.code.as_ref() == "GITDIR_REGISTRATION_STALE_NONE"),
        "repair must warn under its none/git row, got {warnings:?}"
    );
    let repaired = fs::read(&backlink).unwrap();
    assert_ne!(repaired, stale, "repair must rewrite the backlink");
    // Git canonicalises the worktree path (on macOS /var resolves to
    // /private/var) before it writes the backlink.
    let canonical_marker = fs::canonicalize(&lane).unwrap().join(".git");
    assert_eq!(
        repaired,
        format!("{}\n", canonical_marker.display()).into_bytes(),
        "repair must point the backlink at this worktree only"
    );
}

#[test]
fn gitdir_held_by_live_lane_refuses() {
    let Some((temp, project, old_lane)) = git_fixture() else {
        return;
    };
    let repo = stub_repo(&project);
    let lane = temp.path().join("moved-lane");
    fs::rename(&old_lane, &lane).unwrap();
    let backlink = project.join(".git/worktrees/lane/gitdir");
    let stale = fs::read(&backlink).unwrap();

    let error = verify_worktree_with(
        &repo,
        &lane,
        GitdirCustody {
            live_holder: Some("attempt-42".to_owned()),
        },
    )
    .err()
    .expect("a live lease must stop the repair");
    assert!(
        matches!(error, RepoError::GitdirHeldByOtherLane { ref holder } if holder == "attempt-42"),
        "refusal must name the holder: {error:?}"
    );
    assert_eq!(
        fs::read(&backlink).unwrap(),
        stale,
        "a live lease must not be rewritten"
    );
}

#[test]
fn fabricated_gitdir_layout_is_operation_error() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let lane = temp.path().join("lane");
    let private = temp.path().join("outside-private");
    let common = temp.path().join("common");
    for dir in [&project, &lane, &private, &common] {
        fs::create_dir(dir).unwrap();
    }
    fs::create_dir(project.join(".git")).unwrap();
    fs::write(project.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
    fs::write(private.join("commondir"), b"../common\n").unwrap();
    fs::write(
        lane.join(".git"),
        format!("gitdir: {}\n", private.display()),
    )
    .unwrap();
    let repo = stub_repo(&project);

    let error = verify_worktree(&repo, &lane).err().unwrap();
    assert!(
        matches!(error, RepoError::GitdirLayoutInvalid { .. }),
        "a fabricated registration is a typed operation error: {error:?}"
    );
    assert_eq!(error.variant(), "GitdirLayoutInvalid");
}
