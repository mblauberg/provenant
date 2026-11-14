    #[test]
    fn model_override_set_and_clear_preserve_expiry_and_pacing() -> Result<(), Box<dyn Error>> {
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &std::path::Path,
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
        let path = roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing]\npacing_overrides=[{provider='codex',policy='off'}]\n",
        )?;
        let now = provenant_core::WallTime::from_unix_seconds(100)?;
        let request = super::OverrideSet {
            account: None,
            provider: None,
            all: false,
            policy: None,
            model: Some(super::Model("codex/gpt-6-luna".into())),
            risk_tier: Some("substantial".into()),
            task_kind: Some("repair".into()),
            starts: None,
            until: Some("1h".into()),
            window: vec![],
            tz: None,
        };
        super::set_override(&roots, &request, now)?;
        let config = provenant_config::read_with_domains(&roots, &crate::domains::SECTIONS)?;
        assert_eq!(config.domains.routing.model_choices.len(), 1);
        assert_eq!(
            config.domains.routing.model_choices[0].expires,
            Some(provenant_core::WallTime::from_unix_seconds(3700)?)
        );
        assert_eq!(config.domains.routing.pacing_overrides.len(), 1);
        super::clear_model_choice(&roots, "substantial", "repair", now)?;
        let config = provenant_config::read_with_domains(&roots, &crate::domains::SECTIONS)?;
        assert!(config.domains.routing.model_choices.is_empty());
        assert_eq!(config.domains.routing.pacing_overrides.len(), 1);
        Ok(())
    }

    #[test]
    fn override_set_replaces_its_scope_and_prunes_expired_rows() -> Result<(), Box<dyn Error>> {
        use super::{OverrideSet, PacingChoice};
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
        let path = roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing]\npacing_overrides=[{account='codex/removed',policy='pace',schedule={expires='1970-01-01T00:00:01Z'}},{provider='codex',policy='off'},{provider='claude',policy='off'}]\n", // timebomb: allow fixed epoch fixture, not the live clock
        )?;

        let data = super::set_override(
            &roots,
            &OverrideSet {
                account: None,
                provider: Some("codex".into()),
                all: false,
                policy: Some(PacingChoice::Max),
                model: None,
                risk_tier: None,
                task_kind: None,
                starts: None,
                until: Some("1h".into()),
                window: vec!["mon-fri@23-7".into()],
                tz: Some("Australia/Sydney".into()),
            },
            provenant_core::WallTime::from_unix_seconds(2)?,
        )?;

        let report = serde_json::to_value(&data)?;
        assert_eq!(report["recorded"]["scope"], "provider codex");
        assert_eq!(report["recorded"]["value"], "max");
        assert_eq!(report["recorded"]["remaining_seconds"], 3600);
        assert_eq!(report["cleared"].as_array().map(Vec::len), Some(2));
        assert_eq!(report["overrides"].as_array().map(Vec::len), Some(2));
        let saved = std::fs::read_to_string(path)?;
        let saved: serde_json::Value = toml_edit::de::from_str(&saved)?;
        let overrides = saved["domains"]["routing"]["pacing_overrides"]
            .as_array()
            .ok_or("overrides")?;
        assert!(
            !overrides
                .iter()
                .any(|row| row["account"] == "codex/removed")
        );
        assert_eq!(
            overrides
                .iter()
                .filter(|row| row["provider"] == "codex")
                .count(),
            1
        );
        assert!(overrides.iter().any(|row| row["provider"] == "claude"));
        let codex = overrides
            .iter()
            .find(|row| row["provider"] == "codex")
            .ok_or("codex")?;
        assert_eq!(codex["schedule"]["expires"], "1970-01-01T01:00:02.000000Z"); // timebomb: fixed fixture instant
        assert_eq!(codex["schedule"]["tz"], "Australia/Sydney");
        assert_eq!(
            codex["schedule"]["windows"],
            serde_json::json!([{ "weekdays": [0,1,2,3,4], "start_hour": 23, "end_hour": 7 }])
        );
        Ok(())
    }

    #[test]
    fn pacing_account_references_reject_collisions_and_keep_adapter() {
        use provenant_providers::accounts::Settings;
        use provenant_routing::RoutingConfig;
        let settings: Settings = serde_json::from_value(serde_json::json!({"entries": {
            "first": {"adapter":"claude", "label":"work", "key":"shared", "directory":"/tmp/a"},
            "shared": {"adapter":"codex", "label":"shared", "key":"other", "directory":"/tmp/b"},
            "third": {"adapter":"agy", "label":"personal", "key":"shared", "directory":"/tmp/c"}
        }}))
        .unwrap();
        for config in [
            serde_json::json!({"pacing":{"accounts":{"shared":{"policy":"pace"}}}}),
            serde_json::json!({"pacing":{"free":{"codex":{"deadline":{"reset_of":"shared"}}}}}),
        ] {
            let mut config: RoutingConfig = serde_json::from_value(config).unwrap();
            let error = super::resolve_pacing_accounts(&mut config, &settings).unwrap_err();
            assert!(matches!(
                error,
                super::EdgeError::AccountProfile {
                    kind: super::AccountProfileError::Ambiguous,
                    ..
                }
            ));
            let error = error.to_string();
            for expected in ["claude/shared", "codex/other", "agy/shared", "adapter/key"] {
                assert!(error.contains(expected), "{error}");
            }
        }
        let mut config: RoutingConfig = serde_json::from_value(serde_json::json!({"pacing":{
            "accounts":{"work":{"deadline":{"reset_of":"agy/shared"}}}
        }}))
        .unwrap();
        super::resolve_pacing_accounts(&mut config, &settings).unwrap();
        let profile = &config.pacing.accounts["claude/shared"];
        assert!(
            matches!(&profile.deadline, Some(provenant_routing::PacingDeadline::ResetOf(r)) if r.reset_of == "agy/shared")
        );
    }
    #[test]
    fn override_references_resolve_and_registered_labels_precede_derived_accounts() {
        use provenant_providers::accounts::Settings;
        use provenant_routing::RoutingConfig;
        let settings: Settings = serde_json::from_value(serde_json::json!({"entries": {
            "one": {"adapter":"claude", "label":"office", "key":"shared", "directory":"/tmp/a"},
            "two": {"adapter":"codex", "label":"other", "key":"shared", "directory":"/tmp/b"},
            "three": {"adapter":"opencode", "label":"zen", "key":"work", "directory":"/tmp/c"},
            "four": {"adapter":"opencode", "label":"go", "key":"go", "directory":"/tmp/d"}
        }}))
        .unwrap();
        assert_eq!(
            super::resolve_account_reference("zen", "test", &settings).unwrap(),
            "opencode/work"
        );
        assert_eq!(
            super::resolve_account_reference("opencode/zen", "test", &settings).unwrap(),
            "opencode/zen"
        );
        assert!(matches!(
            super::resolve_account_reference("shared", "test", &settings),
            Err(super::EdgeError::AccountProfile {
                kind: super::AccountProfileError::Ambiguous,
                ..
            })
        ));
        for (input, expected) in [
            ("office", "claude/shared"),
            ("claude/shared", "claude/shared"),
        ] {
            let mut config: RoutingConfig =
                serde_json::from_value(serde_json::json!({"pacing_overrides": [{
                    "account": input, "policy": "pace", "schedule": {"expires": 2000000000_i64}
                }]}))
                .unwrap();
            super::resolve_pacing_accounts(&mut config, &settings).unwrap();
            assert_eq!(
                config.pacing_overrides[0].account.as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn names_resolve_to_routes_then_exact_then_unique_models() {
        use super::{Named, match_name};
        let routes = ["junior", "utility"].map(String::from).into();
        let models =
            ["codex/gpt-6-luna", "codex/gpt-6.1-terra", "agy/gpt-6-luna"].map(String::from);
        let model = |name: &str| Named::Model(name.into());
        assert_eq!(
            match_name("utility", &routes, &models),
            Ok(Named::Route("utility".into()))
        );
        assert_eq!(
            match_name("terra", &routes, &models),
            Ok(model("codex/gpt-6.1-terra"))
        );
        assert_eq!(
            match_name("GPT-6.1-TERRA", &routes, &models),
            Ok(model("codex/gpt-6.1-terra"))
        );
        // An adapter-qualified name goes to resolve as the exact model.
        assert_eq!(
            match_name("codex/other", &routes, &models),
            Ok(model("codex/other"))
        );
        // The same ID under two adapters, or a fragment of several, is not guessed.
        let Err(both) = match_name("gpt-6-luna", &routes, &models) else {
            panic!("one ID under two adapters must not resolve");
        };
        assert!(
            both.contains(&"codex/gpt-6-luna".to_owned())
                && both.contains(&"agy/gpt-6-luna".to_owned())
        );
        let Err(close) = match_name("gpt", &routes, &models) else {
            panic!("a fragment of several models must not resolve");
        };
        assert_eq!(close.len(), 3);
        assert!(match_name("zzzzzz", &routes, &models).is_err_and(|close| close.is_empty()));
    }

    #[test]
    fn only_opencode_routes_draw_on_the_free_pool() {
        assert_eq!(
            provenant_routing::free_pools("opencode", || true),
            ["opencode-free"]
        );
        assert!(provenant_routing::free_pools("opencode", || false).is_empty());
        assert!(provenant_routing::free_pools("codex", || true).is_empty());
    }

    #[test]
    fn go_allowance_preference_never_refuses_a_ready_dispatch() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &std::path::Path,
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
        std::fs::set_permissions(
            roots.state.as_path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        let path = roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\ntest=[{model='opencode/opencode-go/glm-5.3-flash',weight=1},{model='opencode/opencode-go/deepseek-v4.1-flash',weight=0}]\n[domains.routing.opencode_go]\nmin_monthly_allowance_usd=61\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let catalogue = super::Catalogue::bundled()?;
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let availability = [provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "opencode".into(),
            executable: "/fixture/opencode".into(),
            version: "fixture".into(),
            fixture_ref: "fixture".into(),
            catalogue_revision: catalogue.revision().into(),
            account_key: Some("go".into()),
            observed_models: vec![
                "opencode-go/glm-5.3-flash".into(),
                "opencode-go/deepseek-v4.1-flash".into(),
            ],
            observed_unix_seconds: now,
            expires_unix_seconds: now + 300,
            last_update: None,
        }];
        let access = [provenant_routing::AccessContext {
            adapter: "opencode",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        }];
        let request = |model: Option<&str>, route: Option<&str>| {
            super::RouteRequest {
                read_heavy: false,
                model: model.map(|m| super::Model(m.into())),
                route: route.map(str::to_owned),
                effort: None,
                confidential: false,
                fallback: vec![],
                exclude: vec![],
            }
            .to_domain()
        };
        let resolve = |request: &provenant_routing::RouteRequest| {
            super::resolve_dispatch(
                request,
                &roots,
                None,
                ("fixture", 0),
                &availability,
                &access,
                "fixture",
                None,
                &[],
            )
        };
        for floor in [61, 1_000] {
            std::fs::write(
                &path,
                format!(
                    "schema_version=1\n[domains.routing.pools]\ntest=[{{model='opencode/opencode-go/glm-5.3-flash',weight=1}}]\n[domains.routing.opencode_go]\nmin_monthly_allowance_usd={floor}\n"
                ),
            )?;
            for query in [
                request(Some("opencode/opencode-go/glm-5.3-flash"), None)?,
                request(None, Some("test"))?,
            ] {
                let selected = resolve(&query)?;
                assert!(matches!(selected.readiness, RouteReadiness::Ready { .. }));
                assert!(super::check_dispatch_candidate(&roots, &selected, super::REGISTRY).is_ok());
                let warning = &selected.extra["account_load"]["warnings"];
                assert!(warning.as_array().is_some_and(|rows| !rows.is_empty()));
                assert!(warning.to_string().contains("source=config"));
            }
        }
        Ok(())
    }

    #[test]
    fn spent_account_yields_to_a_ready_account_for_the_same_model() -> Result<(), Box<dyn Error>> {
        use provenant_quota::{AccountRecord, AccountScope, QuotaSnapshot, QuotaStatus};
        use std::os::unix::fs::PermissionsExt;

        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &std::path::Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let catalogue = super::Catalogue::bundled()?;
        let mut availability = Vec::new();
        for (key, status) in [
            ("fresh", QuotaStatus::Available),
            ("exhausted", QuotaStatus::Exhausted),
        ] {
            let account = AccountScope {
                adapter: "codex".into(),
                account_key: key.into(),
                label: key.into(),
            };
            let mut record = AccountRecord::empty(account.clone());
            record.snapshot = Some(QuotaSnapshot {
                schema_version: 1,
                account,
                revision: 0,
                status,
                observed_unix_seconds: now,
                source_ref: "fixture".into(),
                authority_ref: None,
                details: None,
                windows: Vec::new(),
            });
            let quota = roots.state.as_path().join("quota");
            let directory = quota.join(key);
            std::fs::create_dir_all(&directory)?;
            for directory in [roots.state.as_path(), quota.as_path(), directory.as_path()] {
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
            }
            let path = directory.join("codex.json");
            std::fs::write(&path, serde_json::to_vec(&record)?)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            availability.push(provenant_providers::AvailabilityRecord {
                schema_version: 1,
                adapter: "codex".into(),
                executable: "/fixture/codex".into(),
                version: "fixture".into(),
                last_update: None,
                fixture_ref: "fixture".into(),
                catalogue_revision: catalogue.revision().into(),
                account_key: Some(key.into()),
                observed_models: Vec::new(),
                observed_unix_seconds: now,
                expires_unix_seconds: now + 300,
            });
        }
        let request = super::RouteRequest {
            read_heavy: false,
            model: Some(super::Model("codex/gpt-6-luna".into())),
            route: None,
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec![],
        }
        .to_domain()?;
        let access = [provenant_routing::AccessContext {
            adapter: "codex",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        }];
        // The spent account is listed first: it must not be drawn.
        let resolved = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &availability,
            &access,
            "fixture",
            None,
            &[],
        )?;
        assert!(matches!(&resolved.readiness, RouteReadiness::Ready { .. }));
        assert_eq!(resolved.account_key.as_deref(), Some("fresh"));
        Ok(())
    }


    #[test]
    fn unlisted_exact_models_keep_account_quota_gates() -> Result<(), Box<dyn Error>> {
        use provenant_quota::{AccountRecord, AccountScope, QuotaSnapshot, QuotaStatus};
        use provenant_records::RouteExclusionReason;
        use std::os::unix::fs::PermissionsExt;

        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &std::path::Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let catalogue = super::Catalogue::bundled()?;
        let mut availability = Vec::new();
        for (key, status) in [
            ("exhausted", QuotaStatus::Exhausted),
            ("fresh", QuotaStatus::Available),
        ] {
            let account = AccountScope {
                adapter: "codex".into(),
                account_key: key.into(),
                label: key.into(),
            };
            let mut record = AccountRecord::empty(account.clone());
            record.snapshot = Some(QuotaSnapshot {
                schema_version: 1,
                account,
                revision: 0,
                status,
                observed_unix_seconds: now,
                source_ref: "fixture".into(),
                authority_ref: None,
                details: None,
                windows: Vec::new(),
            });
            let quota = roots.state.as_path().join("quota");
            let directory = quota.join(key);
            std::fs::create_dir_all(&directory)?;
            for directory in [roots.state.as_path(), quota.as_path(), directory.as_path()] {
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
            }
            let path = directory.join("codex.json");
            std::fs::write(&path, serde_json::to_vec(&record)?)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            availability.push(provenant_providers::AvailabilityRecord {
                schema_version: 1,
                adapter: "codex".into(),
                executable: "/fixture/codex".into(),
                version: "fixture".into(),
                last_update: None,
                fixture_ref: "fixture".into(),
                catalogue_revision: catalogue.revision().into(),
                account_key: Some(key.into()),
                observed_models: Vec::new(),
                observed_unix_seconds: now,
                expires_unix_seconds: now + 300,
            });
        }
        let request = super::RouteRequest {
            read_heavy: false,
            model: Some(super::Model("codex/unlisted-exact".into())),
            route: None,
            effort: None,
            confidential: false,
            fallback: vec![super::Model("codex/unlisted-fallback".into())],
            exclude: vec![],
        }
        .to_domain()?;
        let access = [provenant_routing::AccessContext {
            adapter: "codex",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        }];
        let exhausted = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &availability[..1],
            &access,
            "fixture",
            None,
            &[],
        )?;
        assert!(matches!(
            &exhausted.readiness,
            RouteReadiness::Unavailable {
                reason: RouteExclusionReason::Exhausted,
                ..
            }
        ));
        assert!(matches!(
            &exhausted.alternatives.as_slice()[0].readiness,
            RouteReadiness::Unavailable {
                reason: RouteExclusionReason::Exhausted,
                ..
            }
        ));
        let fresh = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &availability,
            &access,
            "fixture",
            None,
            &[],
        )?;
        assert!(matches!(fresh.readiness, RouteReadiness::Ready { .. }));
        assert_eq!(fresh.account_key.as_deref(), Some("fresh"));
        assert_eq!(
            fresh.alternatives.as_slice()[0].account_key.as_deref(),
            Some("fresh")
        );
        Ok(())
    }

    #[test]
    fn refused_account_falls_through_to_another_for_the_same_model() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &std::path::Path,
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
        std::fs::set_permissions(
            roots.state.as_path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let catalogue = super::Catalogue::bundled()?;
        let availability: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|key| provenant_providers::AvailabilityRecord {
                schema_version: 1,
                adapter: "codex".into(),
                executable: "/fixture/codex".into(),
                version: "fixture".into(),
                last_update: None,
                fixture_ref: "fixture".into(),
                catalogue_revision: catalogue.revision().into(),
                account_key: Some(key.into()),
                observed_models: vec!["gpt-6-luna".into()],
                observed_unix_seconds: now,
                expires_unix_seconds: now + 300,
            })
            .collect();
        let access = [provenant_routing::AccessContext {
            adapter: "codex",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        }];
        let request = super::RouteRequest {
            read_heavy: false,
            model: Some(super::Model("codex/gpt-6-luna".into())),
            route: None,
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec![],
        }
        .to_domain()?;
        let resolve = |account: Option<&str>| {
            super::resolve_dispatch(
                &request,
                &roots,
                account,
                ("fixture", 0),
                &availability,
                &access,
                "fixture",
                None,
                &[],
            )
        };
        let drawn = resolve(None)?;
        let spent = drawn.account_key.clone().ok_or("account")?;
        assert!(super::check_dispatch_candidate(&roots, &drawn, super::REGISTRY).is_ok());
        // The provider refused the drawn account: the next dispatch keeps the
        // model and takes the other account instead of failing.
        crate::quota::persist_provider_refusal(
            roots.state.as_path(),
            &provenant_records::run::RouteRecord {
                provider: Some("codex".into()),
                model: Some("codex/gpt-6-luna".into()),
                account_key: Some(spent.clone()),
                account_label: Some(spent.clone()),
                ..Default::default()
            },
            None,
            now,
            Some(now + 3_600),
        )?;
        let preview = super::Route::Resolve(super::RouteResolve {
            writes: false,
            account: None,
            name: Some("codex/gpt-6-luna".into()),
            request: super::RouteRequest {
                read_heavy: false,
                model: None,
                route: None,
                effort: None,
                confidential: false,
                fallback: vec![],
                exclude: vec![],
            },
        });
        let now_wall = provenant_core::WallTime::from_micros(now * 1_000_000)?;
        let super::Data::Resolved(preview) = super::execute(&preview, &roots, now_wall)? else {
            return Err("expected route preview".into());
        };
        assert!(preview.provider_refusals.iter().any(|reason| reason.contains(&spent) && reason.contains("scoped provider refusal")), "{:?}", preview.provider_refusals);
        // A refusal written after resolution still stops the launch.
        let error = super::check_dispatch_candidate(&roots, &drawn, super::REGISTRY).unwrap_err();
        assert!(matches!(error, super::EdgeError::Quota(provenant_quota::QuotaError::Refused(_))));
        let redrawn = resolve(None)?;
        assert!(super::check_dispatch_candidate(&roots, &redrawn, super::REGISTRY).is_ok());
        assert_eq!(redrawn.model, "gpt-6-luna");
        assert!(matches!(redrawn.readiness, RouteReadiness::Ready { .. }));
        assert_ne!(redrawn.account_key.as_deref(), Some(spent.as_str()));
        // A pinned account is the caller's choice and is never swapped.
        // It resolves as unavailable with the typed reason rather than ready.
        let RouteReadiness::Unavailable { reason, .. } = resolve(Some(&spent))?.readiness else {
            return Err("a pinned refused account must be unavailable".into());
        };
        assert_eq!(
            reason,
            provenant_records::RouteExclusionReason::ProviderRefused
        );
        // The unavailable reason retains the typed owner's cause and repair.
        let diagnostic = reason.diagnostic("REFUSED");
        let detail = provenant_routing::exclusion_detail(&reason);
        assert!(detail.contains(diagnostic.detail.as_deref().ok_or("cause detail")?));
        assert!(detail.contains(diagnostic.next.as_deref().ok_or("cause action")?));
        assert!(!detail.contains("ProviderRefused"));
        Ok(())
    }

    #[test]
    fn exhausted_go_plan_binds_only_go_models() -> Result<(), Box<dyn Error>> {
        use provenant_quota::{AccountRecord, AccountScope, QuotaSnapshot, QuotaStatus};
        let account = AccountScope {
            adapter: "opencode".into(),
            account_key: "go".into(),
            label: "go".into(),
        };
        let mut record = AccountRecord::empty(account.clone());
        record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account,
            revision: 0,
            status: QuotaStatus::Exhausted,
            observed_unix_seconds: 100,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: Vec::new(),
        });
        let availability = [provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "opencode".into(),
            executable: "opencode".into(),
            version: "fixture".into(),
            last_update: None,
            fixture_ref: "fixture".into(),
            catalogue_revision: "fixture".into(),
            account_key: Some("go".into()),
            observed_models: vec![
                "opencode-go/glm-5.1".into(),
                "opencode/glm-5.3-flash-free".into(),
                "openrouter/some-model".into(),
            ],
            observed_unix_seconds: 100,
            expires_unix_seconds: 200,
        }];
        let catalogue = super::Catalogue::bundled()?;
        let requested = [
            super::ModelId::parse("opencode/opencode-go/unlisted")?,
            super::ModelId::parse("opencode/opencode/unlisted")?,
            super::ModelId::parse("opencode/openrouter/unlisted")?,
        ];
        let policies = provenant_providers::quota_policies(provenant_providers::REGISTRY);
        let observed =
            super::quota_observations(&catalogue, &availability, &[record], &policies, &requested)?;
        let bound: Vec<_> = observed.iter().map(|o| o.model.as_str()).collect();
        assert!(bound.contains(&"opencode/opencode-go/glm-5.1"));
        assert!(bound.contains(&"opencode/opencode-go/unlisted"));
        assert!(!bound.contains(&"opencode/opencode/glm-5.3-flash-free"));
        assert!(!bound.contains(&"opencode/openrouter/some-model"));
        assert!(bound.iter().all(|m| m.starts_with("opencode/opencode-go/")));
        Ok(())
    }

    #[test]
    fn observed_display_names_with_spaces_are_skipped() -> Result<(), Box<dyn Error>> {
        use provenant_quota::{AccountRecord, AccountScope, QuotaSnapshot, QuotaStatus};
        let account = AccountScope {
            adapter: "opencode".into(),
            account_key: "go".into(),
            label: "go".into(),
        };
        let mut record = AccountRecord::empty(account.clone());
        record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account,
            revision: 0,
            status: QuotaStatus::Exhausted,
            observed_unix_seconds: 100,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: Vec::new(),
        });
        let availability = [provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "opencode".into(),
            executable: "opencode".into(),
            version: "fixture".into(),
            last_update: None,
            fixture_ref: "fixture".into(),
            catalogue_revision: "fixture".into(),
            account_key: Some("go".into()),
            observed_models: vec!["opencode-go/glm-5.1".into(), "opencode-go/GPT-OSS 120B (Medium)".into()],
            observed_unix_seconds: 100,
            expires_unix_seconds: 200,
        }];
        let catalogue = super::Catalogue::bundled()?;
        let policies = provenant_providers::quota_policies(provenant_providers::REGISTRY);
        let observed =
            super::quota_observations(&catalogue, &availability, &[record], &policies, &[])?;
        let bound: Vec<_> = observed.iter().map(|o| o.model.as_str()).collect();
        assert!(bound.contains(&"opencode/opencode-go/glm-5.1"));
        assert!(bound.iter().all(|m| !m.contains(' ')));
        Ok(())
    }

    fn resolved(adapter: &str, model: &str, roll: u64) -> ResolvedRoute {
        let readiness = RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 200,
        };
        ResolvedRoute {
            extra: Default::default(),
            adapter: adapter.into(),
            model: model.into(),
            effort: None,
            catalogue_revision: "fixture".into(),
            account_key: None,
            host: "fixture".into(),
            alternatives: RouteAlternatives::default(),
            exclusions: Vec::new(),
            eligibility: RouteEligibility::Eligible,
            readiness: readiness.clone(),
            confidentiality: RouteConfidentiality {
                extra: Default::default(),
                evidence_ref: "fixture".into(),
            },
            pool_selection: Some(RouteSelectionReceipt {
                extra: Default::default(),
                pool: "junior".into(),
                candidates: vec![RouteSelectionCandidate {
                    extra: Default::default(),
                    pacing_detail: None,
                    model: format!("{adapter}/{model}"),
                    weight: 1,
                    factor_basis_points: 10_000,
                    effort: None,
                    readiness: Some(readiness),
                    exclusion: None,
                }],
                roll,
                total_weight: 10_000,
                pick: format!("{adapter}/{model}"),
                reason: None,
                redraws: Vec::new(),
            }),
        }
    }
