use super::*;

#[test]
fn lane_access_refuses_credential_and_enclosing_roots() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("host-home");
    let credential = home.join(".ssh").join("nested");
    std::fs::create_dir_all(&credential)?;
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("gh"))?;
    let roots = [credential.clone(), home.clone(), config];
    let credentials = provenant_core::CredentialRoots::for_home(&home);
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
            let fail = lane_access(&access, &temp.path().join("state"), &credentials).unwrap_err();
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
