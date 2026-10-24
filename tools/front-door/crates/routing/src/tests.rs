use super::*;
use provenant_config::{DomainConfig, DomainSections};
use provenant_providers::{AvailabilityRecord, REGISTRY};
use provenant_records::{RouteEligibility, RouteExclusionReason, RouteReadiness};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn now() -> Result<provenant_core::WallTime, Box<dyn std::error::Error>> {
    Ok(provenant_core::WallTime::from_micros(100_000_000)?)
}
fn context<'a>(
    catalogue: &'a Catalogue,
    config: &'a RoutingConfig,
    now: provenant_core::WallTime,
) -> RoutingContext<'a> {
    RoutingContext {
        catalogue,
        config,
        registry: REGISTRY,
        availability: &[],
        quota: &[],
        access: &[],
        now,
        host: "test-host",
    }
}
fn request(model: &str) -> Result<RouteRequest, RoutingError> {
    Ok(RouteRequest {
        choice: RouteChoice::Model(ModelId::parse(model)?),
        effort: None,
        confidential: false,
        allow_free: false,
        fallback: Fallback::None,
    })
}
#[test]
fn model_selector_keeps_the_entire_provider_path() -> Result<(), RoutingError> {
    let id = ModelId::parse("opencode/openrouter/~anthropic/claude-haiku-latest")?;
    assert_eq!(id.adapter(), "opencode");
    assert_eq!(id.model(), "openrouter/~anthropic/claude-haiku-latest");
    for text in [
        "",
        "codex/",
        "/id",
        "codex",
        "codex/id\n",
        "codex/id#high",
        "codex/id,other",
    ] {
        assert!(ModelId::parse(text).is_err(), "{text:?}");
    }
    assert!(ModelId::parse(&format!("codex/{}", "x".repeat(513))).is_err());
    Ok(())
}
#[test]
fn exact_unknown_is_retained_but_never_upgraded_or_activated()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let route = resolve(&request("codex/gpt-6.1-sol")?, &context)?;
    assert_eq!(route.model, "gpt-6.1-sol");
    assert_eq!(route.effort, None);
    assert_eq!(route.eligibility, RouteEligibility::UnverifiedModel);
    assert!(matches!(
        route.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownAvailability
        }
    ));
    assert_eq!(route.exclusions.len(), 1);
    assert!(route.alternatives.as_slice().is_empty());
    assert!(matches!(
        resolve(&request("invented/id")?, &context),
        Err(RoutingError::UnknownAdapter)
    ));
    Ok(())
}
#[test]
fn named_routes_are_exact_with_explicit_effort_override_and_no_default()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    let model = ModelId::parse("codex/fixture-model")?;
    catalogue.models.push(ModelEntry {
        model: model.clone(),
        efforts: vec!["low".into(), "high".into()],
        evidence_ref: "synthetic-controls".into(),
        family: None,
        trains_on_prompts: None,
    });
    let config = RoutingConfig {
        routes: BTreeMap::from([(
            "review".into(),
            NamedRoute {
                model,
                effort: Some("low".into()),
            },
        )]),
    };
    let context = context(&catalogue, &config, now()?);
    let mut query = RouteRequest {
        choice: RouteChoice::NamedRoute("review".into()),
        effort: None,
        confidential: false,
        allow_free: false,
        fallback: Fallback::None,
    };
    assert_eq!(resolve(&query, &context)?.effort.as_deref(), Some("low"));
    query.effort = Some("high".into());
    assert_eq!(resolve(&query, &context)?.effort.as_deref(), Some("high"));
    query.effort = Some("max".into());
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::UnsupportedEffort)
    ));
    query.choice = RouteChoice::NamedRoute("typo".into());
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::UnknownRoute)
    ));
    Ok(())
}
#[test]
fn utility_pool_resolves_and_confidential_skips_training_members()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    catalogue
        .models
        .iter_mut()
        .find(|row| row.model.as_str() == "claude/claude-haiku-5-5")
        .expect("utility member is catalogued")
        .trains_on_prompts = Some(true);
    let config = RoutingConfig::default();
    let mut context = context(&catalogue, &config, now()?);
    let mut query = RouteRequest {
        choice: RouteChoice::NamedRoute("utility".into()),
        effort: None,
        confidential: true,
        allow_free: false,
        fallback: Fallback::Models(BoundedModels::new(vec![
            ModelId::parse("claude/claude-haiku-5-5")?,
            ModelId::parse("codex/unclassified")?,
        ])?),
    };
    let route = resolve(&query, &context)?;
    assert_eq!(route.model, "gpt-6-luna");
    assert!(route.alternatives.as_slice().is_empty());
    query.fallback = Fallback::None;

    query.confidential = false;
    assert_eq!(resolve(&query, &context)?.model, "claude-haiku-5-5");
    let registry = [
        provenant_providers::Registration {
            adapter: &provenant_providers::claude::Claude,
            quota_source: None,
        },
        provenant_providers::Registration {
            adapter: &RecordedFixture(&provenant_providers::codex::Codex),
            quota_source: None,
        },
    ];
    let mut records = [availability()];
    records[0].observed_models = vec!["gpt-6-luna".into()];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    context.registry = &registry;
    context.availability = &records;
    context.access = &access;
    let route = resolve(&query, &context)?;
    assert_eq!(route.model, "gpt-6-luna");
    assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
    Ok(())
}

