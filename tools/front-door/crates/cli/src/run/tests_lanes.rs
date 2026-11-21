use super::*;

#[test]
fn lane_access_defers_credential_root_validation_to_the_resolved_route()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("host-home");
    let credential = home.join(".ssh").join("nested");
    std::fs::create_dir_all(&credential)?;
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("gh"))?;
    let roots = [credential.clone(), home.clone(), config];
    for root in roots {
        for read in [true, false] {
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
            let granted =
                lane_access(&access, &temp.path().join("state")).map_err(|fail| fail.message)?;
            let canonical = std::fs::canonicalize(&root)?;
            assert_eq!(
                if read {
                    granted.read_roots
                } else {
                    granted.write_roots
                },
                vec![canonical]
            );
        }
    }
    Ok(())
}
