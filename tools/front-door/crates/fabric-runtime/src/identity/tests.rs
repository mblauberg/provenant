use super::*;

fn git(cwd: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .into()
}
fn repository(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q"]);
    // commit-tree needs no persistent account configuration or personal details.
    let tree = git(path, &["mktree"]);
    let result = Command::new("git")
        .current_dir(path)
        .args(["commit-tree", &tree, "-m", "fixture"])
        .envs([
            ("GIT_AUTHOR_NAME", "Fixture"),
            ("GIT_COMMITTER_NAME", "Fixture"),
            ("GIT_AUTHOR_EMAIL", ""),
            ("GIT_COMMITTER_EMAIL", ""),
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let sha = String::from_utf8(result.stdout).unwrap();
    git(path, &["update-ref", "HEAD", sha.trim()]);
}

#[test]
fn registered_worktrees_share_project_but_keep_cwd_and_copied_metadata_is_isolated() {
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    let primary = root.path().join("primary\nline");
    repository(&primary);
    let linked = root.path().join("linked\r");
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    let env = Environment::from([
        ("AGENT_FABRIC_SEAT".into(), "fixture".into()),
        ("AGENT_FABRIC_LABEL".into(), "worker".into()),
    ]);
    let worker = identify(&env, &linked).unwrap();
    assert_eq!(worker.project, fs::canonicalize(&primary).unwrap());
    assert_eq!(worker.cwd, fs::canonicalize(&linked).unwrap());
    assert_eq!(worker.agent_id, "worker");
    assert_eq!(run_root(&linked, &env).unwrap(), primary.join(".agent-run"));
    let nested = linked.join("nested/source");
    fs::create_dir_all(&nested).unwrap();
    assert_eq!(project_root(&nested, &env).unwrap(), primary);
    let copied = root.path().join("copied");
    fs::create_dir(&copied).unwrap();
    fs::copy(linked.join(".git"), copied.join(".git")).unwrap();
    assert_eq!(project_root(&copied, &env).unwrap(), copied);
    let stale = root.path().join("stale");
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            stale.to_str().unwrap(),
            "HEAD",
        ],
    );
    fs::copy(linked.join(".git"), stale.join(".git")).unwrap();
    assert_eq!(project_root(&stale, &env).unwrap(), stale);
}

#[test]
fn separate_metadata_layouts_are_not_aliased() {
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    let primary = root.path().join("primary");
    repository(&primary);
    let metadata = root.path().join("metadata");
    git(
        &primary,
        &[
            "init",
            "-q",
            "--separate-git-dir",
            metadata.to_str().unwrap(),
        ],
    );
    let linked = root.path().join("linked");
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    assert_eq!(
        project_root(&primary, &Environment::new()).unwrap(),
        primary
    );
    assert_eq!(project_root(&linked, &Environment::new()).unwrap(), linked);
}

#[test]
fn directories_symlinks_and_git_redirects() {
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    let primary = root.path().join("primary");
    repository(&primary);
    let other = root.path().join("other");
    repository(&other);
    let env = Environment::from([
        ("GIT_DIR".into(), other.join(".git").into()),
        ("GIT_WORK_TREE".into(), other.into_os_string()),
        ("GIT_CONFIG_KEY_0".into(), "bad".into()),
        ("GIT_CONFIG_VALUE_0".into(), "bad".into()),
        ("GIT_CONFIG_KEY_x".into(), "keep".into()),
    ]);
    assert_eq!(project_root(&primary, &env).unwrap(), primary);
    let clean = without_git_redirects(&env);
    assert_eq!(clean.len(), 1);
    assert!(clean.contains_key(OsStr::new("GIT_CONFIG_KEY_x")));
    assert_eq!(project_root(root.path(), &env).unwrap(), root.path());
    #[cfg(unix)]
    {
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&primary, &alias).unwrap();
        assert_eq!(identify(&Environment::new(), &alias).unwrap().cwd, primary);
        assert_eq!(
            run_root(&alias, &Environment::new()).unwrap(),
            primary.join(".agent-run")
        );
    }
}

