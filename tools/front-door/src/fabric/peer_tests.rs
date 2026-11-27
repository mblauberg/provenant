#[cfg(test)]
#[allow(unused_imports)] // The custom worktree driver compiles without a test harness.
#[path = "peer_output_projection_tests.rs"]
mod output_projection_tests;

#[cfg(test)]
mod host_aggregate_tests {
    use super::*;
    use provenant_records::Outcome;
    use serde_json::json;

    #[test]
    fn host_all_aggregates_and_marks_unreachable_partial() {
        let result = aggregate_host_readings(
            "status",
            vec![
                ("workshop".into(), HostResult::Ok(json!({"count": 3}))),
                ("laptop".into(), HostResult::Unreachable),
            ],
        );
        assert_eq!(result.status, Outcome::Unavailable);
        assert!(result.data.partial);
        assert_eq!(result.data.hosts.len(), 2);
        assert_eq!(result.data.hosts[0].host, "workshop");
        assert_eq!(result.data.hosts[1].status, Outcome::Unavailable);
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].code.as_ref(), PEER_UNREACHABLE.as_str());
        assert_eq!(
            host_sections(&result.data),
            "workshop: Ok count=3\nlaptop: Unavailable PEER_UNREACHABLE"
        );
    }

    #[test]
    fn all_hosts_unreachable_is_unavailable_without_partial() {
        let result = aggregate_host_readings(
            "gc",
            vec![
                ("workshop".into(), HostResult::Unreachable),
                ("laptop".into(), HostResult::Unreachable),
            ],
        );
        assert_eq!(result.status, Outcome::Unavailable);
        assert!(!result.data.partial);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn unreachable_status_is_worst_even_when_a_reachable_host_failed() {
        let result = aggregate_host_readings(
            "gc",
            vec![
                ("workshop".into(), HostResult::Ok(json!({"ok": true}))),
                ("laptop".into(), HostResult::Failed("no space".into())),
                ("desk".into(), HostResult::Unreachable),
            ],
        );
        assert_eq!(result.status, Outcome::Unavailable);
        assert!(result.data.partial);
        assert_eq!(result.warnings.len(), 1);
    }

    #[test]
    fn transport_failures_are_unreachable_but_refusals_are_host_errors() {
        assert!(matches!(
            host_result_from_peer(Err::<serde_json::Value, _>(
                provenant_cli::fabric::PeerFailure {
                    error: Box::new(provenant_fabric_runtime::coordination::facade::FacadeError::HomeUnavailable),
                    detail: Some("peer is unavailable".into()),
                }
            )),
            HostResult::Unreachable
        ));
        assert!(matches!(
            host_result_from_peer(Err::<serde_json::Value, _>(
                provenant_cli::fabric::PeerFailure {
                    error: Box::new(provenant_fabric_runtime::coordination::facade::FacadeError::DispatchRefused {
                        reason: "refused".into(), disk: None, fact: None,
                    }),
                    detail: None,
                }
            )),
            HostResult::Command {
                status: Outcome::Refused,
                ..
            }
        ));
        assert!(matches!(
            host_result_from_peer(Ok(json!({"ok": true}))),
            HostResult::Ok(_)
        ));
    }
}

#[cfg(test)]
#[allow(unused_imports)] // The custom worktree driver compiles without a test harness.
#[path = "peer_console_tree_tests.rs"]
mod console_tree_tests;

#[cfg(test)]
mod peer_project_root_tests {

    #[test]
    fn peer_refusal_precedes_binding_writes_and_holds_bare_common_metadata()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::*;
        use provenant_core::RootInputs;

        fn git(cwd: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
            provenant_os::fixture::run_git(cwd, args)?;
            Ok(())
        }

        // Spec: docs/specs/run-git.md#project-layout-ownership
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        let assets = base.join("assets");
        let source = base.join("source");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&assets)?;
        std::fs::create_dir_all(&source)?;
        git(&source, &["init", "-q", "--initial-branch=main"])?;
        git(&source, &["config", "user.name", "Fixture"])?;
        git(
            &source,
            &["config", "user.email", "fixture@example.invalid"],
        )?;
        std::fs::write(source.join("README.md"), "bare root boundary fixture\n")?;
        git(&source, &["add", "README.md"])?;
        git(&source, &["commit", "-qm", "initial"])?;

        for name in ["bare-store", ".git"] {
            let parent = base.join(name.replace('.', "dot"));
            std::fs::create_dir_all(&parent)?;
            let common = parent.join(name);
            git(
                &base,
                &[
                    "clone",
                    "--bare",
                    "-q",
                    source.to_str().ok_or("source path")?,
                    common.to_str().ok_or("common path")?,
                ],
            )?;
            let state = common.join("state");
            std::fs::create_dir_all(&state)?;
            std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
            let mut inputs = RootInputs::new(home.clone(), assets.clone());
            inputs.state_dir = Some(state.clone());
            inputs.workspaces.push(home.clone());
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let globals = provenant_cli::Globals {
                state_dir: Some(state.clone()),
                ..Default::default()
            };

            assert!(!state.join("projects").exists());
            assert!(!state.join("host-id").exists());
            let refusal = prepare_peer_project_roots(
                &globals,
                roots,
                &common,
                false,
                crate::roots::Product::Optional,
            );
            assert!(refusal.is_err(), "state under {common:?} was admitted");
            assert!(!state.join("projects").exists());
            assert!(!state.join("host-id").exists());
        }

        let plain = base.join("plain-project");
        std::fs::create_dir_all(&plain)?;
        git(&plain, &["init", "-q", "--initial-branch=main"])?;
        let common = plain.join(".git");
        let state = plain.join("state");
        std::fs::create_dir_all(&state)?;
        std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        let mut inputs = RootInputs::new(home, assets);
        inputs.state_dir = Some(state.clone());
        inputs.workspaces.push(base.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let globals = provenant_cli::Globals {
            state_dir: Some(state.clone()),
            ..Default::default()
        };
        let (roots, binding) = prepare_peer_project_roots(
            &globals,
            roots,
            &common,
            false,
            crate::roots::Product::Optional,
        )?;
        assert_eq!(binding.common_dir(), common);
        assert!(roots.state.as_path().join("projects").is_dir());
        assert!(roots.state.as_path().join("host-id").is_file());
        Ok(())
    }
}
