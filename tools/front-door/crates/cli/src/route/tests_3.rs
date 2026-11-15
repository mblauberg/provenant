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
        assert!(matches!(
            replacement.readiness,
            RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::StaleAvailability,
                ..
            }
        ));
        assert_eq!(
            replacement.pool_selection.unwrap().redraws[0].rejected_pick,
            "codex/paid"
        );
        Ok(())
    }
    #[test]
    fn review_family_exclusion_tracks_whether_a_family_was_already_used() {
        for (requested, used, excluded) in [
            (None, Vec::new(), false),
            (None, vec![String::from("openai")], true),
            (Some("anthropic"), vec![String::from("openai")], false),
        ] {
            assert_eq!(super::review_family_excluded(requested, &used), excluded);
        }
    }

    #[test]
    fn review_family_resolution_uses_pool_and_account_evidence() -> Result<(), Box<dyn Error>> {
        let fixture = ReviewFixture::new()?;
        let pool = serde_json::json!("test");
        assert_eq!(
            super::resolve_review_family_at(
                &fixture.roots,
                &pool,
                Some("openai"),
                &[],
                route_test_time()
            )?,
            "openai"
        );
        assert_eq!(
            super::resolve_review_family_at(
                &fixture.roots,
                &pool,
                Some("anthropic"),
                &[],
                route_test_time()
            )?,
            "anthropic"
        );
        assert!(
            super::resolve_review_family_at(
                &fixture.roots,
                &pool,
                Some("missing-family"),
                &[],
                route_test_time()
            )
            .is_err()
        );

        let path = fixture.roots.state.as_path().join("config.toml");
        let original = std::fs::read_to_string(&path)?;
        std::fs::write(
            &path,
            format!(
                "{original}\n[domains.accounts.entries.first]\nadapter='codex'\nlabel='first'\nkey='first'\ndirectory={:?}\n[domains.accounts.entries.second]\nadapter='codex'\nlabel='second'\nkey='second'\ndirectory={:?}\n",
                fixture._temp.path().join("first"),
                fixture._temp.path().join("second"),
            ),
        )?;
        assert_eq!(
            super::resolve_review_family_at(
                &fixture.roots,
                &pool,
                Some("openai"),
                &["first".into()],
                route_test_time()
            )?,
            "openai"
        );
        assert!(
            super::resolve_review_family_at(
                &fixture.roots,
                &pool,
                Some("openai"),
                &["first".into(), "second".into()],
                route_test_time()
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn review_dispatch_prefers_unused_families_and_requires_separation_when_possible()
    -> Result<(), Box<dyn Error>> {
        let fixture = ReviewFixture::new()?;
        let catalogue = super::Catalogue::bundled()?;
        let route_family = |route: &provenant_records::ResolvedRoute| {
            let id =
                provenant_routing::ModelId::parse(&format!("{}/{}", route.adapter, route.model))?;
            catalogue
                .model(&id)
                .and_then(|row| row.family.clone())
                .ok_or("family")
        };
        let selected = fixture.resolve(false)?;
        let family = route_family(&selected)?;
        assert_ne!(family, "openai");
        assert!(matches!(selected.readiness, RouteReadiness::Ready { .. }));

        let used = [String::from("openai"), family.clone()];
        let relaxed = super::resolve_dispatch_at(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            Some((&used, false, &[])),
            &[],
            route_test_time(),
        )?;
        assert!(matches!(relaxed.readiness, RouteReadiness::Ready { .. }));
        assert!(matches!(
            super::resolve_dispatch_at(
                &fixture.request,
                &fixture.roots,
                None,
                ("fixture", 0),
                &fixture.availability,
                &fixture.access,
                "fixture",
                Some((&used, true, &[])),
                &[],
                route_test_time()
            ),
            Err(super::DispatchError::FamilySeparation(..))
        ));
        Ok(())
    }

    #[test]
    fn review_dispatch_composes_family_requirements_with_writer_exclusions()
    -> Result<(), Box<dyn Error>> {
        let fixture = ReviewFixture::new()?;
        let catalogue = super::Catalogue::bundled()?;
        let route_family = |route: &provenant_records::ResolvedRoute| {
            let id =
                provenant_routing::ModelId::parse(&format!("{}/{}", route.adapter, route.model))?;
            catalogue
                .model(&id)
                .and_then(|row| row.family.clone())
                .ok_or("family")
        };
        let first = fixture.resolve(false)?;
        let family = route_family(&first)?;
        let used = [family.clone()];
        let second = super::resolve_dispatch_at(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            Some((&used, false, &[])),
            &[],
            route_test_time(),
        )?;
        let other = route_family(&second)?;
        assert_ne!(family, other);
        let composed = super::resolve_dispatch_at(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            Some((&used, true, std::slice::from_ref(&other))),
            &[],
            route_test_time(),
        )?;
        assert_eq!(composed.adapter, second.adapter);

        let rules = [provenant_routing::ExclusionRule {
            selector: provenant_routing::ExclusionSelector::Provider(second.adapter.clone()),
            source: provenant_routing::ExclusionSource::Flag,
        }];
        let with_rules = |required: bool| {
            super::resolve_dispatch_at(
                &fixture.request,
                &fixture.roots,
                None,
                ("fixture", 0),
                &fixture.availability,
                &fixture.access,
                "fixture",
                Some((&used, required, &[])),
                &rules,
                route_test_time(),
            )
        };
        assert!(matches!(
            with_rules(true),
            Err(super::DispatchError::FamilySeparation(..))
        ));
        assert_ne!(with_rules(false)?.adapter, second.adapter);
        Ok(())
    }

    #[test]
    fn review_dispatch_keeps_a_stale_unused_family_for_preparation() -> Result<(), Box<dyn Error>> {
        let fixture = ReviewFixture::new()?;
        let mut stale_claude = fixture.availability[1].clone();
        stale_claude.observed_unix_seconds = fixture.now - 10;
        stale_claude.expires_unix_seconds = fixture.now - 1;
        let availability = [fixture.availability[0].clone(), stale_claude];
        let deferred = super::resolve_dispatch_at(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &availability,
            &fixture.access,
            "fixture",
            Some((&["openai".into()], false, &[])),
            &[],
            route_test_time(),
        )?;
        assert_eq!(route_family_id(&deferred), "claude");
        assert!(matches!(
            deferred.readiness,
            RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::StaleAvailability,
                ..
            }
        ));
        Ok(())
    }

    fn route_family_id(route: &provenant_records::ResolvedRoute) -> &str {
        &route.adapter
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
            let now = 100_i64;
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
                    expires_unix_seconds: i64::MAX,
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
            super::resolve_dispatch_at(
                &self.request,
                &self.roots,
                None,
                ("fixture", 0),
                &self.availability,
                &self.access,
                "fixture",
                Some((&used, required, &[])),
                &[],
                route_test_time(),
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
        let route = super::resolve_dispatch_at(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            None,
            &rules,
            route_test_time(),
        )?;
        assert_eq!(route.adapter, "codex");
        let receipt = route.pool_selection.ok_or("pool selection")?;
        assert!(receipt.candidates.iter().any(|candidate| {
            candidate.model == "claude/claude-haiku-5-5"
                && candidate.exclusion.as_deref() == Some("excluded by flag: provider claude")
        }));
        // Required family separation names the exclusions that left no member.
        let used = [String::from("openai")];
        let refused = super::resolve_dispatch_at(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            Some((&used, true, &[])),
            &rules,
            route_test_time(),
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
        use provenant_routing::{ExclusionSelector, ExclusionSource};
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.exclude'=['codex']\n",
        )?;
        let rules = super::effective_exclusion_rules(&["claude".to_owned()], &roots)?;
        for (selector, source) in [
            ("codex", ExclusionSource::Config),
            ("claude", ExclusionSource::Flag),
        ] {
            assert!(rules.iter().any(|rule| {
                rule.source == source
                    && matches!(&rule.selector, ExclusionSelector::Provider(provider) if provider == selector)
            }));
        }
        assert!(rules.iter().all(|rule| matches!(
            (&rule.selector, rule.source),
            (ExclusionSelector::Provider(provider), ExclusionSource::Config) if provider == "codex"
        ) || matches!(
            (&rule.selector, rule.source),
            (ExclusionSelector::Provider(provider), ExclusionSource::Flag) if provider == "claude"
        )));
        // A bad standing selector is refused the same as on the flag.
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.exclude'=['nosuchprovider']\n",
        )?;
        assert!(super::effective_exclusion_rules(&[], &roots).is_err());
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
        let selected = super::resolve_dispatch_at(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            Some((&[], false, &["openai".into()])),
            &[],
            route_test_time(),
        )?;
        assert_eq!(selected.adapter, "claude");
        assert!(matches!(selected.readiness, RouteReadiness::Ready { .. }));
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
        assert_eq!(
            super::route_blocked_at("principal", &[], &fixture.roots, route_test_time()),
            None
        );
        Ok(())
    }

    #[test]
    fn route_blocked_names_an_excluded_principal() -> Result<(), Box<dyn Error>> {
        let fixture = blocked_fixture(|_| {})?;
        let flag = super::route_blocked_at(
            "principal",
            &["claude".to_owned()],
            &fixture.roots,
            route_test_time(),
        );
        assert!(flag.is_some(), "{flag:?}");
        // A configured exclusion blocks the same way.
        std::fs::write(
            fixture.roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.exclude'=['claude']\n[domains.routing.pools]\nprincipal=[{model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']}]\n",
        )?;
        let configured =
            super::route_blocked_at("principal", &[], &fixture.roots, route_test_time());
        assert!(configured.is_some(), "{configured:?}");
        Ok(())
    }

    /// Mark the account's allowance spent until it resets in an hour.
    fn exhaust(record: &mut provenant_quota::AccountRecord) {
        let account = record.account.clone();
        let now = 100_i64;
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
                super::route_blocked_at("principal", &[], &fixture.roots, route_test_time()),
                None,
                "claude spent: {spent_claude}"
            );
        }
        Ok(())
    }

    #[test]
    fn route_blocked_names_every_member_when_all_are_exhausted() -> Result<(), Box<dyn Error>> {
        let fixture = pair_fixture(exhaust, exhaust)?;
        let reason = super::route_blocked_at("principal", &[], &fixture.roots, route_test_time())
            .ok_or("a pool with every member spent must block")?;
        assert!(reason.contains("claude/claude-haiku-5-5"), "{reason}");
        assert!(reason.contains("codex/gpt-6-luna"), "{reason}");
        Ok(())
    }

    #[test]
    fn route_blocked_ends_for_a_single_model_route_with_every_account_spent()
    -> Result<(), Box<dyn Error>> {
        let fixture = blocked_fixture(exhaust)?;
        let path = fixture.roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.routes.solo]\nmodel='claude/claude-haiku-5-5'\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let blocked = super::route_blocked_at("solo", &[], &fixture.roots, route_test_time());
        assert!(blocked.is_some());
        Ok(())
    }
