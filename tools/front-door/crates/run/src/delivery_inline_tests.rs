//! Unit tests for [`super::delivery`], moved verbatim from the parent file.

use super::*;

#[cfg(test)]
mod stored_format_tests {
    use super::*;

    #[test]
    fn config_failure_from_a_fact_chain_is_rendered_once() {
        use provenant_confinement::utility::UtilityError;
        use provenant_confinement::worktrees::WorktreeError;

        let cause_text =
            "setting branch configuration failed with exit status 128 (lock contention: false)";
        let error = provenant_os::git_config::ConfigError {
            context: provenant_os::git_config::ConfigContext::new(
                "set branch upstream configuration",
                "/p/.git/config",
                Some("branch tracking key"),
            ),
            source: RunError::DeliveryFact {
                stage: Stage::Upstream,
                source: WorktreeError::Utility(UtilityError::ConfigFailed {
                    status: 128,
                    lock_contention: false,
                }),
            },
        };
        let detail = error.to_string();
        assert!(
            detail.contains(
                "set branch upstream configuration on /p/.git/config for key branch tracking key"
            ),
            "{detail}"
        );
        assert_eq!(detail.matches(cause_text).count(), 1, "{detail}");
        assert!(!detail.contains("delivery failed at"), "{detail}");
    }

    #[test]
    fn transport_config_refusal_names_the_read_once_and_carries_the_failure() {
        let error = provenant_os::git_config::ConfigError {
            context: provenant_os::git_config::ConfigContext::new(
                "read effective Git configuration",
                "/p/.git/config",
                None,
            ),
            source: RunError::ConfigCommandFailed {
                reason: "git \"Exit(128)\" (timed out: false): fatal: bad config line 3".into(),
            },
        };
        let refusal = RunError::DeliveryTransport {
            stage: Stage::Push,
            kind: crate::TransportRefusal::Unreadable,
            reason: transport_config_cause(error),
        };
        let rendered = refusal.to_string();
        assert_eq!(rendered.matches("Push").count(), 1, "{rendered}");
        assert_eq!(
            rendered
                .matches("read effective Git configuration on /p/.git/config")
                .count(),
            1,
            "{rendered}"
        );
        assert_eq!(
            rendered.matches("fatal: bad config line 3").count(),
            1,
            "{rendered}"
        );
    }

    #[test]
    fn a_command_that_exited_zero_keeps_the_ports_parse_error() {
        let ok = Some((Termination::Exit(0), false, "warning".to_owned()));
        let cause = failure_cause(ok, "malformed response");
        assert!(
            matches!(&cause, DeliveryCause::Parse { detail } if detail == "malformed response")
        );
        for failed in [(Termination::Exit(1), false), (Termination::Exit(0), true)] {
            let cause = failure_cause(Some((failed.0, failed.1, "boom".into())), "ignored");
            assert!(matches!(cause, DeliveryCause::Command { .. }), "{failed:?}");
        }
        assert!(matches!(
            failure_cause(None, "no command ran"),
            DeliveryCause::Parse { .. }
        ));
    }

    #[test]
    fn a_long_reason_keeps_its_start_and_its_end() {
        let long = format!(
            "forge step {} fatal: the real failure",
            "noise ".repeat(400)
        );
        let reason = prepare_reason(&long);
        assert!(reason.starts_with("forge step"), "{reason}");
        assert!(reason.ends_with("fatal: the real failure"), "{reason}");
        assert!(reason.len() <= PREPARE_REASON_LIMIT + PREPARE_REASON_TRUNCATION_MARKER.len());
        assert!(reason.contains(PREPARE_REASON_TRUNCATION_MARKER));
    }

    #[test]
    fn transport_refusal_receipt_keeps_the_stage_and_reason() {
        let refusal = RunError::DeliveryTransport {
            stage: Stage::Push,
            kind: crate::TransportRefusal::Unreadable,
            reason: prepare_reason("check delivery transport configuration on /p: leaf marker"),
        };
        let WriterDeliveryState::Failed { stage, reason, .. } = failed_state(&refusal) else {
            panic!("a transport refusal must fail its receipt");
        };
        assert_eq!(stage, Stage::Push);
        assert_eq!(
            reason.as_deref(),
            Some("check delivery transport configuration on /p: leaf marker")
        );
    }

    #[test]
    fn compiled_policy_stays_off_the_guardian_handoff_wire() {
        // Delivery consults the compiled policy on the host, before the context
        // is sealed into the holder handoff. Serialising it would repeat the
        // whole rule table into every launch message and overflow the guardian's
        // holder-context limit, turning a launched lane into an unknown handoff.
        let host = HostDeliveryContext::new(Vec::new())
            .with_policy(provenant_records::policy::Policy::default());
        let encoded = serde_json::to_vec(&host).expect("host delivery encodes");
        assert!(
            !encoded
                .windows(b"\"rules\"".len())
                .any(|window| window == b"\"rules\""),
            "the compiled rule table must not reach the handoff wire"
        );
    }

