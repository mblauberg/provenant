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
        // The unused family remains selected when its availability is stale,
        // instead of silently relaxing to the ready, already-used family.
        let deferred = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &availability[..1],
            &access,
            "fixture",
            Some((&["openai".into()], false, &[])),
            &[],
        )?;
        assert_eq!(deferred.adapter, "claude");
        assert!(matches!(
            deferred.readiness,
            provenant_records::RouteReadiness::Unavailable { .. }
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
            let scope = provenant_quota::AccountScope {
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
                record.snapshot = Some(provenant_quota::QuotaSnapshot {
                    schema_version: 1,
                    account,
                    revision: 0,
                    status: provenant_quota::QuotaStatus::Exhausted,
                    observed_unix_seconds: if stale { now - 86_400 } else { now },
                    source_ref: "fixture".into(),
                    authority_ref: None,
                    windows: vec![provenant_quota::QuotaWindow {
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
        record.snapshot = Some(provenant_quota::QuotaSnapshot {
            schema_version: 1,
            account,
            revision: 0,
            status: provenant_quota::QuotaStatus::Exhausted,
            observed_unix_seconds: now,
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![provenant_quota::QuotaWindow {
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
    fn admission_redraw_onto_an_unready_member_is_returned_for_preparation()
    -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        // The opencode Go member is ready to the resolver, but the monthly
        // allowance floor refuses it only at admission. The other member has
        // no availability, so the redraw lands on an unready route.
        let fixture = ReviewFixture::new()?;
        let path = fixture.roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.opencode_go]\nmin_monthly_allowance_usd=100\n[domains.routing.pools]\nprincipal=[{model='opencode/opencode-go/deepseek-v4.1-flash',weight=1,efforts=[]},{model='codex/gpt-6-luna',weight=1,effort='high',efforts=['high']}]\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let opencode = provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "opencode".into(),
            executable: "/fixture/provider".into(),
            version: "fixture".into(),
            fixture_ref: "fixture".into(),
            catalogue_revision: super::Catalogue::bundled()?.revision().into(),
            account_key: Some("opencode-account".into()),
            observed_models: vec!["opencode-go/deepseek-v4.1-flash".into()],
            observed_unix_seconds: fixture.now,
            expires_unix_seconds: fixture.now + 300,
            last_update: None,
        };
        let access = [
            provenant_routing::AccessContext {
                adapter: "opencode",
                disclosure_ref: "fixture",
                confinement_ref: "fixture",
            },
            provenant_routing::AccessContext {
                adapter: "codex",
                disclosure_ref: "fixture",
                confinement_ref: "fixture",
            },
        ];
        let request = super::RouteRequest {
            read_heavy: false,
            model: None,
            route: Some("principal".into()),
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec![],
        }
        .to_domain()?;
        let route = super::resolve_dispatch_excluding(
            &request,
            &fixture.roots,
            super::REGISTRY,
            None,
            ("fixture", 0),
            std::slice::from_ref(&opencode),
            &access,
            "fixture",
            &[],
            &[],
            None,
        )?;
        // The redraw is not an error and not a Ready route: preparation
        // re-probes the unready member and decides whether it may launch.
        assert_eq!(route.adapter, "codex");
        assert!(
            matches!(route.readiness, RouteReadiness::Unavailable { .. }),
            "{:?}",
            route.readiness
        );
        let receipt = route.pool_selection.ok_or("pool selection")?;
        assert_eq!(receipt.redraws.len(), 1, "{receipt:?}");
        assert_eq!(
            receipt.redraws[0].rejected_pick,
            "opencode/opencode-go/deepseek-v4.1-flash"
        );
        assert_eq!(
            receipt.redraws[0].extra.get("cause"),
            Some(&serde_json::json!("admission_refused"))
        );
        Ok(())
    }