#[test]
fn utility_requires_free_opt_in_and_keeps_haiku_low_effort()
-> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let query = serde_json::json!({"choice":{"kind":"named_route","value":"utility"},"effort":null,"confidential":true});
    let request: RouteRequest = serde_json::from_value(query.clone())?;
    assert_eq!(
        resolve(&request, &context(&catalogue, &config, now()?))?
            .effort
            .as_deref(),
        Some("low")
    );
    // Confidential callers cannot use the paid members in this fixture.
    for row in &mut catalogue.models {
        if row.model.adapter() != "opencode" {
            row.trains_on_prompts = Some(true);
        }
    }
    assert!(resolve(&request, &context(&catalogue, &config, now()?)).is_err());
    let mut query = query;
    query["allow_free"] = serde_json::json!(true);
    let request: RouteRequest = serde_json::from_value(query)?;
    assert_eq!(
        resolve(&request, &context(&catalogue, &config, now()?))?.model,
        "opencode/deepseek-v4.1-flash-free"
    );
    Ok(())
}

#[test]
fn alternatives_are_bounded_distinct_and_keep_the_callers_order()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let mut query = request("codex/primary")?;
    query.fallback = Fallback::Models(BoundedModels::new(vec![
        ModelId::parse("agy/second")?,
        ModelId::parse("claude/third")?,
    ])?);
    let route = resolve(&query, &context)?;
    assert_eq!(route.adapter, "codex");
    assert_eq!(
        route
            .alternatives
            .as_slice()
            .iter()
            .map(|a| a.model.as_str())
            .collect::<Vec<_>>(),
        ["second", "third"]
    );
    assert_eq!(route.exclusions.len(), 3);
    assert!(
        route
            .alternatives
            .as_slice()
            .iter()
            .all(|a| matches!(a.readiness, RouteReadiness::Unavailable { .. }))
    );
    query.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse("codex/primary")?])?);
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::Alternatives)
    ));
    assert!(
        BoundedModels::<2>::new(vec![ModelId::parse("codex/a")?, ModelId::parse("codex/a")?])
            .is_err()
    );
    assert!(
        serde_json::from_str::<BoundedModels<2>>(r#"["codex/a","codex/b","codex/c"]"#).is_err()
    );
    assert!(
        serde_json::from_str::<RouteRequest>(
            r#"{"choice":{"kind":"model","value":"codex/a"},"effort":null,"pool":"principal"}"#
        )
        .is_err()
    );
    Ok(())
}
#[test]
fn every_alternative_validates_its_own_effort() -> Result<(), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    catalogue.models.push(ModelEntry {
        model: ModelId::parse("codex/test")?,
        efforts: vec!["high".into()],
        evidence_ref: "test".into(),
        family: None,
        trains_on_prompts: None,
    });
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let mut query = request("codex/test")?;
    query.effort = Some("high".into());
    query.fallback = Fallback::Models(BoundedModels::new(vec![ModelId::parse("agy/unknown")?])?);
    assert!(matches!(
        resolve(&query, &context),
        Err(RoutingError::UnsupportedEffort)
    ));
    Ok(())
}
fn availability() -> AvailabilityRecord {
    AvailabilityRecord {
        schema_version: 1,
        adapter: "codex".into(),
        executable: "/fixture/codex".into(),
        version: "fixture".into(),
        fixture_ref: "test".into(),
        catalogue_revision: "recordings-20261024-utility-v1".into(),
        account_key: Some("opaque-account".into()),
        observed_models: vec!["test".into()],
        last_preflight_error: None,
        observed_unix_seconds: 80,
        expires_unix_seconds: 120,
    }
}
#[test]
fn cached_claims_cannot_activate_and_age_is_exact() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let records = [availability()];
    let mut context = context(&catalogue, &config, now()?);
    context.availability = &records;
    let listed = list(
        &RouteListQuery {
            adapter: Some("codex".into()),
        },
        &context,
    )?;
    assert_eq!(listed.availability.len(), 1);
    assert_eq!(
        listed.availability.first().and_then(|a| a.age_seconds),
        Some(20)
    );
    // Enabled and available, Codex reaches the next gate: disclosure.
    assert!(matches!(
        resolve(&request("codex/test")?, &context)?.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::DisclosureDenied
        }
    ));
    // A fresh record cannot activate an adapter the matrix leaves disabled.
    let mut cursor = availability();
    cursor.adapter = "cursor".into();
    let records = [cursor];
    context.availability = &records;
    assert!(matches!(
        resolve(&request("cursor/test")?, &context)?.readiness,
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::Disabled
        }
    ));
    let duplicate = [availability(), availability()];
    context.availability = &duplicate;
    assert!(matches!(
        list(&RouteListQuery::default(), &context),
        Err(RoutingError::Availability)
    ));
    let mut invalid = availability();
    invalid.account_key = Some("email@example.com".into());
    assert!(validate_availability(&invalid, "codex").is_err());
    invalid = availability();
    invalid.expires_unix_seconds = invalid.observed_unix_seconds;
    assert!(validate_availability(&invalid, "codex").is_err());
    Ok(())
}
#[test]
fn configured_domain_is_closed_and_registered_without_config_dependency_cycles()
-> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Domains {
        routing: RoutingConfig,
    }
    static ENTRIES: [provenant_config::DomainSection; 1] = [SECTION];
    let sections = DomainSections::<Domains>::new(&ENTRIES);
    // Config's loader owns root validation; tests inject an isolated, non-I/O reader.
    struct Resolver;
    impl provenant_core::PathResolver for Resolver {
        fn resolve_existing_ancestor(
            &self,
            path: &std::path::Path,
        ) -> std::io::Result<std::path::PathBuf> {
            Ok(path.into())
        }
    }
    let mut inputs =
        provenant_core::RootInputs::new("/fixture/home".into(), "/fixture/assets".into());
    inputs.state_dir = Some("/fixture/state".into());
    let roots = inputs.resolve(&Resolver)?;
    let cfg = provenant_config::load_with_domains(
        &roots,
        Some("schema_version=1\n[domains.routing.routes.review]\nmodel='codex/exact'\n".as_bytes()),
        &sections,
    )?;
    assert_eq!(
        cfg.domains
            .routing
            .routes
            .get("review")
            .map(|r| r.model.model()),
        Some("exact")
    );
    let cfg: RoutingConfig =
        toml_edit::de::from_str("[routes.review]\nmodel = 'opencode/openrouter/vendor/model'\n")?;
    assert!(cfg.validate().is_empty());
    assert_eq!(
        cfg.routes.get("review").map(|r| r.model.model()),
        Some("openrouter/vendor/model")
    );
    assert!(toml_edit::de::from_str::<RoutingConfig>("preferences = true\n[routes]").is_err());
    Ok(())
}
#[test]
fn family_requires_provider_answer_evidence_at_the_same_revision()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_providers::{
        Adapter, ExitObservation, FamilyEntry, RouteProvenance, claude::Claude,
    };
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = context(&catalogue, &config, now()?);
    let route = resolve(&request("claude/claude-haiku-4-5-20251001")?, &context)?;
    let mut stream = Claude.stream(None)?;
    Claude.observe(
        &mut stream,
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/providers/claude/direct-model.jsonl"
        ))
        .as_bytes(),
    )?;
    let outcome = Claude.finish(
        stream,
        ExitObservation {
            code: Some(0),
            timed_out: false,
        },
    )?;
    let families = [FamilyEntry {
        adapter: "claude".into(),
        model: route.model.clone(),
        family: "anthropic".into(),
    }];
    let lineage = Claude.validate_provenance(&RouteProvenance {
        route: &route,
        outcome: &outcome,
        evidence_ref: "recording",
        evidence_catalogue_revision: &catalogue.revision,
        catalogue: &families,
    })?;
    let forged_rows = [FamilyEntry {
        adapter: "claude".into(),
        model: route.model.clone(),
        family: "fabricated-family".into(),
    }];
    let forged = Claude.validate_provenance(&RouteProvenance {
        route: &route,
        outcome: &outcome,
        evidence_ref: "recording",
        evidence_catalogue_revision: &catalogue.revision,
        catalogue: &forged_rows,
    })?;
    assert_eq!(
        FamilyFact::from_verified(
            ModelId::parse("claude/claude-haiku-4-5-20251001")?,
            &forged,
            80,
            120,
            &catalogue
        )?
        .record()
        .family,
        None
    );
    let fact = FamilyFact::from_verified(
        ModelId::parse("claude/claude-haiku-4-5-20251001")?,
        &lineage,
        80,
        120,
        &catalogue,
    )?;
    assert_eq!(fact.record().family.as_deref(), Some("anthropic"));
    let wrong = FamilyFact::from_verified(
        ModelId::parse("claude/claude-sonnet-5")?,
        &lineage,
        80,
        120,
        &catalogue,
    )?;
    assert_eq!(wrong.record().family, None);
    let mut broker = catalogue.clone();
    broker.models.push(ModelEntry {
        model: ModelId::parse("codex/claude-haiku-4-5-20251001")?,
        efforts: vec![],
        family: Some("anthropic".into()),
        evidence_ref: "test".into(),
        trains_on_prompts: None,
    });
    assert_eq!(
        FamilyFact::from_verified(
            ModelId::parse("codex/claude-haiku-4-5-20251001")?,
            &lineage,
            80,
            120,
            &broker
        )?
        .record()
        .family,
        None
    );
    let mut changed = catalogue.clone();
    changed.revision = "new-revision".into();
    let wrong = FamilyFact::from_verified(
        ModelId::parse("claude/claude-haiku-4-5-20251001")?,
        &lineage,
        80,
        120,
        &changed,
    )?;
    assert_eq!(wrong.record().family, None);
    let demand = ReviewDemand {
        family_slots: BTreeMap::from([("anthropic".into(), 1)]),
    };
    let facts = [fact];
    let requests = [request("claude/claude-haiku-4-5-20251001")?];
    assert!(
        !review_availability(&RoutingSnapshot::new(&requests, &context, &facts)?, &demand)?
            .satisfied()
    );
    let registry = [provenant_providers::Registration {
        adapter: &RecordedFixture(&Claude),
        quota_source: None,
    }];
    let mut record = availability();
    record.adapter = "claude".into();
    record.observed_models = vec!["claude-haiku-4-5-20251001".into()];
    let records = [record];
    let access = [AccessContext {
        adapter: "claude",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut context = context;
    context.registry = &registry;
    context.availability = &records;
    context.access = &access;
    let duplicated = [requests[0].clone(), requests[0].clone()];
    let snapshot = RoutingSnapshot::new(&duplicated, &context, &facts)?;
    assert!(review_availability(&snapshot, &demand)?.satisfied());
    let double = ReviewDemand {
        family_slots: BTreeMap::from([("anthropic".into(), 2)]),
    };
    assert!(!review_availability(&snapshot, &double)?.satisfied());
    context.now = provenant_core::WallTime::from_micros(120_000_000)?;
    assert!(
        !review_availability(&RoutingSnapshot::new(&requests, &context, &facts)?, &demand)?
            .satisfied()
    );
    Ok(())
}

/// A test-only activation gate, with the provider's real mechanics otherwise
/// unchanged. No fixture activation is exported from the library or registry.
struct RecordedFixture(&'static dyn provenant_providers::Adapter);
impl provenant_providers::Adapter for RecordedFixture {
    fn id(&self) -> &'static str {
        self.0.id()
    }
    fn execution_enabled(&self) -> bool {
        true
    }
    fn account(
        &self,
        s: &provenant_providers::StreamState,
    ) -> Result<provenant_providers::AccountDetection, provenant_providers::ProviderError> {
        self.0.account(s)
    }
    fn preflight(
        &self,
        i: &provenant_providers::PreflightInput,
    ) -> Result<provenant_providers::PreflightPlan, provenant_providers::ProviderError> {
        self.0.preflight(i)
    }
    fn decode_preflight(
        &self,
        p: &provenant_providers::PreflightPlan,
        b: &[u8],
    ) -> Result<AvailabilityRecord, provenant_providers::ProviderError> {
        self.0.decode_preflight(p, b)
    }
    fn prepare(
        &self,
        i: provenant_providers::PrepareInput<'_>,
    ) -> Result<provenant_providers::LaunchSpec, provenant_providers::ProviderError> {
        self.0.prepare(i)
    }
    fn stream(
        &self,
        c: Option<&provenant_providers::Continuation>,
    ) -> Result<provenant_providers::StreamState, provenant_providers::ProviderError> {
        self.0.stream(c)
    }
    fn observe(
        &self,
        s: &mut provenant_providers::StreamState,
        b: &[u8],
    ) -> Result<Vec<provenant_providers::ProviderEvent>, provenant_providers::ProviderError> {
        self.0.observe(s, b)
    }
    fn finish(
        &self,
        s: provenant_providers::StreamState,
        e: provenant_providers::ExitObservation,
    ) -> Result<provenant_providers::ProviderOutcome, provenant_providers::ProviderError> {
        self.0.finish(s, e)
    }
    fn continuation(
        &self,
        o: &provenant_providers::ProviderOutcome,
        r: &ResolvedRoute,
        d: &str,
    ) -> Result<provenant_providers::Continuation, provenant_providers::ProviderError> {
        self.0.continuation(o, r, d)
    }
    fn validate_provenance(
        &self,
        p: &provenant_providers::RouteProvenance<'_>,
    ) -> Result<provenant_providers::VerifiedLineage, provenant_providers::ProviderError> {
        self.0.validate_provenance(p)
    }
}
fn reason(route: &ResolvedRoute) -> Option<&RouteExclusionReason> {
    match &route.readiness {
        RouteReadiness::Unavailable { reason } => Some(reason),
        _ => None,
    }
}
fn quota(
    model: &str,
    account: &str,
    status: provenant_quota::QuotaStatus,
    revision: u64,
    observed: i64,
    expires: i64,
) -> Result<QuotaObservation, RoutingError> {
    Ok(QuotaObservation {
        model: ModelId::parse(model)?,
        expires_unix_seconds: expires,
        snapshot: provenant_quota::QuotaSnapshot {
            schema_version: 1,
            account: provenant_quota::AccountScope {
                adapter: "codex".into(),
                account_key: account.into(),
                label: "test".into(),
            },
            revision,
            status,
            observed_unix_seconds: observed,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: vec![],
        },
    })
}
#[test]
fn activation_availability_access_and_scoped_quota_are_independent_gates()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_quota::QuotaStatus;
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let mut context = context(&catalogue, &config, now()?);
    let registry = [provenant_providers::Registration {
        adapter: &RecordedFixture(&provenant_providers::codex::Codex),
        quota_source: None,
    }];
    context.registry = &registry;
    let query = request("codex/test")?;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::UnknownAvailability)
    );
    let mut records = [availability()];
    records[0].expires_unix_seconds = 100;
    context.availability = &records;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::StaleAvailability)
    );
    let records = [availability()];
    context.availability = &records;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::DisclosureDenied)
    );
    let disclosure = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "",
    }];
    context.access = &disclosure;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::ConfinementUnavailable)
    );
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    context.access = &access;
    let route = resolve(&query, &context)?;
    assert!(reason(&route).is_none());
    assert_eq!(route.confidentiality.evidence_ref, "policy");
    assert_eq!(route.account_key.as_deref(), Some("opaque-account"));
    assert_eq!(
        reason(&resolve(&request("codex/not-observed")?, &context)?),
        Some(&RouteExclusionReason::UnverifiedModel)
    );
    let mut error_record = availability();
    error_record.last_preflight_error = Some(provenant_providers::PreflightFailure::AuthRequired);
    let error_records = [error_record];
    context.availability = &error_records;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::AuthRequired)
    );
    context.availability = &records;
    let stale = [quota(
        "codex/test",
        "opaque-account",
        QuotaStatus::Exhausted,
        1,
        80,
        99,
    )?];
    context.quota = &stale;
    assert!(reason(&resolve(&query, &context)?).is_none());
    let unknown = [quota(
        "codex/test",
        "opaque-account",
        QuotaStatus::Unknown,
        1,
        80,
        120,
    )?];
    context.quota = &unknown;
    assert!(reason(&resolve(&query, &context)?).is_none());
    let other_account = [quota(
        "codex/test",
        "other-account",
        QuotaStatus::Exhausted,
        1,
        80,
        120,
    )?];
    context.quota = &other_account;
    assert!(reason(&resolve(&query, &context)?).is_none());
    let other_model = [quota(
        "codex/other",
        "opaque-account",
        QuotaStatus::Exhausted,
        1,
        80,
        120,
    )?];
    context.quota = &other_model;
    assert!(reason(&resolve(&query, &context)?).is_none());
    let current = [quota(
        "codex/test",
        "opaque-account",
        QuotaStatus::Exhausted,
        1,
        80,
        120,
    )?];
    context.quota = &current;
    assert_eq!(
        reason(&resolve(&query, &context)?),
        Some(&RouteExclusionReason::Exhausted)
    );
    let reset = [
        quota(
            "codex/test",
            "opaque-account",
            QuotaStatus::Exhausted,
            1,
            80,
            120,
        )?,
        quota(
            "codex/test",
            "opaque-account",
            QuotaStatus::Available,
            2,
            90,
            120,
        )?,
    ];
    context.quota = &reset;
    assert!(
        reason(&resolve(&query, &context)?).is_none(),
        "a newer comparable observation supersedes exhausted"
    );
    Ok(())
}