    #[cfg(unix)]
    #[test]
    fn requester_path_value_and_automatic_lookup_share_filtered_directories() -> std::io::Result<()>
    {
        use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};

        fn executable(path: &Path) -> std::io::Result<()> {
            std::fs::write(path, "#!/bin/sh\nexit 0\n")?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        }

        let temp = tempfile::tempdir()?;
        // The filtered PATH holds canonical directories; a canonical root keeps
        // expected lookups comparable where the temporary directory is a link.
        let root = &std::fs::canonicalize(temp.path())?;
        let project = root.join("project");
        let worktree = root.join("worktree");
        let retained = root.join("retained-bin");
        let captured = root.join("captured-bin");
        let project_bin = project.join("bin");
        let worktree_bin = worktree.join("bin");
        let shared = root.join("shared-tool");
        for directory in [
            &project,
            &worktree,
            &retained,
            &captured,
            &project_bin,
            &worktree_bin,
        ] {
            std::fs::create_dir_all(directory)?;
        }

        executable(&retained.join("which"))?;
        executable(&captured.join("which"))?;
        executable(&captured.join("fallback-tool"))?;
        executable(&shared)?;
        executable(&project_bin.join("project-tool"))?;
        executable(&worktree_bin.join("worktree-tool"))?;
        std::os::unix::fs::symlink(shared, retained.join("safe-alias"))?;
        std::os::unix::fs::symlink(
            project_bin.join("project-tool"),
            retained.join("project-link"),
        )?;
        std::os::unix::fs::symlink(
            worktree_bin.join("worktree-tool"),
            retained.join("worktree-link"),
        )?;

        let mut host = crate::HostLaunchContext::from_environment(
            &root.join("state"),
            &project,
            PathBuf::from("/usr/bin/git"),
        );
        host.executable_search_path = vec![captured.clone()];
        host.lane_path = Some(
            std::env::join_paths([
                retained.as_path(),
                project_bin.as_path(),
                worktree_bin.as_path(),
            ])
            .map_err(std::io::Error::other)?
            .as_os_str()
            .as_bytes()
            .to_vec(),
        );

        let selected = FilteredLanePath::for_requester(Some(&host), &project, &worktree)?;
        let directories = std::env::split_paths(selected.path()).collect::<Vec<_>>();
        assert_eq!(directories, [std::fs::canonicalize(&retained)?]);
        assert_eq!(
            selected.resolve("which").map_err(std::io::Error::other)?,
            retained.join("which")
        );
        assert_eq!(
            selected
                .resolve("safe-alias")
                .map_err(std::io::Error::other)?,
            retained.join("safe-alias")
        );
        assert!(selected.resolve("project-link").is_err());
        assert!(selected.resolve("worktree-link").is_err());
        assert!(selected.resolve("./which").is_err());

        host.lane_path = None;
        let captured_path = FilteredLanePath::for_requester(Some(&host), &project, &worktree)?;
        assert_eq!(
            captured_path
                .resolve("which")
                .map_err(std::io::Error::other)?,
            captured.join("which")
        );

        host.lane_path = Some(
            std::env::join_paths([project_bin.as_path(), worktree_bin.as_path()])
                .map_err(std::io::Error::other)?
                .as_os_str()
                .as_bytes()
                .to_vec(),
        );
        let empty = FilteredLanePath::for_requester(Some(&host), &project, &worktree)?;
        assert!(empty.path().is_empty());
        assert!(empty.resolve("fallback-tool").is_err());

