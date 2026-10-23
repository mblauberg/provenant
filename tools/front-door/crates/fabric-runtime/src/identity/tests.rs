use super::*;

fn git(cwd:&Path,args:&[&str])->String {
    let result=Command::new("git").current_dir(cwd).args(args).output().unwrap();
    assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stderr));
    String::from_utf8(result.stdout).unwrap().trim_end_matches('\n').into()
}
fn repository(path:&Path) {
    fs::create_dir_all(path).unwrap(); git(path,&["init","-q"]);
    // commit-tree needs no persistent account configuration or personal details.
    let tree=git(path,&["mktree"]);
    let result=Command::new("git").current_dir(path).args(["commit-tree",&tree,"-m","fixture"])
        .envs([("GIT_AUTHOR_NAME","Fixture"),("GIT_COMMITTER_NAME","Fixture"),("GIT_AUTHOR_EMAIL",""),("GIT_COMMITTER_EMAIL","")]).output().unwrap();
    assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stderr));
    let sha=String::from_utf8(result.stdout).unwrap(); git(path,&["update-ref","HEAD",sha.trim()]);
}

#[test]
fn registered_worktrees_share_project_but_keep_cwd_and_copied_metadata_is_isolated() {
    let root=tempfile::tempdir().unwrap();
    let primary=root.path().join("primary\nline"); repository(&primary);
    let linked=root.path().join("linked\r");
    git(&primary,&["worktree","add","-q","--detach",linked.to_str().unwrap(),"HEAD"]);
    let env=Environment::from([("AGENT_FABRIC_SEAT".into(),"fixture".into()),("AGENT_FABRIC_LABEL".into(),"worker".into())]);
    let worker=identify(&env,&linked).unwrap();
    assert_eq!(worker.project,fs::canonicalize(&primary).unwrap());
    assert_eq!(worker.cwd,fs::canonicalize(&linked).unwrap());
    assert_eq!(worker.agent_id,"worker");
    assert_eq!(run_root(&linked,&env).unwrap(),primary.join(".agent-run"));
    let nested=linked.join("nested/source"); fs::create_dir_all(&nested).unwrap();
    assert_eq!(project_root(&nested,&env).unwrap(),primary);
    let copied=root.path().join("copied"); fs::create_dir(&copied).unwrap();
    fs::copy(linked.join(".git"),copied.join(".git")).unwrap();
    assert_eq!(project_root(&copied,&env).unwrap(),copied);
    let stale=root.path().join("stale");
    git(&primary,&["worktree","add","-q","--detach",stale.to_str().unwrap(),"HEAD"]);
    fs::copy(linked.join(".git"),stale.join(".git")).unwrap();
    assert_eq!(project_root(&stale,&env).unwrap(),stale);
}

#[test]
fn separate_metadata_and_submodule_layouts_are_not_aliased() {
    let root=tempfile::tempdir().unwrap(); let primary=root.path().join("primary"); repository(&primary);
    let metadata=root.path().join("metadata");
    git(&primary,&["init","-q","--separate-git-dir",metadata.to_str().unwrap()]);
    let linked=root.path().join("linked");
    git(&primary,&["worktree","add","-q","--detach",linked.to_str().unwrap(),"HEAD"]);
    assert_eq!(project_root(&primary,&Environment::new()).unwrap(),primary);
    assert_eq!(project_root(&linked,&Environment::new()).unwrap(),linked);
}

#[test]
fn directories_symlinks_and_git_redirects() {
    let root=tempfile::tempdir().unwrap(); let primary=root.path().join("primary"); repository(&primary);
    let other=root.path().join("other"); repository(&other);
    let env=Environment::from([("GIT_DIR".into(),other.join(".git").into()),("GIT_WORK_TREE".into(),other.into_os_string()),("GIT_CONFIG_KEY_0".into(),"bad".into()),("GIT_CONFIG_VALUE_0".into(),"bad".into()),("GIT_CONFIG_KEY_x".into(),"keep".into())]);
    assert_eq!(project_root(&primary,&env).unwrap(),primary);
    let clean=without_git_redirects(&env);
    assert_eq!(clean.len(),1); assert!(clean.contains_key(OsStr::new("GIT_CONFIG_KEY_x")));
    assert_eq!(project_root(root.path(),&env).unwrap(),root.path());
    #[cfg(unix)] {
        let alias=root.path().join("alias"); std::os::unix::fs::symlink(&primary,&alias).unwrap();
        assert_eq!(identify(&Environment::new(),&alias).unwrap().cwd,primary);
        assert_eq!(run_root(&alias,&Environment::new()).unwrap(),primary.join(".agent-run"));
    }
}

#[test]
fn identity_defaults_and_state_path() {
    let root=tempfile::tempdir().unwrap();
    let identity=identify(&Environment::new(),root.path()).unwrap();
    assert_eq!(identity.agent_id,"agent"); assert_eq!(identity.provider,"agent");
    assert!(serde_json::to_value(identity).unwrap().get("registeredProjects").is_none());
    assert_eq!(database_path(&Environment::new(),root.path()).unwrap(),root.path().join(".local/state/agent-harness/fabric/fabric.sqlite3"));
    let env=Environment::from([("AGENT_FABRIC_SEAT".into(),"".into()),("AGENT_FABRIC_CLIENT_LABEL".into(),"ignored".into())]);
    assert_eq!(identify(&env,root.path()).unwrap().provider,"");
}

#[test]
fn sha_prefixes_resolve_objects_not_hex_refs() {
    let root=tempfile::tempdir().unwrap(); repository(root.path());
    let sha=git(root.path(),&["rev-parse","HEAD"]);
    assert_eq!(resolve_commit_sha(root.path(),&sha[..7].to_uppercase()).unwrap(),sha);
    assert_eq!(resolve_commit_sha(root.path(),&format!(" \u{feff}{sha}\n")).unwrap(),sha);
    let full="a".repeat(40); assert_eq!(resolve_commit_sha(root.path(),&full).unwrap(),full);
    git(root.path(),&["branch","deadbee"]);
    assert_eq!(resolve_commit_sha(root.path(),"deadbee").unwrap_err().code(),"expected_sha_unresolved");
    assert_eq!(resolve_commit_sha(root.path(),"abc123").unwrap_err().code(),"expected_sha_invalid");
    assert_eq!(resolve_commit_sha(root.path(),"HEAD").unwrap_err().code(),"expected_sha_invalid");
    let tree=git(root.path(),&["rev-parse","HEAD^{tree}"]);
    assert!(resolve_commit_sha(root.path(),&tree[..7]).is_err());
}
