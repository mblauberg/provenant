use super::*;

#[test]
fn lane_access_refuses_credential_and_enclosing_roots() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let credential = temp.path().join(".ssh").join("nested");
    std::fs::create_dir_all(&credential)?;
    let mut roots = vec![std::fs::canonicalize(&credential)?];
    // The home itself and its `.config` are enclosing roots; use the host's
    // real directories so the check sees a conventional home.
    let host_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_dir());
    if let Some(home) = &host_home {
        if home.join(".config").is_dir() {
            roots.push(home.join(".config"));
        }
        roots.push(home.clone());
    }
    assert!(roots.len() >= 2);
    let home = host_home
        .as_deref()
        .unwrap_or(Path::new("/fixture/host-home"));
    for root in roots {
        for (flag, read) in [("--read-root", true), ("--write-root", false)] {
            let access = crate::Access {
                attach: Vec::new(),
                mode: Some(Mode::ReadOnly),
                worktree: None,
                read_root: if read { vec![root.clone()] } else { vec![] },
                write_root: if read { vec![] } else { vec![root.clone()] },
                network: None,
                capability: vec![],
                no_capability: Vec::new(),
            };
            let fail = lane_access(&access, &temp.path().join("state"), home).unwrap_err();
            assert!(fail.message.contains(flag), "{}", fail.message);
            assert!(
                fail.message.contains(&root.display().to_string()),
                "{}",
                fail.message
            );
        }
    }
    Ok(())
}