        let process_path = std::env::var_os("PATH").unwrap_or_default();
        let without_host = FilteredLanePath::for_requester(None, &project, &worktree)?;
        let expected = FilteredLanePath::from_path(&process_path, &project, &worktree)?;
        assert_eq!(without_host.path(), expected.path());
        Ok(())
    }

    #[derive(Clone, Copy)]
    enum StoredDeliveryShape {
        LaunchBinding,
        PickupReceipt,
        OriginRefreshRequest,
    }

    const STORED_DELIVERY_SHAPES: [StoredDeliveryShape; 3] = [
        StoredDeliveryShape::LaunchBinding,
        StoredDeliveryShape::PickupReceipt,
        StoredDeliveryShape::OriginRefreshRequest,
    ];

    fn preserves_unknown<T>(
        record: T,
        max_bytes: u64,
    ) -> std::result::Result<(), Box<dyn std::error::Error>>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let future = serde_json::json!({"preserved": true});
        let mut value = serde_json::to_value(record)?;
        value
            .as_object_mut()
            .ok_or("stored record must be an object")?
            .insert("future_record_field".to_owned(), future.clone());
        let incoming = serde_json::to_vec(&value)?;
        let limits = provenant_records::Limits {
            max_bytes,
            ..Default::default()
        };
        let decoded: T = provenant_records::decode(incoming.as_slice(), limits)?;
        let rewritten = provenant_records::CanonicalV1::bytes(&decoded)?;
        let _: T = provenant_records::decode(rewritten.as_bytes(), limits)?;
        let shape: serde_json::Value = provenant_records::decode(rewritten.as_bytes(), limits)?;
        assert_eq!(shape.get("future_record_field"), Some(&future));
        Ok(())
    }

    #[test]
    fn persisted_delivery_shapes_preserve_unknown_fields_through_bounded_decode()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let request = WriterDeliveryRequest {
            extra: Default::default(),
            mode: crate::DeliveryMode::Push,
            remote: provenant_records::run::Destination::from_name("origin")
                .expect("fixture remote"),
            branch: "feature".into(),
            open_pr: false,
            fast_forward_only: false,
        };
        let bound = BoundDelivery {
            base_head: GitOid::parse(&"a".repeat(40))?,
            push_url: "file:///remote.git".into(),
            lane_branch: "lane".into(),
            pull_request_repo: None,
            git_dir: PathBuf::from("/repo/.git"),
            common_git_dir: PathBuf::from("/repo/.git"),
            host_cwd: PathBuf::from("/repo"),
            destination: request.clone(),
        };
        let scope = provenant_records::custody::Scope {
            attempt: provenant_core::AttemptId::from_bytes([1; 16]),
            generation: 1,
        };
        let receipt = WriterDeliveryReceipt {
            extra: Default::default(),
            schema_version: 1,
            remote: provenant_records::run::Destination::from_name("origin")
                .expect("fixture remote"),
            branch: "feature".into(),
            head: None,
            review_acceptance: None,
            state: WriterDeliveryState::NoNewCommits {
                extra: Default::default(),
            },
            integration: None,
        };
        for shape in STORED_DELIVERY_SHAPES {
            match shape {
                StoredDeliveryShape::LaunchBinding => {
                    preserves_unknown(LaunchBinding::new(scope, &bound), crate::RECORD_LIMIT)?;
                }
                StoredDeliveryShape::PickupReceipt => {
                    preserves_unknown(
                        PickupReceipt {
                            extra: Default::default(),
                            schema_version: 1,
                            request: request.clone(),
                            receipt: receipt.clone(),
                        },
                        crate::RECORD_LIMIT,
                    )?;
                }
                StoredDeliveryShape::OriginRefreshRequest => {
                    preserves_unknown(
                        OriginRefreshRequest {
                            extra: Default::default(),
                            boot: "fixture-boot".into(),
                            micros: 1,
                        },
                        ORIGIN_REFRESH_LIMIT,
                    )?;
                }
            }
        }
        let temp = tempfile::tempdir()?;
        let attempt = Directory::open_anchor(temp.path())?;
        let future = serde_json::json!({"preserved": true});
        let mut extra = provenant_records::ExtraFields::new();
        extra.insert("future_record_field".into(), future.clone());
        let prior = PickupReceipt {
            extra,
            schema_version: 1,
            request: request.clone(),
            receipt: receipt.clone(),
        };
        attempt
            .atomic_write(
                OsStr::new("delivery-pickup.json"),
                ".fixture-",
                &crate::db::bytes(&prior)?,
                provenant_os::fs::PublishMode::Replace,
            )?
            .require_durable()?;
        publish_pickup(&attempt, &request, &receipt)?;
        let raw = attempt.read_stable(OsStr::new("delivery-pickup.json"), crate::RECORD_LIMIT)?;
        let updated: PickupReceipt = provenant_records::decode(
            raw.as_slice(),
            provenant_records::Limits {
                max_bytes: crate::RECORD_LIMIT,
                ..Default::default()
            },
        )?;
        let rewritten = provenant_records::encode(&updated)?;
        let shape: serde_json::Value = provenant_records::decode(
            rewritten.as_slice(),
            provenant_records::Limits {
                max_bytes: crate::RECORD_LIMIT,
                ..Default::default()
            },
        )?;
        assert_eq!(shape.get("future_record_field"), Some(&future));
        Ok(())
    }
}

#[cfg(test)]
mod forge_context_tests {
    use super::*;

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn delivery_destination_and_protection_follow_the_upstream_context() {
        let forge = forge_for("upstream", "trunk", &[]);
        let push = HostDeliveryContext::destination(crate::DeliveryMode::Push, "lane", &forge)
            .expect("destination")
            .expect("push request");
        assert_eq!(
            push.remote,
            provenant_records::run::Destination::from_name("upstream").expect("fixture remote")
        );
        assert_eq!(push.branch, "lane");
        assert!(forge.is_protected("trunk"));
        assert!(!forge.is_protected("lane"));
        let integrate = HostDeliveryContext::destination(
            crate::DeliveryMode::Integrate("upstream/trunk".into()),
            "lane",
            &forge,
        )
        .expect("destination")
        .expect("integrate request");
        assert_eq!(
            integrate.remote,
            provenant_records::run::Destination::from_name("upstream").expect("fixture remote")
        );
        assert_eq!(integrate.branch, "trunk");
    }
}
