#[allow(
    unused_imports,
    reason = "the custom worktree driver includes this module without libtest"
)]
use super::*;

#[test]
fn recovery_rebuild_preserves_request_digest_for_any_submitted_spec()
-> Result<(), Box<dyn std::error::Error>> {
    for parent_sandboxed in [false, true] {
        for confidential in [false, true] {
            for build_heavy in [false, true] {
                for account in [None, Some("special-account".to_owned())] {
                    for provider_timeout in [None, Some(1800)] {
                        for admitted_by in [None, Some("parent-admitter".to_owned())] {
                            let spec = provenant_fabric_runtime::graph::AttemptSpec {
                                account: account.clone(),
                                build_heavy,
                                confidential,
                                parent_sandboxed,
                                admitted_by: admitted_by.clone(),
                                provider_timeout,
                                launch_policy: Some(provenant_records::run::LaunchPolicy {
                                    confidential,
                                    sources: vec!["repository declaration".into()],
                                    repository_config: Some(
                                        provenant_config::REPOSITORY_CONFIG.into(),
                                    ),
                                }),
                                commit: build_heavy.then_some(
                                    provenant_records::lane_contract::CommitChoice::OwnPaths,
                                ),
                                carry_uncommitted: confidential
                                    .then_some(provenant_records::lane_contract::CarryMode::All),
                                ..Default::default()
                            };

                            let mut profile = provenant_records::run::HarnessProfile::default();
                            profile
                                .knobs
                                .insert("lanes.sandbox_floor".into(), "strict".into());
                            profile.knob_sources.insert(
                                "lanes.sandbox_floor".into(),
                                provenant_records::Layer::Host,
                            );
                            if spec.confidential {
                                profile.confidential = true;
                            }
                            if spec.parent_sandboxed {
                                profile.parent_sandboxed = true;
                            }

                            profile.repository_policy = spec.launch_policy.clone();
                            if let Some(admitter) = &spec.admitted_by {
                                profile.admitted_by = Some(admitter.clone());
                            }
                            let original_task = provenant_records::run::TaskRequest {
                                context: None,
                                extra: Default::default(),
                                limit_extensions: Default::default(),
                                build_heavy: spec.build_heavy,
                                title: None,
                                profile,
                                prompt_digest: provenant_records::CanonicalV1::file_digest(
                                    b"prompt",
                                ),
                                head: None,
                                source_head: None,
                                base: None,
                                fixture: None,
                                base_ref: None,
                                route: provenant_records::run::RequestedRoute::Named {
                                    required_family: None,
                                    extra: Default::default(),
                                    name: "junior".into(),
                                    effort: None,
                                },
                                account: spec.account.clone(),
                                authority_digest: provenant_core::Digest::from_bytes([0; 32]),
                                writer: None,
                                deliver: None,
                                provider_timeout_seconds: spec.provider_timeout.unwrap_or(3600),
                                delivery_id: None,
                                access: Default::default(),
                            };

                            let original_request = provenant_records::run::CanonicalRequest {
                                extra: Default::default(),
                                chair: None,
                                fresh_continuation: None,
                                schema_version: 1,
                                task: original_task.clone(),
                                parent: None,
                                lane_depth: None,
                                parent_attempt: None,
                            };

                            let rebuilt_spec = recovered_attempt_spec(&original_task, Some(&spec))?;

                            assert_eq!(rebuilt_spec.parent_sandboxed, spec.parent_sandboxed);
                            assert_eq!(rebuilt_spec.admitted_by, spec.admitted_by);
                            assert_eq!(rebuilt_spec.confidential, spec.confidential);
                            assert_eq!(rebuilt_spec.build_heavy, spec.build_heavy);
                            assert_eq!(rebuilt_spec.account, spec.account);
                            assert_eq!(rebuilt_spec.commit, spec.commit);
                            assert_eq!(rebuilt_spec.carry_uncommitted, spec.carry_uncommitted);
                            assert_eq!(
                                rebuilt_spec.provider_timeout,
                                Some(original_task.provider_timeout_seconds)
                            );

                            let rebuilt_profile = rebuilt_spec
                                .sealed_profile
                                .clone()
                                .ok_or("sealed profile")?;
                            assert!(rebuilt_spec.policy_sealed);
                            assert_eq!(rebuilt_spec.launch_policy, spec.launch_policy);
                            let rebuilt_task = provenant_records::run::TaskRequest {
                                fixture: None,
                                profile: rebuilt_profile,
                                build_heavy: rebuilt_spec.build_heavy,
                                account: rebuilt_spec.account.clone(),
                                provider_timeout_seconds: rebuilt_spec
                                    .provider_timeout
                                    .unwrap_or(original_task.provider_timeout_seconds),
                                ..original_task.clone()
                            };

                            let rebuilt_request = provenant_records::run::CanonicalRequest {
                                task: rebuilt_task,
                                ..original_request.clone()
                            };

                            assert_eq!(
                                original_request.digest()?,
                                rebuilt_request.digest()?,
                                "request digest must match after recovery rebuild"
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
