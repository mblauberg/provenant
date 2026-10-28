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
fn linked_lane_can_fetch_and_commit_with_packed_refs_read_only() -> Result<(), Box<dyn Error>> {
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
    fs::create_dir_all(repo_git.join("refs/remotes/upstream"))?;
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
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            allow_process_fork: true,
            runtime_reads: vec![grant(exec_path.clone())],
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
        scratch: grant(scratch),
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
    let mut paths = required_anchors(&plan, &profile, Some((&linked, &selected)));
    paths.insert(push);
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
    let run = |args: &[&str]| {
        Command::new("/usr/bin/sandbox-exec")
            .args(["-p", sbpl])
            .arg(&git_path)
            .args(args)
            .env("GIT_DIR", private)
            .env("GIT_WORK_TREE", &lane)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("run sandboxed Git")
    };
    let packed_before = fs::read(repo_git.join("packed-refs"))?;
    ok(run(&[
        "-c",
        "protocol.file.allow=always",
        "fetch",
        "origin",
    ]));
    assert!(ok(run(&["status", "--short", "--branch"])).contains("behind 1"));
    ok(run(&["commit", "--allow-empty", "-m", "lane"]));
    ok(run(&[
        "-c",
        "protocol.file.allow=always",
        "push",
        "origin",
        "HEAD:refs/heads/lane/test",
    ]));
    assert!(!run(&["pack-refs", "--all"]).status.success());
    assert_eq!(fs::read(repo_git.join("packed-refs"))?, packed_before);
    assert!(
        !run(&["update-ref", "refs/heads/unrelated", "HEAD"])
            .status
            .success()
    );
    assert!(!run(&["config", "test.denied", "yes"]).status.success());
    assert!(
        !run(&["update-ref", "refs/remotes/upstream/main", "HEAD"])
            .status
            .success()
    );
    assert!(private.join("FETCH_HEAD").is_file());
    assert_eq!(
        ok(git(
            &git_path,
            &source_git,
            &source,
            &home,
            &["rev-parse", "HEAD"]
        )),
        ok(git(
            &git_path,
            private,
            &lane,
            &home,
            &["rev-parse", "refs/remotes/origin/main"]
        ))
    );
    Ok(())
}
