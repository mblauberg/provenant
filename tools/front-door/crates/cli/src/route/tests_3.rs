    #[test]
    fn admission_redraw_history_is_retained_in_the_pool_receipt() -> Result<(), Box<dyn Error>> {
        let first = resolved("codex", "paid", 17);
        let mut second = resolved("claude", "ready", 3);
        super::record_pool_redraw(&first, &mut second, "allowance exhausted")?;

        let mut third = resolved("agy", "ready", 9);
        third
            .pool_selection
            .as_mut()
            .ok_or("third receipt")?
            .candidates
            .push(RouteSelectionCandidate {
                ranking: Default::default(),
                extra: Default::default(),
                pacing_detail: None,
                model: "codex/paid".into(),
                weight: 1,
                factor_basis_points: 10_000,
                effort: None,
                readiness: None,
                exclusion: Some("admission rejected".into()),
            });
        super::record_pool_redraw(&second, &mut third, "scoped provider refusal")?;
        let value = serde_json::to_value(third)?;
        let redraws = &value["pool_selection"]["redraws"];
        assert_eq!(redraws[0]["rejected_pick"], "codex/paid");
        assert_eq!(redraws[0]["roll"], 17);
        assert_eq!(redraws[0]["reason"], "allowance exhausted");
        assert_eq!(redraws[1]["rejected_pick"], "claude/ready");
        assert_eq!(redraws[1]["roll"], 3);
        assert_eq!(redraws[1]["reason"], "scoped provider refusal");
        let rejected = value["pool_selection"]["candidates"]
            .as_array()
            .and_then(|candidates| candidates.iter().find(|row| row["model"] == "codex/paid"))
            .ok_or("rejected candidate")?;
        assert_eq!(rejected["exclusion"], "admission rejected");
        assert_eq!(rejected["readiness"]["status"], "ready");
        Ok(())
    }
    #[test]
    fn admission_redraw_keeps_a_stale_member_for_preparation() -> Result<(), Box<dyn Error>> {
        let first = resolved("codex", "paid", 17);
        let mut stale = resolved("claude", "stale", 3);
        stale.readiness = RouteReadiness::stale();
        let mut replacement = super::admission_replacement(Ok(stale))?
            .ok_or("stale member was treated as exhaustion")?;
        super::record_pool_redraw(&first, &mut replacement, "admission refused")?;
        assert!(matches!(replacement.readiness, RouteReadiness::Unavailable { reason: provenant_records::RouteExclusionReason::StaleAvailability, .. }));
        assert_eq!(replacement.pool_selection.unwrap().redraws[0].rejected_pick, "codex/paid");
        Ok(())
    }
    // Spec: docs/specs/routing.md#monthly-allowance-ledger
    #[test]
    fn review_preview_requires_local_login_evidence_beyond_allowance_debt() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::DirBuilder::new().mode(0o700).create(&state)?;
        let roots = provenant_core::RootInputs {
            state_dir: Some(state.clone()),
            ..provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"))
        }.resolve(&provenant_os::fs::SystemPathResolver)?;
        let config = state.join("config.toml");
        std::fs::write(&config, "schema_version=1\n[domains.routing.pools]\nlocal=[{model='cursor/composer-2.5',weight=1}]\n")?;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))?;
        let preview = || super::resolve_review_family(&roots, &serde_json::json!("local"), Some("cursor"), &[]);
        assert!(preview().is_err(), "an absent login cannot supply a local family");
        let account = provenant_quota::AccountScope {
            extra: Default::default(), adapter: "cursor".into(), account_key: "local".into(), label: "default".into(),
        };
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let mut record = provenant_quota::AccountRecord::empty(account.clone());
        record.allowance.record_launch("first", now, 1)?;
        let quota = state.join("quota/local");
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&quota)?;
        let quota_path = quota.join("cursor.json");
        std::fs::write(&quota_path, serde_json::to_vec(&record)?)?;
        std::fs::set_permissions(&quota_path, std::fs::Permissions::from_mode(0o600))?;
        assert!(preview().is_err(), "allowance debt cannot supply a local family");

        let readiness = state.join("readiness/local");
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&readiness)?;
        std::fs::set_permissions(state.join("readiness"), std::fs::Permissions::from_mode(0o700))?;
        let readiness_path = readiness.join("cursor.json");
        let mut file = provenant_quota::ReadinessFile::default();
        file.records.push(provenant_quota::RouteReadiness {
            extra: Default::default(), schema_version: 1,
            scope: provenant_quota::ReadinessScope {
                extra: Default::default(), host: provenant_quota::LOCAL_HOST.into(), adapter: "cursor".into(),
                account_key: "local".into(), route: "cursor/composer-2.5".into(),
            },
            state: provenant_quota::ReadinessState::Ready, reason: None,
            observed_unix_seconds: now, expires_unix_seconds: now + 300,
            reset_unix_seconds: None, evidence_ref: Some("fixture:login-status".into()),
        });
        std::fs::write(&readiness_path, serde_json::to_vec(&file)?)?;
        std::fs::set_permissions(&readiness_path, std::fs::Permissions::from_mode(0o600))?;
        let read = crate::quota::read_readiness(&state, &file.records[0].scope, now);
        assert_eq!(read.state, provenant_quota::ReadinessState::Ready, "{read:?}");
        assert_eq!(preview()?, "cursor", "real readiness remains a candidate");
        file.sync_probe_result(&record, provenant_quota::LOCAL_HOST, now, 300,
            Some(provenant_quota::ReadinessReason::ProbeTimeout))?;
        std::fs::write(&readiness_path, serde_json::to_vec(&file)?)?;
        assert!(preview().is_err(), "a published local probe failure still constrains preview");
        std::fs::remove_file(&readiness_path)?;
        record.snapshot = Some(provenant_quota::QuotaSnapshot {
            extra: Default::default(), schema_version: 1, account, revision: 0,
            status: provenant_quota::QuotaStatus::Available,
            observed_unix_seconds: now, source_ref: "fixture".into(), authority_ref: None,
            details: None, windows: Vec::new(),
        });
        std::fs::write(&quota_path, serde_json::to_vec(&record)?)?;
        assert_eq!(preview()?, "cursor", "a provider observation remains a candidate");
        Ok(())
    }
    #[test]
    fn review_uses_unused_families_and_substitutes_when_exhausted() -> Result<(), Box<dyn Error>> {
        assert!(!super::review_family_excluded(None, &[]));
        assert!(super::review_family_excluded(None, &["openai".into()]));
        assert!(!super::review_family_excluded(
            Some("anthropic"),
            &["openai".into()]
        ));
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
            "schema_version=1\n[domains.routing.pools]\ntest=[{model='codex/gpt-6-luna',weight=1,effort='high',efforts=['high']},{model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']}]\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        // Explain-time resolution has no launch grants or fresh availability:
        // unknown readiness must still permit a family present in the pool.
        assert_eq!(super::resolve_review_family(&roots, &serde_json::json!("test"), Some("openai"), &[])?, "openai");
        assert_eq!(super::resolve_review_family(&roots, &serde_json::json!("test"), Some("anthropic"), &[])?, "anthropic");
        let absent = super::resolve_review_family(&roots, &serde_json::json!("test"), Some("missing-family"), &[]).unwrap_err();
        assert!(absent.contains("missing-family") && absent.contains("count 1"), "{absent}");
        let original_config = std::fs::read_to_string(&path)?;
        std::fs::write(&path, format!("{original_config}\n[domains.accounts.entries.first]\nadapter='codex'\nlabel='first'\nkey='first'\ndirectory={:?}\n[domains.accounts.entries.second]\nadapter='codex'\nlabel='second'\nkey='second'\ndirectory={:?}\n", temp.path().join("first"), temp.path().join("second")))?;
        assert_eq!(super::resolve_review_family(&roots, &serde_json::json!("test"), Some("openai"), &["first".into()])?, "openai");
        let blocked = super::resolve_review_family(&roots, &serde_json::json!("test"), Some("openai"), &["first".into(), "second".into()]).unwrap_err();
        assert!(blocked.contains("account codex/first") && blocked.contains("account codex/second"), "{blocked}");
        std::fs::write(&path, original_config)?;
        let catalogue = super::Catalogue::bundled()?;
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let availability: Vec<_> = [("codex", "gpt-6-luna"), ("claude", "claude-haiku-5-5")]
            .into_iter()
            .map(|(adapter, model)| provenant_providers::AvailabilityRecord {
                schema_version: 1,
                adapter: adapter.into(),
                executable: "/fixture/provider".into(),
                version: "fixture".into(),
                fixture_ref: "fixture".into(),
                catalogue_revision: catalogue.revision().into(),
                account_key: Some(format!("{adapter}-account")),
                plan: None,
                observed_models: vec![model.into()],
                observed_unix_seconds: now,
                expires_unix_seconds: now + 300,
                last_update: None,
            })
            .collect();
        let access = [
            provenant_routing::AccessContext {
                adapter: "codex",
                disclosure_ref: "fixture",
                confinement_ref: "fixture",
            },
            provenant_routing::AccessContext {
                adapter: "claude",
                disclosure_ref: "fixture",
                confinement_ref: "fixture",
            },
        ];
        let request = super::RouteRequest {
            read_heavy: false,
            model: None,
            route: Some("test".into()),
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec![],
        }
        .to_domain()?;
        let resolve = |excluded: &[String]| {
            super::resolve_dispatch(
                &request,
                &roots,
                None,
                ("fixture", 0),
                &availability,
                &access,
                "fixture",
                Some((excluded, false, &[])),
                &[],
            )
        };
        // Spec: docs/specs/routing.md#unknown-never-means-ready
        let unresolved = super::resolve_dispatch(
            &request, &roots, None, ("fixture", 0), &[], &access,
            "fixture", None, &[],
        )?;
        assert!(matches!(unresolved.readiness, RouteReadiness::Unavailable {
            reason: provenant_records::RouteExclusionReason::UnknownAvailability, ..
        }));
        let first = resolve(&[])?;
        let first_id =
            provenant_routing::ModelId::parse(&format!("{}/{}", first.adapter, first.model))?;
        let family = catalogue
            .model(&first_id)
            .and_then(|row| row.family.clone())
            .ok_or("family")?;
        let second = resolve(std::slice::from_ref(&family))?;
        let second_id =
            provenant_routing::ModelId::parse(&format!("{}/{}", second.adapter, second.model))?;
        let other = catalogue
            .model(&second_id)
            .and_then(|row| row.family.clone())
            .ok_or("family")?;
        assert_ne!(family, other);
        let all = [family.clone(), other.clone()];
        let refused = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &availability,
            &access,
            "fixture",
            Some((&all, true, &[])),
            &[],
        )
        .err()
        .ok_or("a required separation must not substitute")?;
        assert!(matches!(
            refused,
            super::DispatchError::FamilySeparation(..)
        ));
        // A sibling preference can relax while the writer exclusion remains hard.
        let composed = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &availability,
            &access,
            "fixture",
            Some((
                std::slice::from_ref(&family),
                true,
                std::slice::from_ref(&other),
            )),
            &[],
        )?;
        assert_eq!(composed.adapter, second.adapter);
        // The next family's provider is excluded: a required separation reports
        // none, and an optional one never substitutes the excluded provider.
        let rules = [provenant_routing::ExclusionRule {
            selector: provenant_routing::ExclusionSelector::Provider(second.adapter.clone()),
            source: provenant_routing::ExclusionSource::Flag,
        }];
        let with_rules = |required: bool| {
            super::resolve_dispatch(
                &request,
                &roots,
                None,
                ("fixture", 0),
                &availability,
                &access,
                "fixture",
                Some((std::slice::from_ref(&family), required, &[])),
                &rules,
            )
        };
        assert!(matches!(
            with_rules(true)
                .err()
                .ok_or("excluded family must not substitute")?,
            super::DispatchError::FamilySeparation(..)
        ));
        assert_ne!(with_rules(false)?.adapter, second.adapter);
        let fallback = resolve(&[family, other])?;
        assert!(matches!(
            fallback.readiness,
            provenant_records::RouteReadiness::Ready { .. }
        ));
        // An unobserved unused family reaches preparation, not a ready launch.
        let missing = super::resolve_dispatch(
            &request, &roots, None, ("fixture", 0), &availability[..1],
            &access, "fixture", Some((&["openai".into()], false, &[])), &[],
        )?;
        assert_eq!(missing.adapter, "claude");
        assert!(matches!(missing.readiness, RouteReadiness::Unavailable {
            reason: provenant_records::RouteExclusionReason::UnknownAvailability, ..
        }));
        let mut stale = availability.clone();
        stale[1].observed_unix_seconds = now - 10;
        stale[1].expires_unix_seconds = now - 1;
        // The unused family remains selected when its availability is stale,
        // instead of silently relaxing to the ready, already-used family.
        let deferred = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &stale,
            &access,
            "fixture",
            Some((&["openai".into()], false, &[])),
            &[],
        )?;
        assert_eq!(deferred.adapter, "claude");
        assert_eq!(deferred.account_key, stale[1].account_key);
        // Stale executable evidence warns; it does not erase a known identity.
        assert!(matches!(
            deferred.readiness,
            provenant_records::RouteReadiness::Ready { availability_warning: Some(warning), .. }
                if warning == format!("{:?}", provenant_records::RouteExclusionReason::StaleAvailability)
        ));
        Ok(())
    }

    /// A codex family already used by a review and an unused claude one, each
    /// with a single ready account.
    struct ReviewFixture {
        _temp: tempfile::TempDir,
        roots: provenant_core::Roots,
        availability: Vec<provenant_providers::AvailabilityRecord>,
        access: [provenant_routing::AccessContext<'static>; 2],
        request: provenant_routing::RouteRequest,
        now: i64,
    }
    impl ReviewFixture {
        fn new() -> Result<Self, Box<dyn Error>> {
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
            let mut inputs = provenant_core::RootInputs::new(
                temp.path().join("home"),
                temp.path().join("checkout"),
            );
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
                "schema_version=1\n[domains.routing.pools]\ntest=[{model='codex/gpt-6-luna',weight=1,effort='high',efforts=['high']},{model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']}]\n",
            )?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            let catalogue = super::Catalogue::bundled()?;
            let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
            let availability = [("codex", "gpt-6-luna"), ("claude", "claude-haiku-5-5")]
                .into_iter()
                .map(|(adapter, model)| provenant_providers::AvailabilityRecord {
                    schema_version: 1,
                    adapter: adapter.into(),
                    executable: "/fixture/provider".into(),
                    version: "fixture".into(),
                    fixture_ref: "fixture".into(),
                    catalogue_revision: catalogue.revision().into(),
                    account_key: Some(format!("{adapter}-account")),
                    plan: None,
                    observed_models: vec![model.into()],
                    observed_unix_seconds: now,
                    expires_unix_seconds: now + 300,
                    last_update: None,
                })
                .collect();
            let access = ["codex", "claude"].map(|adapter| provenant_routing::AccessContext {
                adapter,
                disclosure_ref: "fixture",
                confinement_ref: "fixture",
            });
            let request = super::RouteRequest {
                read_heavy: false,
                model: None,
                route: Some("test".into()),
                effort: None,
                confidential: false,
                fallback: vec![],
                exclude: vec![],
            }
            .to_domain()?;
            Ok(Self {
                _temp: temp,
                roots,
                availability,
                access,
                request,
                now,
            })
        }
        /// Resolve with the openai family already used by the review.
        fn resolve(
            &self,
            required: bool,
        ) -> Result<provenant_records::ResolvedRoute, super::DispatchError> {
            let used = [String::from("openai")];
            super::resolve_dispatch(
                &self.request,
                &self.roots,
                None,
                ("fixture", 0),
                &self.availability,
                &self.access,
                "fixture",
                Some((&used, required, &[])),
                &[],
            )
        }
        /// Persist the claude account's record, as the quota cache holds it.
        fn seed_claude(
            &self,
            edit: impl FnOnce(&mut provenant_quota::AccountRecord),
        ) -> Result<(), Box<dyn Error>> {
            self.seed("claude", edit)
        }
        /// Persist one provider account's record, as the quota cache holds it.
        fn seed(
            &self,
            adapter: &str,
            edit: impl FnOnce(&mut provenant_quota::AccountRecord),
        ) -> Result<(), Box<dyn Error>> {
            use std::os::unix::fs::PermissionsExt;
            let key = format!("{adapter}-account");
            let scope = provenant_quota::AccountScope { extra: Default::default(),
                adapter: adapter.into(),
                account_key: key.clone(),
                label: key.clone(),
            };
            let mut record = provenant_quota::AccountRecord::empty(scope);
            edit(&mut record);
            let quota = self.roots.state.as_path().join("quota");
            let account = quota.join(&key);
            std::fs::create_dir_all(&account)?;
            for dir in [&quota, &account] {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
            let file = account.join(format!("{adapter}.json"));
            std::fs::write(&file, serde_json::to_vec(&record)?)?;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
            Ok(())
        }
    }

    #[test]
    fn the_non_graph_run_path_carries_the_flag_into_rules_that_drop_the_provider()
    -> Result<(), Box<dyn Error>> {
        use provenant_routing::{ExclusionSelector, ExclusionSource};
        let fixture = ReviewFixture::new()?;
        // `run --route test --exclude claude` pushes this override; the task's
        // profile is built from it exactly as the dispatch builds it.
        let overrides = vec![format!(
            "routing.flag_exclude={}",
            serde_json::Value::from(vec!["claude"])
        )];
        let profile =
            provenant_config::read_with_domains(&fixture.roots, &crate::domains::SECTIONS)?
                .dispatch_profile(&overrides)?;
        let rules = super::task_exclusion_rules(&profile, &fixture.roots)?;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].source, ExclusionSource::Flag);
        assert_eq!(
            rules[0].selector,
            ExclusionSelector::Provider("claude".into())
        );
        let route = super::resolve_dispatch(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            None,
            &rules,
        )?;
        assert_eq!(route.adapter, "codex");
        let receipt = route.pool_selection.ok_or("pool selection")?;
        assert!(receipt.candidates.iter().any(|candidate| {
            candidate.model == "claude/claude-haiku-5-5"
                && candidate.exclusion.as_deref() == Some("excluded by flag: provider claude")
        }));
        // Required family separation names the exclusions that left no member.
        let used = [String::from("openai")];
        let refused = super::resolve_dispatch(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            Some((&used, true, &[])),
            &rules,
        )
        .unwrap_err();
        assert!(matches!(
            refused,
            super::DispatchError::FamilySeparation(..)
        ));
        assert!(
            refused
                .to_string()
                .contains("excluded by flag: provider claude"),
            "{refused}"
        );
        Ok(())
    }

    // Spec: docs/specs/retry-owner.md#recovery-preserves-work-and-budget
    #[test]
    fn stale_graph_exclusions_do_not_refuse_dispatch() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        let overrides = vec![format!("routing.graph_exclude={}", serde_json::json!(["nonesuch", "claude"]))];
        let profile = provenant_config::read_with_domains(&roots, &crate::domains::SECTIONS)?.dispatch_profile(&overrides)?;
        let rules = super::task_exclusion_rules(&profile, &roots)?;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].selector, provenant_routing::ExclusionSelector::Provider("claude".into()));
        Ok(())
    }

    #[test]
    fn explain_reads_the_same_union_of_standing_and_flag_exclusions() -> Result<(), Box<dyn Error>>
    {
        use provenant_routing::ExclusionSource;
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.exclude'=['codex']\n",
        )?;
        let rules = super::effective_exclusion_rules(&["claude".to_owned()], &roots)?;
        let texts: Vec<_> = rules.iter().map(ToString::to_string).collect();
        assert_eq!(
            texts,
            [
                "excluded by config: provider codex",
                "excluded by flag: provider claude"
            ]
        );
        assert!(
            rules
                .iter()
                .any(|rule| rule.source == ExclusionSource::Config)
        );
        // A bad standing selector is refused the same as on the flag.
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.exclude'=['nosuchprovider']\n",
        )?;
        let error = super::effective_exclusion_rules(&[], &roots).unwrap_err();
        assert!(error.to_string().contains("valid forms"), "{error}");
        // A malformed list is refused rather than recovered to empty.
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.exclude'=['claude', 5]\n",
        )?;
        assert!(super::effective_exclusion_rules(&[], &roots).is_err());
        Ok(())
    }

    #[test]
    fn review_substitution_skips_refused_unused_families() -> Result<(), Box<dyn Error>> {
        let fixture = ReviewFixture::new()?;
        // The used family is the one codex serves, so Claude is the unused one.
        let unused = fixture.resolve(false)?;
        assert_eq!(unused.adapter, "claude");
        assert!(matches!(unused.readiness, RouteReadiness::Ready { .. }));
        // Its only account is refused: review relaxes to the ready, used family
        // rather than picking the refused member.
        crate::quota::persist_provider_refusal(
            fixture.roots.state.as_path(),
            &provenant_records::run::RouteRecord {
                provider: Some("claude".into()),
                model: Some("claude/claude-haiku-5-5".into()),
                account_key: Some("claude-account".into()),
                account_label: Some("claude-account".into()),
                ..Default::default()
            },
            None,
            fixture.now,
            Some(fixture.now + 3_600),
        )?;
        let relaxed = fixture.resolve(false)?;
        assert_eq!(relaxed.adapter, "codex");
        assert!(matches!(relaxed.readiness, RouteReadiness::Ready { .. }));
        // A required separation never relaxes.
        assert!(matches!(
            fixture.resolve(true),
            Err(super::DispatchError::FamilySeparation(..))
        ));
        // Relaxing the sibling preference must not exchange one refused
        // route for another unavailable route.
        crate::quota::persist_provider_refusal(
            fixture.roots.state.as_path(),
            &provenant_records::run::RouteRecord {
                provider: Some("codex".into()),
                model: Some("codex/gpt-6-luna".into()),
                account_key: Some("codex-account".into()),
                account_label: Some("codex-account".into()),
                ..Default::default()
            },
            None,
            fixture.now,
            Some(fixture.now + 3_600),
        )?;
        let selected = super::resolve_dispatch(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            Some((&[], false, &["openai".into()])),
            &[],
        )?;
        assert_eq!(selected.adapter, "claude");
        Ok(())
    }

    #[test]
    fn review_substitution_skips_a_mismatched_unused_family() -> Result<(), Box<dyn Error>> {
        let fixture = ReviewFixture::new()?;
        fixture.seed_claude(|record| {
            record.last_error = Some(provenant_quota::ProbeFailure::AccountMismatch);
        })?;
        let relaxed = fixture.resolve(false)?;
        assert_eq!(relaxed.adapter, "codex");
        assert!(matches!(relaxed.readiness, RouteReadiness::Ready { .. }));
        assert!(matches!(
            fixture.resolve(true),
            Err(super::DispatchError::FamilySeparation(..))
        ));
        Ok(())
    }

    #[test]
    fn review_substitution_only_skips_fresh_snapshot_exhaustion() -> Result<(), Box<dyn Error>> {
        // Fresh exhaustion excludes the unused family, so optional separation
        // relaxes. Stale exhaustion still ranks it without excluding it.
        for stale in [false, true] {
            let fixture = ReviewFixture::new()?;
            let now = fixture.now;
            fixture.seed_claude(|record| {
                let account = record.account.clone();
                record.snapshot = Some(provenant_quota::QuotaSnapshot { extra: Default::default(),
                    schema_version: 1,
                    account,
                    revision: 0,
                    status: provenant_quota::QuotaStatus::Exhausted,
                    observed_unix_seconds: if stale { now - 86_400 } else { now },
                    source_ref: "fixture".into(),
                    authority_ref: None,
                    windows: vec![provenant_quota::QuotaWindow { extra: Default::default(),
                        pool: "seven_day".into(),
                        window_seconds: Some(604_800),
                        used_basis_points: 10_000,
                        resets_unix_seconds: Some(now + 3_600),
                    }],
                    details: None,
                });
            })?;
            let route = fixture.resolve(false)?;
            assert_eq!(route.adapter, if stale { "claude" } else { "codex" });
            assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
            if stale {
                assert_eq!(fixture.resolve(true)?.adapter, "claude");
            } else {
                assert!(matches!(
                    fixture.resolve(true),
                    Err(super::DispatchError::FamilySeparation(..))
                ));
            }
        }
        Ok(())
    }

    /// A one-member `principal` pool, with the member's account record seeded.
    fn blocked_fixture(
        edit: impl FnOnce(&mut provenant_quota::AccountRecord),
    ) -> Result<ReviewFixture, Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        let fixture = ReviewFixture::new()?;
        let path = fixture.roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\nprincipal=[{model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']}]\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        fixture.seed_claude(edit)?;
        Ok(fixture)
    }

    #[test]
    fn route_blocked_is_quiet_for_a_usable_route() -> Result<(), Box<dyn Error>> {
        let fixture = blocked_fixture(|_| {})?;
        assert_eq!(super::route_blocked("principal", &[], &fixture.roots), None);
        Ok(())
    }

    #[test]
    fn route_blocked_names_an_excluded_principal() -> Result<(), Box<dyn Error>> {
        let fixture = blocked_fixture(|_| {})?;
        let flag = super::route_blocked("principal", &["claude".to_owned()], &fixture.roots);
        assert!(flag.is_some(), "{flag:?}");
        // A configured exclusion blocks the same way.
        std::fs::write(
            fixture.roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.exclude'=['claude']\n[domains.routing.pools]\nprincipal=[{model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']}]\n",
        )?;
        let configured = super::route_blocked("principal", &[], &fixture.roots);
        assert!(configured.is_some(), "{configured:?}");
        Ok(())
    }

    /// Mark the account's allowance spent until it resets in an hour.
    fn exhaust(record: &mut provenant_quota::AccountRecord) {
        let account = record.account.clone();
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        record.snapshot = Some(provenant_quota::QuotaSnapshot { extra: Default::default(),
            schema_version: 1,
            account,
            revision: 0,
            status: provenant_quota::QuotaStatus::Exhausted,
            observed_unix_seconds: now,
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![provenant_quota::QuotaWindow { extra: Default::default(),
                pool: "seven_day".into(),
                window_seconds: Some(604_800),
                used_basis_points: 10_000,
                resets_unix_seconds: Some(now + 3_600),
            }],
            details: None,
        });
    }

    /// A two-member `principal` pool, one member per provider, each with its
    /// account record seeded. Whichever the resolver picks first, the other
    /// is the retry.
    fn pair_fixture(
        claude: impl FnOnce(&mut provenant_quota::AccountRecord),
        codex: impl FnOnce(&mut provenant_quota::AccountRecord),
    ) -> Result<ReviewFixture, Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        let fixture = ReviewFixture::new()?;
        let path = fixture.roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\nprincipal=[{model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']},{model='codex/gpt-6-luna',weight=1,effort='high',efforts=['high']}]\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        fixture.seed("claude", claude)?;
        fixture.seed("codex", codex)?;
        Ok(fixture)
    }

    #[test]
    fn route_blocked_looks_past_an_exhausted_member_to_a_usable_one() -> Result<(), Box<dyn Error>>
    {
        // Dispatch retries the next member, so either one spent is no block.
        for spent_claude in [true, false] {
            let fixture = pair_fixture(
                |record| {
                    if spent_claude {
                        exhaust(record)
                    }
                },
                |record| {
                    if !spent_claude {
                        exhaust(record)
                    }
                },
            )?;
            assert_eq!(
                super::route_blocked("principal", &[], &fixture.roots),
                None,
                "claude spent: {spent_claude}"
            );
        }
        Ok(())
    }

    #[test]
    fn route_blocked_names_every_member_when_all_are_exhausted() -> Result<(), Box<dyn Error>> {
        let fixture = pair_fixture(exhaust, exhaust)?;
        let reason = super::route_blocked("principal", &[], &fixture.roots)
            .ok_or("a pool with every member spent must block")?;
        assert!(reason.contains("claude/claude-haiku-5-5"), "{reason}");
        assert!(reason.contains("codex/gpt-6-luna"), "{reason}");
        Ok(())
    }

    #[test]
    fn route_blocked_ends_for_a_single_model_route_with_every_account_spent()
    -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        let fixture = blocked_fixture(exhaust)?;
        let path = fixture.roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.routes.solo]\nmodel='claude/claude-haiku-5-5'\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        // A resolver that returns the same model for ever would hang this
        // call, so bound it.
        let (sender, receiver) = std::sync::mpsc::channel();
        let roots = fixture.roots.clone();
        std::thread::spawn(move || {
            let _ = sender.send(super::route_blocked("solo", &[], &roots));
        });
        let blocked = receiver
            .recv_timeout(std::time::Duration::from_secs(20))
            .map_err(|_| "route_blocked did not end")?;
        let reason = blocked.ok_or("a spent single-model route must block")?;
        assert!(reason.contains("allowance exhausted"), "{reason}");
        assert_eq!(
            reason.matches("claude/claude-haiku-5-5").count(),
            1,
            "{reason}"
        );
        Ok(())
    }

    #[test]
    fn route_resolve_shows_evidence_from_settled_attempts() -> Result<(), Box<dyn Error>> {
        use provenant_core::{Clock, CancelToken, RootInputs};
        use provenant_os::clock::SystemClock;
        use provenant_records::route_selection::{Exploration, PriorSource, SelectionRule, WeightSource};
        use provenant_run as run;
        use std::os::unix::fs::PermissionsExt;
        use super::{config::execute_in, Data, Route, RouteResolve};

        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        let project = base.join("project");
        let state = base.join("state");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(&state)?;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;

        let mut inputs = RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(state);
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let config_path = roots.state.as_path().join("config.toml");
        std::fs::write(
            &config_path,
            "schema_version=1\n[domains.routing.pools]\nprincipal=[\n  {model='codex/gpt-6-luna',weight=1,effort='high',efforts=['high']},\n  {model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']}\n]\n",
        )?;
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600))?;

        provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(project.clone())?,
        )?;

        let context = crate::fabric::Context::open(roots.clone(), &home, &project)?;
        let project_id = context.identity.project;
        let actor_id = context.identity.inbox.actor;

        let clock = SystemClock::new();
        let make_query = || RouteResolve {
            writes: false,
            account: None,
            name: Some("principal".into()),
            request: super::RouteRequest {
                read_heavy: false,
                model: None,
                route: None,
                effort: None,
                confidential: false,
                fallback: vec![],
                exclude: vec![],
            },
        };

        // Before any attempts: evidence count is 0, source is NoEvidence
        let Data::Resolved(initial) = execute_in(&Route::Resolve(make_query()), &roots, clock.wall_now()?, &home, &project)? else {
            return Err("expected resolved route".into());
        };
        let luna_initial = initial
            .route
            .pool_selection
            .as_ref()
            .ok_or("pool selection receipt")?
            .candidates
            .iter()
            .find(|c| c.model == "codex/gpt-6-luna")
            .ok_or("luna candidate")?;
        let initial_prior = luna_initial.ranking.outcome_prior.as_ref().ok_or("outcome prior")?;
        assert_eq!(initial_prior.evidence_count, 0);
        assert_eq!(initial_prior.source, PriorSource::NoEvidence);

        let cancel = CancelToken::new();
        let run_call = run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let exec = run::ExecutionContext {
            state_root: roots.state.clone(),
            project_id,
            actor_id,
            read_prompts: false,
        };
        let store = run::RunStore::open(&exec, &run_call)?;

        let make_receipt = || provenant_records::RouteSelectionReceipt {
            selection_rule: SelectionRule::WeightedDraw,
            considered_exclusions: vec![],
            exploration: Exploration {
                extra: Default::default(),
                selected: false,
                percent: 0,
                enabled: false,
                source: "default".into(),
            },
            weight_source: WeightSource::Config,
            account_load: None,
            extra: Default::default(),
            pool: "principal".into(),
            candidates: vec![],
            roll: 0,
            total_weight: 1,
            pick: "codex/gpt-6-luna".into(),
            reason: None,
            redraws: vec![],
        };
        let make_route = || run::RouteRecord {
            selection_context: Some(provenant_records::route_selection::RouteAttemptContext {
                extra: Default::default(),
                workload: provenant_routing::Workload::General,
                writes: false,
            }),
            provider: Some("codex".into()),
            model: Some("codex/gpt-6-luna".into()),
            effort: Some("high".into()),
            account_label: Some("codex-account".into()),
            pool_selection: Some(make_receipt()),
            ..Default::default()
        };

        let settle_attempt = |attempt_num: u8, succeeded: bool| -> Result<(), Box<dyn Error>> {
            let attempt_id = provenant_core::AttemptId::from_bytes([attempt_num; 16]);
            let mut request: run::CanonicalRequest = serde_json::from_value(serde_json::json!({
                "schema_version": 1,
                "task": {
                    "prompt_digest": provenant_records::CanonicalV1::file_digest(b"task"),
                    "route": {
                        "kind": "model",
                        "provider": "codex",
                        "model": "codex/gpt-6-luna",
                        "effort": "high"
                    },
                    "authority_digest": provenant_core::Digest::from_bytes([1; 32]),
                    "provider_timeout_seconds": 30,
                    "delivery_id": null
                },
                "parent": null,
                "lane_depth": 1
            }))?;
            request.task.context = Some(provenant_records::run::WorkContext {
                graph_id: Some(format!("lane-{attempt_num}")),
                node_id: Some("node".into()),
                attempt: Some(1),
                role: None,
                ..Default::default()
            });
            let prepared = run::PreparedRun {
                attempt_id,
                operation_id: provenant_core::OperationId::parse(&format!("op-{attempt_num}"))?,
                created_at: clock.wall_now()?,
                request,
                prompt: "task".into(),
            };
            store.create(&prepared, &run_call)?;

            let code = if succeeded { 0 } else { 1 };
            let exit_obs = provenant_supervisor::custody::ExitObservation {
                cancel_delivered_before_exit: None,
                provider_identity_conflict: false,
                provider_identity_evidence_incomplete: false,
                provider_identity_eof_fragment: false,
                publication: provenant_supervisor::custody::PublicationState::Durable,
                scope: provenant_records::custody::Scope {
                    attempt: attempt_id,
                    generation: 0,
                },
                termination: Some(provenant_os::exec::Termination::Exit(code)),
                never_spawned: false,
                stop_reason: None,
                delivered_signals: vec![],
                reaped: vec![],
                omitted_reaped: 0,
                spared: vec![],
                omitted_spared: 0,
                launch_refusal: None,
                warnings: vec![],
                omitted_warnings: 0,
                terminal_observed: false,
                unresolved_cleanup: false,
                unresolved_pids: vec![],
                settlement_evidence: provenant_supervisor::custody::SettlementEvidence::DarwinCensusAndCustodyLease,
                stdout: Default::default(),
                stderr: Default::default(),
            };
            let mut exit_val = serde_json::to_value(&exit_obs)?;
            let exit_obj = exit_val.as_object_mut().ok_or("exit fixture")?;
            exit_obj.remove("publication");
            exit_obj.insert("schema_version".into(), 1.into());
            let exit_path = roots
                .state
                .as_path()
                .join("attempts")
                .join(attempt_id.to_string())
                .join("process-exit.json");
            std::fs::write(&exit_path, serde_json::to_vec(&exit_val)?)?;

            let route = make_route();
            let finished_at = clock.wall_now()?;
            let publication = run::TerminalPublication {
                handoff: Ok(None),
                terminal_cause: None,
                terminal_note: None,
                outcome: if succeeded {
                    run::Outcome::Succeeded
                } else {
                    run::Outcome::Failed
                },
                finished_at,
                started_at: Some(finished_at),
                provider_ms: Some(100),
                reason: None,
                result: if succeeded {
                    "answer\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[],\"note\":\"finished\"}\n```"
                } else {
                    "failed"
                },
                complete: succeeded,
                redacted: false,
                resolved_route: Some(route.clone()),
                applied_route: Some(route.clone()),
                observed_route: Some(route),
                usage: run::UsageRecord::default(),
                session: None,
                partial_signals: Vec::new(),
            };
            store.publish_terminal(attempt_id, 1, &publication, &exit_obs, &run_call)?;
            Ok(())
        };

        // Settle first attempt: success
        settle_attempt(1, true)?;

        let Data::Resolved(after_success) = execute_in(&Route::Resolve(make_query()), &roots, clock.wall_now()?, &home, &project)? else {
            return Err("expected resolved route".into());
        };
        let luna_success = after_success
            .route
            .pool_selection
            .as_ref()
            .ok_or("pool selection receipt")?
            .candidates
            .iter()
            .find(|c| c.model == "codex/gpt-6-luna")
            .ok_or("luna candidate")?;
        let success_prior = luna_success.ranking.outcome_prior.as_ref().ok_or("outcome prior")?;
        assert_eq!(success_prior.evidence_count, 1, "selection evidence: {:?}", super::selection_evidence(&roots, &home, &project, None));
        assert_eq!(success_prior.source, PriorSource::RouteOutcomes);
        let success_ev = success_prior.evidence.as_ref().ok_or("evidence")?;
        assert_eq!(success_ev.successes, 1);
        assert_eq!(success_ev.failures, 0);
        let success_basis_points = success_prior.factor_basis_points;

        // Settle second attempt: failure
        settle_attempt(2, false)?;

        let Data::Resolved(after_failure) = execute_in(&Route::Resolve(make_query()), &roots, clock.wall_now()?, &home, &project)? else {
            return Err("expected resolved route".into());
        };
        let luna_failure = after_failure
            .route
            .pool_selection
            .as_ref()
            .ok_or("pool selection receipt")?
            .candidates
            .iter()
            .find(|c| c.model == "codex/gpt-6-luna")
            .ok_or("luna candidate")?;
        let failure_prior = luna_failure.ranking.outcome_prior.as_ref().ok_or("outcome prior")?;
        assert_eq!(failure_prior.evidence_count, 2);
        assert_eq!(failure_prior.source, PriorSource::RouteOutcomes);
        let failure_ev = failure_prior.evidence.as_ref().ok_or("evidence")?;
        assert_eq!(failure_ev.successes, 1);
        assert_eq!(failure_ev.failures, 1);
        let failure_basis_points = failure_prior.factor_basis_points;

        assert!(
            failure_basis_points < success_basis_points,
            "failure must move factor down: {failure_basis_points} < {success_basis_points}"
        );

        Ok(())
    }
