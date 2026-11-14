    #[test]
    fn account_profile_refusals_have_distinct_stable_codes() {
        use super::AccountProfileError;
        let error = super::resolve_account_reference(
            "missing",
            "account",
            &provenant_providers::accounts::Settings::default(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            super::EdgeError::AccountProfile {
                kind: AccountProfileError::Unknown,
                ..
            }
        ));
        assert_eq!(AccountProfileError::Unknown.code(), "UNKNOWN_ACCOUNT");
        assert_eq!(
            AccountProfileError::Duplicate.code(),
            "DUPLICATE_PACING_PROFILE"
        );
        assert_eq!(AccountProfileError::Ambiguous.code(), "CONFIG");
    }

    #[test]
    fn route_list_does_not_call_omitted_evidence_unprobed() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        let query = super::RouteListRequest {
            valid: false,
            adapter: Some("codex".into()),
            pool: None,
        };
        let mut list = super::display_list_data(&roots, &query)?;
        let model = super::ModelId::parse("codex/gpt-6-luna")?;
        assert!(super::unobserved_model(&model, &list).contains("not probed"));
        list.readiness_truncated = true;
        assert!(super::unobserved_model(&model, &list).contains("not shown"));
        Ok(())
    }

    #[test]
    fn recorded_route_readiness_keeps_model_and_account_scope() -> Result<(), Box<dyn Error>> {
        use provenant_quota::{
            ReadinessFile, ReadinessReason, ReadinessScope, ReadinessState, RouteReadiness,
        };
        let settings: provenant_providers::accounts::Settings =
            serde_json::from_value(serde_json::json!({"entries":{
                "one":{"adapter":"codex","label":"one","key":"key-one","directory":"/fixture/one"},
                "two":{"adapter":"codex","label":"two","key":"key-two","directory":"/fixture/two"}
            }}))?;
        let model = super::ModelId::parse("codex/gpt-6-luna")?;
        let scope = ReadinessScope {
            host: provenant_quota::LOCAL_HOST.into(),
            adapter: "codex".into(),
            account_key: "key-one".into(),
            route: model.as_str().into(),
        };
        let file = ReadinessFile {
            records: vec![
                RouteReadiness {
                    schema_version: 1,
                    scope: scope.clone(),
                    state: ReadinessState::Unavailable,
                    reason: Some(ReadinessReason::ProviderRefusal),
                    observed_unix_seconds: 10,
                    expires_unix_seconds: 20,
                    reset_unix_seconds: None,
                    evidence_ref: Some("fixture".into()),
                },
                RouteReadiness {
                    schema_version: 1,
                    scope: ReadinessScope {
                        account_key: "key-two".into(),
                        ..scope.clone()
                    },
                    state: ReadinessState::Ready,
                    reason: None,
                    observed_unix_seconds: 11,
                    expires_unix_seconds: 20,
                    reset_unix_seconds: None,
                    evidence_ref: Some("fixture".into()),
                },
                RouteReadiness {
                    schema_version: 1,
                    scope: ReadinessScope {
                        account_key: "key-two".into(),
                        route: "verified-pool".into(),
                        ..scope.clone()
                    },
                    state: ReadinessState::Unavailable,
                    reason: Some(ReadinessReason::Exhausted),
                    observed_unix_seconds: 12,
                    expires_unix_seconds: 20,
                    reset_unix_seconds: None,
                    evidence_ref: Some("fixture".into()),
                },
            ],
            ..Default::default()
        };
        let rows = super::recorded_model_readiness(
            &settings,
            &model,
            &[],
            None,
            Some("one"),
            |requested| file.get(requested, 15),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].host, provenant_quota::LOCAL_HOST);
        assert_eq!(rows[0].account_key, "key-one");
        assert_eq!(rows[0].model, model.as_str());
        assert_eq!(
            rows[0].observation.reason,
            Some(ReadinessReason::ProviderRefusal)
        );
        assert_eq!(rows[0].observation.observed_scope.as_ref(), Some(&scope));
        assert_eq!(rows[0].observation.observed_unix_seconds, Some(10));
        let other = super::recorded_model_readiness(
            &settings,
            &model,
            &[],
            None,
            Some("two"),
            |requested| file.get(requested, 15),
        );
        assert_eq!(other[0].observation.state, ReadinessState::Ready);
        assert_eq!(other[0].observation.observed_unix_seconds, Some(11));
        let pooled = super::recorded_model_readiness(
            &settings,
            &model,
            &["verified-pool".into()],
            None,
            Some("two"),
            |requested| file.get(requested, 15),
        );
        assert_eq!(
            pooled[0].observation.reason,
            Some(ReadinessReason::Exhausted)
        );
        assert_eq!(
            pooled[0].observation.observed_scope.as_ref().unwrap().route,
            "verified-pool"
        );
        let other_model = super::ModelId::parse("codex/gpt-6.1-terra")?;
        let other = super::recorded_model_readiness(
            &settings,
            &other_model,
            &[],
            None,
            Some("one"),
            |requested| file.get(requested, 15),
        );
        assert_eq!(other[0].observation.state, ReadinessState::Unknown);
        Ok(())
    }

    fn exclusion_roots(temp: &tempfile::TempDir) -> Result<provenant_core::Roots, Box<dyn Error>> {
        use std::path::Path;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        Ok(roots)
    }

    #[test]
    fn route_readiness_is_rendered_in_words() {
        use provenant_records::RouteExclusionReason;
        let ready = RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 120,
        };
        assert!(super::readiness_text(&ready).starts_with("ready "));
        let unknown = super::readiness_text(&RouteReadiness::unknown());
        assert!(
            unknown.contains("unknown (not probed") && unknown.contains("provenant quota tick"),
            "{unknown}"
        );
        let stale = super::readiness_text(&RouteReadiness::stale());
        assert!(stale.contains("unknown (observation expired"), "{stale}");
        let partial = RouteReadiness::Unavailable {
            extra: Default::default(),
            reason: RouteExclusionReason::UnknownPartialHeadroom,
        };
        assert!(super::readiness_text(&partial).starts_with("unknown (partial headroom"));
        let refused = RouteReadiness::Unavailable {
            extra: Default::default(),
            reason: RouteExclusionReason::AuthRequired,
        };
        assert_eq!(
            super::readiness_text(&refused),
            "unavailable (auth required)"
        );
    }

    #[test]
    fn resolve_after_quota_tick_uses_the_published_snapshot() -> Result<(), Box<dyn Error>> {
        use provenant_core::Clock;
        use provenant_quota::{
            AccountRecord, AccountScope, ProbeCapture, ProbeFailure, QuotaEffects, QuotaError,
            QuotaProbePlan,
        };
        use std::os::unix::fs::PermissionsExt;
        struct Effects;
        impl QuotaEffects for Effects {
            fn accounts(&self) -> Result<Vec<AccountScope>, QuotaError> {
                Ok(vec![AccountScope {
                    adapter: "codex".into(),
                    account_key: "key".into(),
                    label: "work".into(),
                }])
            }
            fn capture(
                &self,
                _: &QuotaProbePlan,
                _: Option<&AccountRecord>,
            ) -> Result<ProbeCapture, ProbeFailure> {
                Ok(ProbeCapture {
                    stdout: br#"{"payload":{"result":{"rateLimits":{"primary":{"usedPercent":20,"windowDurationMins":300}}}}}"#.to_vec(),
                    stderr: vec![],
                    exit_code: Some(0),
                    overflowed: false,
                })
            }
        }
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        std::fs::set_permissions(
            roots.state.as_path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            format!(
                "schema_version=1\n[domains.accounts.entries.work]\nadapter='codex'\nlabel='work'\nkey='key'\ndirectory={:?}\n",
                temp.path().to_string_lossy()
            ),
        )?;
        let report = crate::quota::execute(
            &crate::quota::Quota::Tick {
                adapter: crate::quota::Filter {
                    adapter: Some("codex".into()),
                },
                trigger: crate::quota::Trigger::Manual,
                power: crate::quota::Power::Ac,
            },
            &roots,
            provenant_providers::REGISTRY,
            &Effects,
        )?;
        assert_eq!(
            report.accounts[0].status,
            provenant_quota::QuotaStatus::Available
        );
        let command = super::Route::Resolve(super::RouteResolve {
            writes: false,
            account: None,
            name: None,
            request: super::RouteRequest {
                read_heavy: false,
                model: Some(super::Model("codex/gpt-6-luna".into())),
                route: None,
                effort: None,
                confidential: false,
                fallback: vec![],
                exclude: vec![],
            },
        });
        let super::Data::Resolved(preview) = super::execute(
            &command,
            &roots,
            provenant_os::clock::SystemClock::new().wall_now()?,
        )?
        else {
            return Err("expected resolved route".into());
        };
        assert_eq!(
            preview.recorded_readiness[0].observation.state,
            provenant_quota::ReadinessState::Ready
        );
        assert!(preview.admission_refusal.is_none());
        Ok(())
    }

    #[test]
    fn route_resolve_human_output_excludes_policy_prompt() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[domains.routing.model_policy.defaults.lane]\nstop_early_ok=false\n[domains.routing.model_policy.defaults.result]\nsections=['PRIVATE_SENTINEL']\n",
        )?;
        let command = super::Route::Resolve(super::RouteResolve {
            writes: false,
            account: None,
            name: None,
            request: super::RouteRequest {
                read_heavy: false,
                model: Some(super::Model("codex/gpt-6-luna".into())),
                route: None,
                effort: None,
                confidential: false,
                fallback: vec![],
                exclude: vec![],
            },
        });
        let super::Data::Resolved(preview) = super::execute(
            &command,
            &roots,
            provenant_core::WallTime::from_unix_seconds(100)?,
        )?
        else {
            panic!("expected route preview")
        };
        assert!(
            preview
                .model_policy
                .policy
                .guidance()
                .contains("PRIVATE_SENTINEL")
        );
        let text = super::resolved_text(&preview);
        assert!(!text.contains("PRIVATE_SENTINEL"), "{text}");
        assert!(!text.contains("Carry the task to completion"), "{text}");
        Ok(())
    }

    #[test]
    fn dispatch_rechecks_recorded_auth_with_a_healthy_account_probe()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_quota::{
            AccountRecord, AccountScope, LOCAL_HOST, ReadinessFile, ReadinessReason,
        };
        use std::os::unix::fs::PermissionsExt;
        for adapter in ["codex", "claude"] {
            let temp = tempfile::tempdir()?;
            let mut inputs =
                provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
            inputs.state_dir = Some(temp.path().join("state"));
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let dir = roots.state.as_path().join("readiness/key");
            std::fs::create_dir_all(&dir)?;
            for path in [
                roots.state.as_path(),
                roots.state.as_path().join("readiness").as_path(),
                dir.as_path(),
            ] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            }
            let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
            let account = AccountRecord::empty(AccountScope {
                adapter: adapter.into(),
                account_key: "key".into(),
                label: "work".into(),
            });
            let mut file = ReadinessFile::default();
            file.sync_probe_result(
                &account,
                LOCAL_HOST,
                now - 10,
                300,
                Some(ReadinessReason::SignedOut),
            )?;
            let path = dir.join(format!("{adapter}.json"));
            std::fs::write(&path, serde_json::to_vec(&file)?)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            let catalogue = super::Catalogue::bundled()?;
            let mut available = provenant_providers::AvailabilityRecord {
                schema_version: 1,
                adapter: adapter.into(),
                executable: format!("/fixture/{adapter}"),
                version: "fixture".into(),
                last_update: None,
                fixture_ref: format!("installed/{adapter}/version"),
                catalogue_revision: catalogue.revision().into(),
                account_key: Some("key".into()),
                plan: None,
                observed_models: Vec::new(),
                observed_unix_seconds: now,
                expires_unix_seconds: now + 300,
            };
            let request = super::RouteRequest {
                read_heavy: false,
                model: Some(super::Model(format!("{adapter}/unlisted-exact"))),
                route: None,
                effort: None,
                confidential: false,
                fallback: vec![],
                exclude: vec![],
            }
            .to_domain()?;
            let access = [provenant_routing::AccessContext {
                adapter,
                disclosure_ref: "fixture",
                confinement_ref: "fixture",
            }];
            let resolve = |record: &provenant_providers::AvailabilityRecord| {
                super::resolve_dispatch(
                    &request,
                    &roots,
                    Some("key"),
                    ("fixture", 0),
                    std::slice::from_ref(record),
                    &access,
                    "fixture",
                    None,
                    &[],
                )
            };
            let before = resolve(&available)?;
            // Current recorded sign-out evidence excludes until a healthy probe clears it.
            assert!(matches!(
                before.readiness,
                provenant_records::RouteReadiness::Unavailable {
                    reason: provenant_records::RouteExclusionReason::AuthRequired,
                    ..
                }
            ));
            available.fixture_ref = format!("installed/{adapter}/login-status");
            let after = resolve(&available)?;
            assert!(matches!(
                after.readiness,
                provenant_records::RouteReadiness::Ready { .. }
            ));
        }
        Ok(())
    }

    #[test]
    fn opencode_unverified_login_allows_selected_account_to_try()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_quota::{
            AccountRecord, AccountScope, LOCAL_HOST, ReadinessFile, ReadinessReason,
        };
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let dir = roots.state.as_path().join("readiness/go");
        std::fs::create_dir_all(&dir)?;
        for path in [
            roots.state.as_path(),
            roots.state.as_path().join("readiness").as_path(),
            dir.as_path(),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let account = AccountRecord::empty(AccountScope {
            adapter: "opencode".into(),
            account_key: "go".into(),
            label: "go".into(),
        });
        let mut file = ReadinessFile::default();
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            now - 10,
            300,
            Some(ReadinessReason::SignedOut),
        )?;
        let path = dir.join("opencode.json");
        std::fs::write(&path, serde_json::to_vec(&file)?)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let catalogue = super::Catalogue::bundled()?;
        let mut available = provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "opencode".into(),
            executable: "/fixture/opencode".into(),
            version: "fixture".into(),
            last_update: None,
            fixture_ref: "installed/opencode/version".into(),
            catalogue_revision: catalogue.revision().into(),
            account_key: Some("go".into()),
            plan: Some("go".into()),
            observed_models: Vec::new(),
            observed_unix_seconds: now,
            expires_unix_seconds: now + 300,
        };
        let request = super::RouteRequest {
            read_heavy: false,
            model: Some(super::Model(
                "opencode/opencode-go/deepseek-v4.1-flash".into(),
            )),
            route: None,
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec![],
        }
        .to_domain()?;
        let access = [provenant_routing::AccessContext {
            adapter: "opencode",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        }];
        let resolve = |record: &provenant_providers::AvailabilityRecord| {
            super::resolve_dispatch(
                &request,
                &roots,
                Some("go"),
                ("fixture", 0),
                std::slice::from_ref(record),
                &access,
                "fixture",
                None,
                &[],
            )
        };
        // Current recorded sign-out evidence excludes until a login probe clears it.
        assert!(matches!(
            resolve(&available)?.readiness,
            provenant_records::RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::AuthRequired,
                ..
            }
        ));
        available.fixture_ref = "installed/opencode/login-unverified".into();
        assert!(matches!(
            resolve(&available)?.readiness,
            provenant_records::RouteReadiness::Ready { .. }
        ));
        // The probe does not erase the recorded failure.
        assert_eq!(
            file.get(
                &provenant_quota::ReadinessScope {
                    host: LOCAL_HOST.into(),
                    adapter: "opencode".into(),
                    account_key: "go".into(),
                    route: "opencode/opencode-go/deepseek-v4.1-flash".into()
                },
                now
            )
            .reason,
            Some(ReadinessReason::SignedOut)
        );
        let zen_dir = roots.state.as_path().join("readiness/zen");
        std::fs::create_dir_all(&zen_dir)?;
        std::fs::set_permissions(&zen_dir, std::fs::Permissions::from_mode(0o700))?;
        let zen = AccountRecord::empty(AccountScope {
            adapter: "opencode".into(),
            account_key: "zen".into(),
            label: "zen".into(),
        });
        let mut zen_readiness = ReadinessFile::default();
        zen_readiness.sync_probe_result(
            &zen,
            LOCAL_HOST,
            now - 10,
            300,
            Some(ReadinessReason::SignedOut),
        )?;
        let zen_path = zen_dir.join("opencode.json");
        std::fs::write(&zen_path, serde_json::to_vec(&zen_readiness)?)?;
        std::fs::set_permissions(&zen_path, std::fs::Permissions::from_mode(0o600))?;
        let mut zen_available = available.clone();
        zen_available.account_key = Some("zen".into());
        zen_available.plan = Some("zen".into());
        zen_available.fixture_ref = "installed/opencode/version".into();
        let zen_request = super::RouteRequest {
            read_heavy: false,
            model: Some(super::Model(
                "opencode/opencode/deepseek-v4.1-flash-free".into(),
            )),
            route: None,
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec![],
        }
        .to_domain()?;
        let zen_result = super::resolve_dispatch(
            &zen_request,
            &roots,
            Some("zen"),
            ("fixture", 0),
            &[available.clone(), zen_available],
            &access,
            "fixture",
            None,
            &[],
        )?;
        // Another account's probe does not clear this account's current sign-out.
        assert!(matches!(
            zen_result.readiness,
            provenant_records::RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::AuthRequired,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn exclude_selectors_parse_and_unknown_ones_list_the_valid_forms() -> Result<(), Box<dyn Error>>
    {
        use provenant_routing::{ExclusionSelector, ExclusionSource};
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        let parse =
            |text: &str| super::exclusion_rules(&[text.to_owned()], ExclusionSource::Flag, &roots);
        assert_eq!(
            parse("claude")?[0].selector,
            ExclusionSelector::Provider("claude".into())
        );
        assert!(matches!(
            parse("codex/gpt-6-luna")?[0].selector,
            ExclusionSelector::Model(_)
        ));
        let message = parse("nonesuch").unwrap_err().to_string();
        assert!(
            message.contains("unknown exclusion `nonesuch`"),
            "{message}"
        );
        assert!(message.contains("valid forms: a provider"), "{message}");
        Ok(())
    }

    #[test]
    fn explain_names_exclusions_and_never_picks_the_excluded_provider() -> Result<(), Box<dyn Error>>
    {
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        let request = super::RouteRequest {
            read_heavy: false,
            model: None,
            route: Some("junior".into()),
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec!["claude".into()],
        };
        use super::{
            Clock, Data, EdgeError, Roots, Route, RouteResolve, RoutingError, SystemClock, execute,
            resolved_text,
        };
        let preview = |request: &super::RouteRequest, roots: &Roots| {
            let query = RouteResolve {
                writes: false,
                account: None,
                name: None,
                request: request.clone(),
            };
            let data = execute(
                &Route::Resolve(query),
                roots,
                SystemClock::new().wall_now()?,
            )?;
            let Data::Resolved(preview) = &data else {
                return Err(EdgeError::from(RoutingError::Catalogue));
            };
            Ok::<_, EdgeError>((
                serde_json::to_value(&data).map_err(|_| RoutingError::Catalogue)?,
                resolved_text(preview),
            ))
        };
        let (data, text) = preview(&request, &roots)?;
        assert_ne!(data["adapter"], "claude");
        assert_eq!(data["allow_unknown_partial_headroom"]["value"], false);
        assert_eq!(data["allow_unknown_partial_headroom"]["source"], "default");
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.allow_unknown_partial_headroom'=true\n",
        )?;
        let (override_data, override_text) = preview(&request, &roots)?;
        assert_eq!(
            override_data["allow_unknown_partial_headroom"]["value"],
            true
        );
        assert_eq!(
            override_data["allow_unknown_partial_headroom"]["source"],
            "override"
        );
        assert!(override_text.contains("allow unknown partial headroom: true (source: Override)"));
        assert!(
            text.contains("claude/claude-haiku-5-5: excluded by flag: provider claude"),
            "{text}"
        );
        let all = super::RouteRequest {
            exclude: ["claude", "codex", "opencode", "agy", "cursor"]
                .map(String::from)
                .to_vec(),
            ..request
        };
        let message = preview(&all, &roots).unwrap_err().to_string();
        assert!(message.contains("excluded by flag: provider"), "{message}");
        Ok(())
    }

    #[test]
    fn model_exclusion_commands_record_list_and_remove() -> Result<(), Box<dyn Error>> {
        use std::path::Path;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        let now = provenant_core::WallTime::from_unix_seconds(100)?;
        let model = super::Model("codex/gpt-6-luna".into());
        super::edit_model_exclusion(
            &roots,
            &model,
            Some(provenant_core::WallTime::from_unix_seconds(200)?),
            true,
            now,
        )?;
        let rows = super::list_overrides(&roots, now)?;
        assert!(rows.iter().any(|row| row.kind == "model_exclusion"
            && row.active
            && row.remaining_seconds == Some(100)));
        super::edit_model_exclusion(&roots, &model, None, false, now)?;
        assert!(
            !super::list_overrides(&roots, now)?
                .iter()
                .any(|row| row.kind == "model_exclusion")
        );
        Ok(())
    }

    #[test]
    fn adding_an_exclusion_prunes_expired_rows_and_notes_bad_ones() -> Result<(), Box<dyn Error>> {
        use std::path::Path;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[domains.routing]\nmodel_exclusions=[\
             {model='claude/old-model',schedule={expires='1970-01-01T00:00:01Z'}},\
             {model='claude/not-a-row',schedule={expires='garbage'}},\
             {model='not a model'}]\n", // timebomb: allow fixed epoch fixture, not the live clock
        )?;
        let now = provenant_core::WallTime::from_unix_seconds(100)?;
        let notes = super::exclusion_notes(&roots)?;
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[0].contains("stays excluded with no schedule"),
            "{notes:?}"
        );
        assert!(notes[1].contains("is ignored"), "{notes:?}");
        let report = super::execute(
            &super::Route::Exclude(super::RouteExclude {
                command: super::RouteExcludeCommand::List,
            }),
            &roots,
            now,
        )?;
        let super::Data::Overrides(report) = report else {
            return Err("overrides".into());
        };
        assert_eq!(report.notes.len(), 2);
        let model = super::Model("codex/gpt-6-luna".into());
        super::edit_model_exclusion(&roots, &model, None, true, now)?;
        let text = std::fs::read_to_string(roots.state.as_path().join("config.toml"))?;
        // The lapsed row is gone; the unreadable ones are kept for the owner.
        assert!(!text.contains("old-model"), "{text}");
        assert!(
            text.contains("not-a-row") && text.contains("not a model"),
            "{text}"
        );
        assert!(text.contains("gpt-6-luna"), "{text}");
        Ok(())
    }

    #[test]
    fn pacing_report_prints_every_active_override() -> Result<(), Box<dyn Error>> {
        use provenant_core::{Clock, MonoTime, TestClock, WallTime};
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let report = super::PacingReport {
            recorded: "codex".into(),
            active_overrides: serde_json::from_value(serde_json::json!([
                {"provider":"codex", "policy":"pace"},
                {"provider":"claude", "policy":"off"}
            ]))?,
        };
        let text = report.human(clock.wall_now()?);
        assert!(text.contains("active overrides: 2"));
        assert!(text.contains("pacing provider codex pace active"));
        assert!(text.contains("pacing provider claude off active"));
        Ok(())
    }

    #[test]
    fn override_instants_and_windows_parse_relative_to_the_supplied_instant() {
        use super::{instant, parse_window};

        let at = |seconds| provenant_core::WallTime::from_unix_seconds(seconds).ok();
        let epoch = provenant_core::WallTime::from_micros(0).expect("epoch");
        assert_eq!(instant("1s", at(99).expect("instant")), at(100));
        assert_eq!(instant("+5m", at(99).expect("instant")), at(399));
        assert_eq!(instant("+0m", epoch), None);
        assert_eq!(instant("++5m", epoch), None);
        assert_eq!(instant("2h", epoch), at(7200));
        assert_eq!(instant("1w", epoch), at(604_800));
        assert_eq!(instant("1h30m", epoch), at(5_400));
        assert_eq!(instant("0h", epoch), None);
        assert_eq!(instant("1970-01-01T00:01:00Z", epoch), at(60)); // timebomb: allow parse-only fixture with an injected instant
        let weekdays = parse_window("mon-wed,fri,6@23-2").expect("window");
        assert_eq!(weekdays.weekdays, [0, 1, 2, 4, 6]);
        assert_eq!((weekdays.start_hour, weekdays.end_hour), (23, 2));
        let every_day = parse_window("9-17").expect("window");
        assert!(every_day.weekdays.is_empty());
        for invalid in ["fri-mon@9-17", "funday@9-17", "7@9-17", "9", "a-b"] {
            assert!(parse_window(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn override_grammar_accepts_model_and_pacing_forms() {
        use clap::Parser;
        #[derive(Parser, Debug)]
        struct Command {
            #[command(subcommand)]
            route: super::Route,
        }
        for args in [
            vec![
                "test",
                "override",
                "set",
                "--model",
                "codex/gpt-6-luna",
                "--risk-tier",
                "substantial",
                "--task-kind",
                "repair",
                "--until",
                "1h",
            ],
            vec![
                "test",
                "override",
                "set",
                "--provider",
                "codex",
                "--policy",
                "pace",
            ],
            vec![
                "test",
                "override",
                "clear",
                "--risk-tier",
                "substantial",
                "--task-kind",
                "repair",
            ],
        ] {
            let parsed = Command::try_parse_from(args).expect("valid override command");
            assert!(matches!(parsed.route, super::Route::Override(_)));
        }
        assert!(
            Command::try_parse_from([
                "test",
                "override",
                "set",
                "--model",
                "codex/gpt-6-luna",
                "--risk-tier",
                "substantial",
                "--task-kind",
                "repair"
            ])
            .is_err()
        );
    }