#[test]
fn identity_defaults_and_state_path() {
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    let identity = identify(&Environment::new(), root.path()).unwrap();
    assert_eq!(identity.agent_id, "agent");
    assert_eq!(identity.provider, "agent");
    assert!(serde_json::to_value(identity)
        .unwrap()
        .get("registeredProjects")
        .is_none());
    assert_eq!(
        database_path(&Environment::new(), root.path()).unwrap(),
        root.path()
            .join(".local/state/agent-harness/fabric/fabric.sqlite3")
    );
    let env = Environment::from([
        ("AGENT_FABRIC_SEAT".into(), "".into()),
        ("AGENT_FABRIC_CLIENT_LABEL".into(), "ignored".into()),
    ]);
    assert_eq!(identify(&env, root.path()).unwrap().provider, "");
}

#[test]
fn sha_prefixes_resolve_objects_not_hex_refs() {
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    repository(root.path());
    let sha = git(root.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        resolve_commit_sha(root.path(), &sha[..7].to_uppercase()).unwrap(),
        sha
    );
    assert_eq!(
        resolve_commit_sha(root.path(), &format!(" \u{feff}{sha}\n")).unwrap(),
        sha
    );
    let full = "a".repeat(40);
    assert_eq!(resolve_commit_sha(root.path(), &full).unwrap(), full);
    git(root.path(), &["branch", "deadbee"]);
    assert_eq!(
        resolve_commit_sha(root.path(), "deadbee")
            .unwrap_err()
            .code(),
        "expected_sha_unresolved"
    );
    assert_eq!(
        resolve_commit_sha(root.path(), "abc123")
            .unwrap_err()
            .code(),
        "expected_sha_invalid"
    );
    assert_eq!(
        resolve_commit_sha(root.path(), "HEAD").unwrap_err().code(),
        "expected_sha_invalid"
    );
    let tree = git(root.path(), &["rev-parse", "HEAD^{tree}"]);
    assert!(resolve_commit_sha(root.path(), &tree[..7]).is_err());
}

