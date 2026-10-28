#![cfg(target_os = "macos")]
#![allow(clippy::disallowed_methods, clippy::expect_used, clippy::print_stderr)]

use provenant_confinement::permissions::{
    AccessMode, AnchoredDirectory, Anchors, BackendContext, NetworkAccess, PathGrant, PathScope,
    PlanPermissions, ProviderProfile, ProviderProfileSpec, WorktreeWriteAuthority,
    prepare_writer_directories, required_anchors, system_backend,
};
use provenant_confinement::repo::{GitRefName, verify_worktree};
use provenant_confinement::{UtilityBudget, UtilityRuntime};
use provenant_core::CancelToken;
use provenant_os::exec::SystemExecutor;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

fn git(git: &Path, git_dir: &Path, tree: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(git)
        .current_dir(tree)
        .args(args)
        .env("GIT_DIR", git_dir)
        .env("GIT_WORK_TREE", tree)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("run Git")
}

fn ok(output: Output) -> String {
    assert!(
        output.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 Git output")
}

#[test]
fn linked_lane_uses_private_objects_and_cannot_mutate_shared_objects() -> Result<(), Box<dyn Error>>
{
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let source = root.join("source");
    let repo = root.join("repo");
    let lane = root.join("lane");
    let scratch = root.join("scratch");
    let push = root.join("push");
    for path in [&home, &source, &repo, &scratch, &push] {
        fs::create_dir(path)?;
    }
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700))?;
    let git_path = [
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
        "/usr/bin/git",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
    .ok_or("Git unavailable")?
    .canonicalize()?;
    let source_git = source.join(".git");
    let repo_git = repo.join(".git");
    let push_git = push.join(".git");
    for (dir, tree) in [
        (&source_git, &source),
        (&repo_git, &repo),
        (&push_git, &push),
    ] {
        ok(git(
            &git_path,
            dir,
            tree,
            &home,
            &["init", "-q", "-b", "main"],
        ));
        ok(git(
            &git_path,
            dir,
            tree,
            &home,
            &["config", "user.name", "Fixture"],
        ));
        ok(git(
            &git_path,
            dir,
            tree,
            &home,
            &["config", "user.email", "fixture@example.invalid"],
        ));
    }
    fs::write(source.join("base"), "shared base blob\n")?;
    ok(git(
        &git_path,
        &source_git,
        &source,
        &home,
        &["add", "base"],
    ));
    ok(git(
        &git_path,
        &source_git,
        &source,
        &home,
        &["commit", "--allow-empty", "-m", "initial"],
    ));
    ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &[
            "remote",
            "add",
            "origin",
            source.to_str().ok_or("source path")?,
        ],
    ));
    ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            push.to_str().ok_or("push path")?,
        ],
    ));
    ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &["-c", "protocol.file.allow=always", "fetch", "origin"],
    ));
    ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    ));
    ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &["repack", "-a", "-d"],
    ));
    fs::write(repo.join("sentinel"), "existing shared loose object\n")?;
    let sentinel = ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &["hash-object", "-w", "sentinel"],
    ));
    let sentinel = sentinel.trim();
    let loose_object = repo_git
        .join("objects")
        .join(&sentinel[..2])
        .join(&sentinel[2..]);
    let mut shared_objects = vec![loose_object];
    shared_objects.extend(
        fs::read_dir(repo_git.join("objects/pack"))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|path| {
                matches!(
                    path.extension().and_then(|ext| ext.to_str()),
                    Some("pack" | "idx")
                )
            }),
    );
    assert_eq!(shared_objects.len(), 3, "loose, pack and index fixtures");
    let origin_main = repo_git.join("refs/remotes/origin/main");
    fs::write(
        &origin_main,
        ok(git(
            &git_path,
            &source_git,
            &source,
            &home,
            &["rev-parse", "HEAD"],
        )),
    )?;
    ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &["pack-refs", "--all"],
    ));
    ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &[
            "worktree",
            "add",
            "-b",
            "lane/test",
            lane.to_str().ok_or("lane path")?,
            "refs/remotes/origin/main",
        ],
    ));
    let oid = ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &["rev-parse", "refs/heads/lane/test"],
    ));
    ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &["pack-refs", "--all"],
    ));
    let loose = repo_git.join("refs/heads/lane/test");
    assert!(!loose.exists(), "fixture branch must start packed");
    fs::create_dir_all(loose.parent().ok_or("ref parent")?)?;
    let lock = loose.with_extension("lock");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)?;
    file.write_all(oid.as_bytes())?;
    drop(file);
    fs::rename(lock, &loose)?;
    let show = ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &["show-ref", "--verify", "--hash", "refs/heads/lane/test"],
    ));
    let each = ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/heads/lane/test",
        ],
    ));
    assert_eq!(show, oid);
    assert_eq!(each, oid);
    ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &["config", "branch.lane/test.remote", "origin"],
    ));
    ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &["config", "branch.lane/test.merge", "refs/heads/main"],
    ));
    ok(git(
        &git_path,
        &source_git,
        &source,
        &home,
        &["commit", "--allow-empty", "-m", "advance"],
    ));
    // The host refreshes tracking refs before verifying and launching the lane.
    ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &["-c", "protocol.file.allow=always", "fetch", "origin"],
    ));
    fs::create_dir_all(repo_git.join("refs/remotes/upstream"))?;
    fs::create_dir_all(repo_git.join("refs/remotes/origin/lane"))?;
    fs::create_dir_all(repo_git.join("logs/refs/remotes/origin/lane"))?;
    let executor = SystemExecutor::new(2)?;
    let cancel = CancelToken::new();
    let runtime = UtilityRuntime::new(&git_path, &home)?;
    let verified = runtime.open_repo(
        &executor,
        &repo,
        UtilityBudget::bounded(Duration::from_secs(10))?,
        &cancel,
    )?;
    let linked = verify_worktree(&verified, &lane)?;
    let selected = GitRefName::branch("lane/test")?;
    let authority = WorktreeWriteAuthority::for_worktree(&linked, &selected, &home)?;
    prepare_writer_directories(&linked, &selected, &authority, &home)?;
    let grant = |path: PathBuf| PathGrant {
        path,
        scope: PathScope::Subtree,
    };
    let exec_path = ok(git(&git_path, &repo_git, &repo, &home, &["--exec-path"]));
    let exec_path = fs::canonicalize(exec_path.trim())?;
    // Homebrew Git loads libraries from other formulae in the same prefix.
    // Grant the canonical prefix so the sandbox can read those dependencies.
    let mut runtime_reads = vec![grant(exec_path.clone())];
    for prefix in ["/opt/homebrew", "/usr/local"] {
        if let Ok(prefix) = fs::canonicalize(prefix)
            && git_path.starts_with(&prefix)
        {
            runtime_reads.push(grant(prefix));
        }
    }
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            allow_process_fork: true,
            runtime_reads,
            tool_exec_roots: ["/bin", "/usr/bin", "/usr/libexec"]
                .iter()
                .map(|p| grant(PathBuf::from(p)))
                .chain([grant(exec_path)])
                .collect(),
            process_executables: vec![PathGrant {
                path: git_path.clone(),
                scope: PathScope::Exact,
            }],
            ..ProviderProfileSpec::default()
        },
        observations: Default::default(),
    };
    let plan = PlanPermissions {
        read_only: None,
        mode: AccessMode::WorktreeWrite(Box::new(authority)),
        scratch: grant(scratch.clone()),
        private_state: None,
        read_roots: vec![grant(source.clone())],
        protected_roots: vec![],
        secret_roots: vec![],
        selected_authentication: None,
        include_continuation: false,
        write_roots: vec![grant(push.clone())],
        network: NetworkAccess::Provider,
        nested_confinement: false,
    };
    let lane_objects = scratch.join("git-objects");
    fs::create_dir_all(lane_objects.join("info"))?;
    fs::write(
        lane_objects.join("info/alternates"),
        format!("{}\n", repo_git.join("objects").display()),
    )?;
    let private_common = scratch.join("git-common");
    fs::create_dir_all(private_common.join("objects"))?;
    fs::create_dir_all(private_common.join("refs/heads/lane"))?;
    fs::write(
        private_common.join("config"),
        fs::read(repo_git.join("config"))?,
    )?;
    fs::write(private_common.join("HEAD"), b"ref: refs/heads/lane/test\n")?;
    // Include newer loose tracking refs as the host's production snapshot does.
    let refs = ok(git(
        &git_path,
        &repo_git,
        &repo,
        &home,
        &[
            "for-each-ref",
            "--sort=refname",
            "--format=%(objectname) %(refname)",
        ],
    ));
    fs::write(
        private_common.join("packed-refs"),
        format!("# pack-refs with: sorted\n{refs}"),
    )?;
    fs::write(private_common.join("refs/heads/lane/test"), &oid)?;
    let mut paths = required_anchors(&plan, &profile, Some((&linked, &selected)));
    paths.insert(push.clone());
    let directories = paths
        .iter()
        .map(AnchoredDirectory::open_canonical)
        .collect::<Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        home: &home,
        directories: &directories,
        worktree: Some((&linked, &selected)),
    };
    let env: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), home.clone().into_os_string()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ];
    // CI requires the native backend; a lane may already have an inherited sandbox.
    let backend = match system_backend(&BackendContext {
        executor: &executor,
        cancel: &cancel,
        env: &env,
    }) {
        Ok(backend) => backend,
        Err(error) if std::env::var("PROVENANT_CONFINEMENT_REQUIRED").as_deref() != Ok("1") => {
            eprintln!("native sandbox unavailable: {error}");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let launch = backend.compile(&plan, &profile, &anchors)?;
    let sbpl = std::str::from_utf8(&launch.profile)?;
    let private = linked.repo().private_git_dir();
    let run_git = |args: &[&str], overlay: bool| {
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command.env_remove("GIT_COMMON_DIR");
        command.env("GIT_DIR", if overlay { &private_common } else { private });
        if overlay {
            command.env("GIT_COMMON_DIR", &private_common);
        }
        command
            .args(["-p", sbpl])
            .current_dir(&lane)
            .arg(&git_path)
            .args(args)
            .env("GIT_OBJECT_DIRECTORY", &lane_objects)
            .env("GIT_CONFIG_COUNT", "2")
            .env("GIT_CONFIG_KEY_0", "gc.auto")
            .env("GIT_CONFIG_VALUE_0", "0")
            .env("GIT_CONFIG_KEY_1", "maintenance.auto")
            .env("GIT_CONFIG_VALUE_1", "false")
            .env("GIT_INDEX_FILE", private.join("index"))
            .env("GIT_WORK_TREE", &lane)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("run sandboxed Git")
    };
    let run = |args: &[&str]| run_git(args, true);
    let run_shared = |args: &[&str]| run_git(args, false);
    let shell = |script: &str, target: &Path, replacement: &Path| {
        Command::new("/usr/bin/sandbox-exec")
            .args(["-p", sbpl, "/bin/sh", "-c", script, "object-probe"])
            .arg(target)
            .arg(replacement)
            .current_dir(&lane)
            .output()
            .expect("run object probe")
    };
    for target in &shared_objects {
        let before = fs::read(target)?;
        let mode = fs::metadata(target)?.permissions().mode();
        for (operation, script) in [
            ("overwrite", "printf replacement > \"$1\""),
            ("truncate", ": > \"$1\""),
            ("rename over", "/bin/mv -f \"$2\" \"$1\""),
            ("unlink", "/bin/rm -f \"$1\""),
            ("chmod", "/bin/chmod 600 \"$1\""),
            (
                "chmod then write",
                "/bin/chmod 600 \"$1\" && printf replacement > \"$1\"",
            ),
        ] {
            let replacement = scratch.join("replacement");
            fs::write(&replacement, b"replacement")?;
            let output = shell(script, target, &replacement);
            assert!(
                !output.status.success()
                    && String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"),
                "{operation} must be refused by Seatbelt for {}: {}",
                target.display(),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fs::read(target)?,
                before,
                "{operation} altered {}",
                target.display()
            );
            assert_eq!(fs::metadata(target)?.permissions().mode(), mode);
        }
        let scratch_link = scratch.join("shared-object-link");
        let output = shell("/bin/ln \"$1\" \"$2\"", target, &scratch_link);
        assert!(
            !output.status.success()
                && String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"),
            "hard-link into scratch must be refused by Seatbelt for {}: {}",
            target.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !scratch_link.exists(),
            "shared object acquired a scratch alias"
        );
        assert_eq!(fs::read(target)?, before);
    }
    for name in [
        "HEAD",
        "HEAD.lock",
        "FETCH_HEAD",
        "COMMIT_EDITMSG",
        "MERGE_HEAD",
        "next-index-123.lock",
    ] {
        let output = shell(
            "printf replacement > \"$1\"",
            &private.join(name),
            &scratch.join("replacement"),
        );
        assert!(
            !output.status.success()
                && String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"),
            "actual linked metadata {name} must stay immutable: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // Positive control: all executable probes must be able to mutate scratch.
    for script in [
        "printf replacement > \"$1\"",
        ": > \"$1\"",
        "/bin/mv -f \"$2\" \"$1\"",
        "/bin/rm -f \"$1\"",
        "/bin/chmod 600 \"$1\"",
    ] {
        let target = scratch.join("control");
        let replacement = scratch.join("replacement");
        fs::write(&target, b"original")?;
        fs::write(&replacement, b"replacement")?;
        let output = shell(script, &target, &replacement);
        assert!(
            output.status.success(),
            "control failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let scratch_source = scratch.join("hard-link-source");
    let scratch_link = scratch.join("hard-link-control");
    fs::write(&scratch_source, b"scratch hard-link control")?;
    // The real lane policy denies file-link everywhere, including scratch.
    // An allow-all native profile proves the same executable and filesystem
    // support hard links, without weakening the lane-policy denial above.
    let output = Command::new("/usr/bin/sandbox-exec")
        .args(["-p", "(version 1)(allow default)", "/bin/ln"])
        .arg(&scratch_source)
        .arg(&scratch_link)
        .current_dir(&lane)
        .output()?;
    assert!(
        output.status.success(),
        "scratch hard-link control failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&scratch_link)?, fs::read(&scratch_source)?);
    let packed_before = fs::read(repo_git.join("packed-refs"))?;
    let tracking_before = ok(git(
        &git_path,
        private,
        &lane,
        &home,
        &["rev-parse", "refs/remotes/origin/main"],
    ));
    fs::write(source.join("upstream"), "fetched blob\n")?;
    ok(git(
        &git_path,
        &source_git,
        &source,
        &home,
        &["add", "upstream"],
    ));
    ok(git(
        &git_path,
        &source_git,
        &source,
        &home,
        &["commit", "--allow-empty", "-m", "advance after launch"],
    ));
    ok(run(&[
        "-c",
        "protocol.file.allow=always",
        "fetch",
        "--refmap=",
        "origin",
        "main",
    ]));
    assert!(ok(run(&["status", "--short", "--branch"])).contains("behind 1"));
    fs::write(lane.join("lane-blob"), "private lane blob\n")?;
    ok(run(&["add", "lane-blob"]));
    ok(run(&["commit", "-m", "lane", "--", "lane-blob"]));
    let lane_blob = ok(run(&["rev-parse", "HEAD:lane-blob"]));
    let lane_blob = lane_blob.trim();
    assert!(
        lane_objects
            .join(&lane_blob[..2])
            .join(&lane_blob[2..])
            .is_file()
    );
    assert!(
        !repo_git
            .join("objects")
            .join(&lane_blob[..2])
            .join(&lane_blob[2..])
            .exists()
    );
    ok(run(&[
        "merge",
        "--no-ff",
        "-m",
        "merge fetched head",
        "FETCH_HEAD",
    ]));
    assert_eq!(fs::read(lane.join("upstream"))?, b"fetched blob\n");
    ok(run(&[
        "-c",
        "protocol.file.allow=always",
        "push",
        "origin",
        "HEAD:refs/heads/lane/test",
    ]));
    assert_eq!(
        ok(git(
            &git_path,
            &push_git,
            &push,
            &home,
            &["rev-parse", "refs/heads/lane/test"]
        )),
        ok(run(&["rev-parse", "HEAD"]))
    );
    assert_eq!(
        ok(git(
            &git_path,
            private,
            &lane,
            &home,
            &["rev-parse", "HEAD"]
        )),
        oid,
        "shared lane branch remains at the base"
    );
    let bypass = run_shared(&["commit", "--allow-empty", "-m", "bypass private refs"]);
    assert!(
        !bypass.status.success()
            && String::from_utf8_lossy(&bypass.stderr).contains("Operation not permitted"),
        "direct Git must not publish the shared ref: {}",
        String::from_utf8_lossy(&bypass.stderr)
    );
    assert!(!run_shared(&["pack-refs", "--all"]).status.success());
    assert_eq!(fs::read(repo_git.join("packed-refs"))?, packed_before);
    assert!(
        !run_shared(&["update-ref", "refs/heads/unrelated", "HEAD"])
            .status
            .success()
    );
    assert!(
        !run_shared(&["config", "test.denied", "yes"])
            .status
            .success()
    );
    assert!(
        !run_shared(&["update-ref", "refs/remotes/upstream/main", "HEAD"])
            .status
            .success()
    );
    for reference in [
        "refs/heads/lane/test",
        "refs/heads/lane/test.lock",
        "logs/refs/heads/lane/test",
        "logs/refs/heads/lane/test.lock",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "refs/remotes/origin/lane/test",
        "logs/refs/remotes/origin/lane/test",
    ] {
        let output = Command::new("/usr/bin/sandbox-exec")
            .args(["-p", sbpl, "/usr/bin/touch"])
            .current_dir(&lane)
            .arg(repo_git.join(reference))
            .output()?;
        assert!(
            !output.status.success(),
            "sandbox permitted writing {reference}"
        );
    }
    for reference in [
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "refs/remotes/origin/lane/test",
    ] {
        assert!(
            !run_shared(&["update-ref", reference, "HEAD"])
                .status
                .success()
        );
    }
    let control_link = scratch.join("link-control");
    let control = Command::new("/usr/bin/sandbox-exec")
        .args(["-p", sbpl, "/bin/ln", "-s"])
        .current_dir(&lane)
        .arg(repo_git.join("config"))
        .arg(&control_link)
        .output()?;
    assert!(
        control.status.success(),
        "link probe could not execute: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    for relative in [
        "refs/remotes/origin/lane/test.lock",
        "logs/refs/remotes/origin/lane/test",
    ] {
        let output = Command::new("/usr/bin/sandbox-exec")
            .args(["-p", sbpl, "/bin/ln", "-s"])
            .current_dir(&lane)
            .arg(repo_git.join("config"))
            .arg(repo_git.join(relative))
            .output()?;
        assert!(
            !output.status.success(),
            "sandbox permitted a link at {relative}"
        );
    }
    assert!(private_common.join("FETCH_HEAD").is_file());
    assert_eq!(
        tracking_before,
        ok(git(
            &git_path,
            private,
            &lane,
            &home,
            &["rev-parse", "refs/remotes/origin/main"]
        ))
    );
    assert_eq!(
        ok(git(
            &git_path,
            &source_git,
            &source,
            &home,
            &["rev-parse", "HEAD"]
        )),
        ok(run(&["rev-parse", "FETCH_HEAD"]))
    );
    // The sole shared write exception must not reopen object content through
    // a symlink at its allowed pathname. Run after Git releases index.lock.
    let index_alias = private.join("index.lock");
    for target in &shared_objects {
        assert!(fs::symlink_metadata(&index_alias).is_err());
        let before = fs::read(target)?;
        let mode = fs::metadata(target)?.permissions().mode();
        let created = shell("/bin/ln -s \"$1\" \"$2\"", target, &index_alias);
        if created.status.success() {
            assert!(created.stdout.is_empty() && created.stderr.is_empty());
            assert!(fs::symlink_metadata(&index_alias)?.file_type().is_symlink());
            assert_eq!(fs::read_link(&index_alias)?, *target);
            for script in [
                "printf replacement > \"$1\"",
                ": > \"$1\"",
                "/bin/chmod 600 \"$1\"",
                "/bin/chmod 600 \"$1\" && printf replacement > \"$1\"",
            ] {
                let output = shell(script, &index_alias, &scratch.join("replacement"));
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    !output.status.success()
                        && output.stdout.is_empty()
                        && !stderr.is_empty()
                        && stderr
                            .lines()
                            .all(|line| line.contains("Operation not permitted")),
                    "shared object index alias bypass for {}: {stderr}",
                    target.display()
                );
                assert_eq!(fs::read(target)?, before);
                assert_eq!(fs::metadata(target)?.permissions().mode(), mode);
            }
            fs::remove_file(&index_alias)?;
        } else {
            let stderr = String::from_utf8_lossy(&created.stderr);
            assert!(
                created.stdout.is_empty()
                    && !stderr.is_empty()
                    && stderr
                        .lines()
                        .all(|line| line.contains("Operation not permitted")),
                "index alias creation failed without a native denial: {stderr}"
            );
            assert!(fs::symlink_metadata(&index_alias).is_err());
        }
        assert_eq!(fs::read(target)?, before);
        assert_eq!(fs::metadata(target)?.permissions().mode(), mode);
    }
    Ok(())
}