#[test]
fn ambiguous_object_prefix_is_refused() {
    use std::io::Write;
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    repository(root.path());
    // Fixed SHA-1 object collision at seven digits, not a probabilistic search.
    for body in [b"fixture-9954".as_slice(), b"fixture-11721"] {
        let mut child = Command::new("git")
            .current_dir(root.path())
            .args(["hash-object", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(body).unwrap();
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(
        resolve_commit_sha(root.path(), "07aefac")
            .unwrap_err()
            .code(),
        "expected_sha_unresolved"
    );
}

#[test]
fn submodule_linked_metadata_remains_separate() {
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    let parent = root.path().join("parent");
    repository(&parent);
    let child = root.path().join("child");
    repository(&child);
    git(
        &parent,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            child.to_str().unwrap(),
            "vendor/child",
        ],
    );
    let submodule = parent.join("vendor/child");
    let linked = root.path().join("linked-submodule");
    git(
        &submodule,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    assert_eq!(
        project_root(&submodule, &Environment::new()).unwrap(),
        submodule
    );
    assert_ne!(
        project_root(&linked, &Environment::new()).unwrap(),
        submodule
    );
}

#[test]
fn sha_trim_is_ecmascript_whitespace() {
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    let full = "a".repeat(40);
    assert!(resolve_commit_sha(root.path(), &format!("\u{0085}{full}\u{0085}")).is_err());
    assert_eq!(
        resolve_commit_sha(root.path(), &format!("\u{feff}{full}\u{feff}")).unwrap(),
        full
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_missing_paths_stay_os_strings() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    let path = root
        .path()
        .join(OsString::from_vec(b"project-\xff".to_vec()));
    // A missing name exercises the lexical fallback without depending on
    // whether the host sandbox permits creating a non-UTF-8 filename.
    assert_eq!(identify(&Environment::new(), &path).unwrap().cwd, path);
    assert_eq!(project_root(&path, &Environment::new()).unwrap(), path);
}

#[test]
fn differential_against_typescript_identity_when_interpreter_is_available() {
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    let primary = root.path().join("primary");
    repository(&primary);
    let linked = root.path().join("linked");
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    let copied = root.path().join("copied");
    fs::create_dir(&copied).unwrap();
    fs::copy(linked.join(".git"), copied.join(".git")).unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../runtime/fabric/src/identity.ts");
    let mut paths = vec![
        primary.clone(),
        linked.clone(),
        copied.clone(),
        root.path().to_path_buf(),
    ];
    #[cfg(unix)]
    {
        let lexical = root.path().join("lexical");
        let target = root.path().join("target");
        for path in [lexical.join("x"), target.join("child"), target.join("x")] {
            fs::create_dir_all(path).unwrap();
        }
        std::os::unix::fs::symlink(target.join("child"), lexical.join("link")).unwrap();
        paths.push(lexical.join("link/../x"));
    }
    let script="const m=await import(requireUrl(process.argv[1])); function requireUrl(p){return new URL('file://'+p).href}; const env={AGENT_FABRIC_SEAT:'fixture',AGENT_FABRIC_LABEL:'worker'}; process.stdout.write(JSON.stringify(process.argv.slice(2).map(p=>({identity:m.identify(env,p),run:m.runRoot(p,env),project:m.projectRoot(p,env)}))))";
    let mut args = vec![source.to_string_lossy().into_owned()];
    args.extend(paths.iter().map(|path| path.to_string_lossy().into_owned()));
    let Some(expected) = crate::test_support::node(script, &args) else {
        return;
    };
    let env = Environment::from([
        ("AGENT_FABRIC_SEAT".into(), "fixture".into()),
        ("AGENT_FABRIC_LABEL".into(), "worker".into()),
    ]);
    let actual:Vec<_>=paths.iter().map(|path| {
        serde_json::json!({"identity":identify(&env,path).unwrap(),"run":run_root(path,&env).unwrap(),"project":project_root(path,&env).unwrap()})
    }).collect();
    assert_eq!(serde_json::json!(actual), expected);
}

#[test]
fn seat_client_and_agent_labels_follow_nullish_precedence() {
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    for (seat, client, label, provider, agent) in [
        (Some("seat"), Some("client"), None, "seat", "seat"),
        (None, Some("client"), None, "client", "client"),
        (
            Some("seat"),
            Some("client"),
            Some("worker"),
            "seat",
            "worker",
        ),
        (None, None, Some(""), "agent", ""),
    ] {
        let mut env = Environment::new();
        for (key, value) in [
            ("AGENT_FABRIC_SEAT", seat),
            ("AGENT_FABRIC_CLIENT_LABEL", client),
            ("AGENT_FABRIC_LABEL", label),
        ] {
            if let Some(value) = value {
                env.insert(key.into(), value.into());
            }
        }
        let identity = identify(&env, root.path()).unwrap();
        assert_eq!(identity.provider, provider);
        assert_eq!(identity.agent_id, agent);
    }
}

#[cfg(unix)]
#[test]
fn project_resolves_dotdot_before_realpath_but_identity_and_run_use_realpath_first() {
    let root = tempfile::tempdir_in(fs::canonicalize("/tmp").unwrap()).unwrap();
    let lexical = root.path().join("lexical");
    let target = root.path().join("target");
    for path in [lexical.join("x"), target.join("child"), target.join("x")] {
        fs::create_dir_all(path).unwrap();
    }
    std::os::unix::fs::symlink(target.join("child"), lexical.join("link")).unwrap();
    let path = lexical.join("link/../x");
    let env = Environment::new();
    assert_eq!(project_root(&path, &env).unwrap(), lexical.join("x"));
    assert_eq!(identify(&env, &path).unwrap().cwd, target.join("x"));
    assert_eq!(run_root(&path, &env).unwrap(), target.join("x/.agent-run"));
}
