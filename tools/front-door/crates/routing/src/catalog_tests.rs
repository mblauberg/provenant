use super::*;
use crate::{AccessContext, Fallback, RouteChoice, RouteRequest, RoutingContext, resolve};
use provenant_providers::{AvailabilityRecord, REGISTRY};

#[test]
fn route_names_include_configured_and_catalogue_pools() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let mut config = RoutingConfig::default();
    config.pools.insert("verifier".into(), Vec::new());
    let names = config.route_names(&catalogue);
    assert!(names.contains("verifier"), "{names:?}");
    assert!(names.contains("junior"), "{names:?}");
    assert!(!names.contains("absent"), "{names:?}");

    let asset: RoutingConfig =
        toml_edit::de::from_str(include_str!("../assets/review-defaults.toml"))?;
    assert_eq!(
        serde_json::to_value(RoutingConfig::default())?,
        serde_json::to_value(asset)?
    );
    Ok(())
}

// Spec: docs/specs/routing.md#provider-model-discovery
#[test]
fn family_aliases_resolve_to_the_latest_catalogued_model() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = Catalogue::bundled()?;
    for (alias, expected) in [
        ("claude/sonnet", "claude/claude-sonnet-5-5"),
        ("claude/opus", "claude/claude-opus-5-5"),
        ("claude/claude-sonnet-5", "claude/claude-sonnet-5"),
        ("agy/sonnet", "agy/claude-sonnet-5-5"),
        ("agy/opus", "agy/claude-opus-5-5"),
        ("agy/hydrogen", "agy/gemini-4-hydrogen"),
        ("agy/neon", "agy/gemini-4-neon"),
        ("agy/argon", "agy/gemini-4-argon"),
        ("agy/flash", "agy/gemini-3.8-flash"),
        ("codex/luna", "codex/gpt-6-luna"),
        ("codex/terra", "codex/gpt-6.1-terra"),
        ("codex/sol", "codex/gpt-6.1-sol"),
    ] {
        assert_eq!(
            catalogue
                .resolve_family_alias(&ModelId::parse(alias)?)
                .map(ModelId::as_str),
            Some(expected),
            "{alias}"
        );
    }
    for unknown in [
        "claude/unicorn",
        "claude/claude",
        "agy/gemini",
        "agy/claude",
    ] {
        assert_eq!(
            catalogue.resolve_family_alias(&ModelId::parse(unknown)?),
            None,
            "{unknown}"
        );
    }
    // Ambiguous suffix across two distinct families on the same adapter stays unresolved.
    let mut synthetic = catalogue.clone();
    for id in ["agy/alpha-1-shared", "agy/beta-2-shared"] {
        synthetic.models.push(ModelEntry {
            capability: Default::default(),
            free: false,
            model: ModelId::parse(id)?,
            efforts: Vec::new(),
            effort_suffix: false,
            evidence_ref: "fixture".into(),
            source: "fixture".into(),
            service_tiers: Vec::new(),
            policy_rules: Vec::new(),
            family: None,
            trains_on_prompts: None,
            monthly_allowance_usd: None,
            quota_pool: None,
            superseded_by: Vec::new(),
        });
    }
    assert_eq!(
        synthetic.resolve_family_alias(&ModelId::parse("agy/shared")?),
        None
    );
    Ok(())
}

#[test]
fn validate_rejects_invalid_and_cyclic_superseded_by_edges()
-> Result<(), Box<dyn std::error::Error>> {
    let base = Catalogue::bundled()?;
    let flash = ModelId::parse("agy/gemini-3.8-flash")?;
    let neon = ModelId::parse("agy/gemini-4-neon")?;

    // Self-reference is rejected.
    let mut self_ref = base.clone();
    self_ref
        .models
        .iter_mut()
        .find(|m| m.model == flash)
        .unwrap()
        .superseded_by = vec![flash.clone()];
    assert!(self_ref.validate().is_err());

    // Cross-adapter target is rejected.
    let mut cross_adapter = base.clone();
    cross_adapter
        .models
        .iter_mut()
        .find(|m| m.model == flash)
        .unwrap()
        .superseded_by = vec![ModelId::parse("claude/claude-sonnet-5-5")?];
    assert!(cross_adapter.validate().is_err());

    // Uncatalogued target is rejected.
    let mut unknown_target = base.clone();
    unknown_target
        .models
        .iter_mut()
        .find(|m| m.model == flash)
        .unwrap()
        .superseded_by = vec![ModelId::parse("agy/not-in-catalogue")?];
    assert!(unknown_target.validate().is_err());

    // Cycle (flash -> neon -> flash) is rejected.
    let mut cyclic = base.clone();
    cyclic
        .models
        .iter_mut()
        .find(|m| m.model == neon)
        .unwrap()
        .superseded_by = vec![flash.clone()];
    assert!(cyclic.validate().is_err());

    // Transitive supersedence (sonnet-4-6 -> sonnet-5-5 -> opus-5-5) is reported.
    let mut chain = base.clone();
    let sonnet46 = ModelId::parse("agy/claude-sonnet-4-6")?;
    let sonnet55 = ModelId::parse("agy/claude-sonnet-5-5")?;
    let opus55 = ModelId::parse("agy/claude-opus-5-5")?;
    chain
        .models
        .iter_mut()
        .find(|m| m.model == sonnet55)
        .unwrap()
        .superseded_by = vec![opus55.clone()];
    assert!(chain.validate().is_ok());
    let transitive = chain.transitively_superseded_by(&sonnet46);
    assert!(transitive.contains(&&sonnet55));
    assert!(transitive.contains(&&opus55));
    Ok(())
}

#[test]
fn writer_advisory_classes_and_pool_priority_are_overridable()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = RoutingContext {
        policy_account_hint: None,
        choice_context: None,
        selection_evidence: None,
        catalogue: &catalogue,
        config: &config,
        registry: REGISTRY,
        availability: &[],
        quota: &[],
        capacity_cooldowns: &[],
        accounts: &[],
        quota_policies: &[],
        launch_specs: None,
        readiness: None,
        access: &[],
        now: provenant_core::WallTime::from_micros(0)?,
        host: "test-host",
    };
    let request = |id| -> Result<RouteRequest, RoutingError> {
        Ok(RouteRequest {
            intent: Default::default(),
            choice: RouteChoice::Model(ModelId::parse(id)?),
            effort: None,
            confidential: false,
            fallback: Fallback::None,
        })
    };
    let warn_model = catalogue
        .writer_advisory
        .warn_models
        .first()
        .ok_or("no warn models in catalogue")?
        .clone();
    let design_cheaper = catalogue
        .pools
        .get("design")
        .and_then(|members| {
            members.iter().find(|member| {
                member.model != warn_model
                    && catalogue
                        .writer_advisory
                        .cheaper_models
                        .contains(&member.model)
            })
        })
        .ok_or("no bundled cheaper design member")?
        .model
        .clone();
    let mut route = resolve(&request(warn_model.as_str())?, &context)?;
    let default = config
        .opus_writer_advisory(&catalogue, &route)
        .ok_or("default advisory missing")?;
    assert!(default.contains(warn_model.as_str()), "{default}");
    let changed: RoutingConfig = toml_edit::de::from_str(&format!(
        r#"
            [writer_advisory]
            warn_models = ["{warn_model}", "custom/synth-warn"]
            cheaper_models = ["custom/synth-cheap", "{design_cheaper}"]
            pool_priority = ["design", "senior", "principal"]
        "#,
        warn_model = warn_model.as_str(),
        design_cheaper = design_cheaper.as_str()
    ))?;
    assert!(changed.validate().is_empty());
    let overridden = changed
        .opus_writer_advisory(&catalogue, &route)
        .ok_or("overridden advisory missing")?;
    for part in [warn_model.as_str(), "design", design_cheaper.as_str()] {
        assert!(overridden.contains(part), "{overridden}");
    }
    route.adapter = "custom".into();
    route.model = "synth-warn".into();
    assert!(config.opus_writer_advisory(&catalogue, &route).is_none());
    // Familiar spelling on an uncatalogued ID supplies no class evidence.
    route.model = "unknown-model".into();
    assert!(config.opus_writer_advisory(&catalogue, &route).is_none());
    Ok(())
}

fn request_for(id: &str) -> Result<RouteRequest, RoutingError> {
    Ok(RouteRequest {
        intent: Default::default(),
        choice: RouteChoice::Model(ModelId::parse(id)?),
        effort: None,
        confidential: false,
        fallback: Fallback::None,
    })
}

#[test]
fn bundled_opus_writer_advice_names_exact_routes() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let context = RoutingContext {
        policy_account_hint: None,
        choice_context: None,
        selection_evidence: None,
        catalogue: &catalogue,
        config: &config,
        registry: REGISTRY,
        availability: &[],
        quota: &[],
        capacity_cooldowns: &[],
        accounts: &[],
        quota_policies: &[],
        launch_specs: None,
        readiness: None,
        access: &[],
        now: provenant_core::WallTime::from_micros(0)?,
        host: "test-host",
    };
    for model in &catalogue.writer_advisory.warn_models {
        for (pool, members) in &catalogue.pools {
            let mut route = resolve(&request_for(model.as_str())?, &context)?;
            route.effort = Some("high".into());
            route.pool_selection = Some(provenant_records::RouteSelectionReceipt {
                selection_rule: provenant_records::route_selection::SelectionRule::WeightedDraw,
                considered_exclusions: Vec::new(),
                exploration: provenant_records::route_selection::Exploration {
                    extra: Default::default(),
                    selected: false,
                    percent: 0,
                    enabled: false,
                    source: "fixture".into(),
                },
                weight_source: provenant_records::route_selection::WeightSource::Catalogue,
                account_load: None,
                extra: Default::default(),
                pool: pool.clone(),
                candidates: vec![],
                roll: 0,
                total_weight: 1,
                pick: format!("{}/{}", route.adapter, route.model),
                reason: None,
                redraws: vec![],
            });
            let advice = config.opus_writer_advisory(&catalogue, &route);
            let cheaper = members.iter().find(|member| {
                member.weight > 0
                    && member.model != *model
                    && catalogue
                        .writer_advisory
                        .cheaper_models
                        .contains(&member.model)
            });
            match cheaper {
                Some(cheaper) => {
                    let advice = advice.ok_or("missing writer advisory")?;
                    for part in [
                        model.as_str(),
                        pool.as_str(),
                        cheaper.model.as_str(),
                        "suppress_opus_writer_warning",
                    ] {
                        assert!(advice.contains(part), "{advice}");
                    }
                }
                None => assert!(advice.is_none(), "unexpected advisory for {pool}"),
            }
        }
    }
    let route = resolve(&request_for("codex/not-in-catalogue-at-all")?, &context)?;
    assert!(config.opus_writer_advisory(&catalogue, &route).is_none());
    Ok(())
}

// Spec: docs/specs/routing.md#provider-model-discovery
#[test]
fn partial_discovery_keeps_writer_advice_for_each_family() -> Result<(), Box<dyn std::error::Error>>
{
    let snapshot = crate::parse_codex_models(
        br#"{"models":[{"slug":"gpt-6.2-sol","supported_reasoning_levels":[{"effort":"high"}]}]}"#,
        1,
    )?;
    let catalogue = Catalogue::bundled()?.with_discovery(&[snapshot])?;
    assert!(
        catalogue
            .writer_advisory
            .cheaper_models
            .iter()
            .any(|id| id.as_str() == "codex/gpt-6.2-sol")
    );
    let config = RoutingConfig::default();
    let context = RoutingContext {
        policy_account_hint: None,
        choice_context: None,
        selection_evidence: None,
        catalogue: &catalogue,
        config: &config,
        registry: REGISTRY,
        availability: &[],
        quota: &[],
        capacity_cooldowns: &[],
        accounts: &[],
        quota_policies: &[],
        launch_specs: None,
        readiness: None,
        access: &[],
        now: provenant_core::WallTime::from_micros(0)?,
        host: "test-host",
    };
    let opus = catalogue.pools["principal"]
        .iter()
        .find(|member| {
            catalogue
                .writer_advisory
                .warn_models
                .contains(&member.model)
        })
        .ok_or("missing principal Opus")?;
    let route = resolve(&request_for(opus.model.as_str())?, &context)?;
    let advice = config
        .opus_writer_advisory(&catalogue, &route)
        .ok_or("missing advice")?;
    assert!(advice.contains("codex/gpt-6.2-sol"), "{advice}");
    Ok(())
}

#[test]
fn partial_writer_advisory_override_keeps_bundled_classes() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = Catalogue::bundled()?;
    let base = RoutingConfig::default();
    let context = RoutingContext {
        policy_account_hint: None,
        choice_context: None,
        selection_evidence: None,
        catalogue: &catalogue,
        config: &base,
        registry: REGISTRY,
        availability: &[],
        quota: &[],
        capacity_cooldowns: &[],
        accounts: &[],
        quota_policies: &[],
        launch_specs: None,
        readiness: None,
        access: &[],
        now: provenant_core::WallTime::from_micros(0)?,
        host: "test-host",
    };
    let warn_model = catalogue
        .writer_advisory
        .warn_models
        .first()
        .ok_or("warn model")?;
    let mut route = resolve(&request_for(warn_model.as_str())?, &context)?;
    route.effort = Some("high".into());
    let advice = |toml: &str| -> Result<Option<String>, Box<dyn std::error::Error>> {
        let config: RoutingConfig = toml_edit::de::from_str(toml)?;
        assert!(config.validate().is_empty(), "{toml}");
        Ok(config.opus_writer_advisory(&catalogue, &route))
    };
    let label_only = advice("[writer_advisory]\nlabel = \"Principal\"\n")?.ok_or("label")?;
    assert!(label_only.contains("Principal"), "{label_only}");
    assert!(label_only.contains(warn_model.as_str()), "{label_only}");
    let pool_only = advice("[writer_advisory]\npool_priority = [\"design\"]\n")?.ok_or("pool")?;
    assert!(pool_only.contains(warn_model.as_str()), "{pool_only}");
    let emptied = advice("[writer_advisory]\nwarn_models = []\n")?;
    assert!(emptied.is_none(), "an empty warn list silences the class");
    Ok(())
}

#[test]
fn writer_advisory_rejects_invalid_pool_order() -> Result<(), Box<dyn std::error::Error>> {
    for pools in [r#"["design", "design"]"#, r#"["bad pool"]"#] {
        let config: RoutingConfig = toml_edit::de::from_str(&format!(
            "[writer_advisory]\nwarn_models=[]\ncheaper_models=[]\npool_priority={pools}\n"
        ))?;
        assert!(
            config
                .validate()
                .iter()
                .any(|e| e.field == "writer_advisory.pool_priority")
        );
    }
    Ok(())
}

#[test]
fn model_policy_fallbacks_are_reported_for_the_effective_route()
-> Result<(), Box<dyn std::error::Error>> {
    let mut config = RoutingConfig::default();
    let unknown_fallback = ModelId::parse("codex/not-catalogued")?;
    config.model_policy.defaults.result.fallback = Some(unknown_fallback.clone());
    assert!(config.validate().is_empty());
    let effective = config.model_policy.effective(
        &crate::Catalogue::bundled().unwrap(),
        &ModelId::parse("codex/gpt-6-luna")?,
        None,
        provenant_core::WallTime::from_unix_seconds(1)?,
    );
    assert!(effective.policy.result.fallback.is_none());
    assert!(
        effective
            .dropped_fallback
            .is_some_and(|(model, _)| model == unknown_fallback)
    );

    let unsupported_effort_fallback = ModelId::parse("codex/gpt-6-luna")?;
    config.model_policy.defaults.result.fallback = Some(unsupported_effort_fallback.clone());
    config.model_policy.defaults.result.fallback_effort = Some("unsupported".into());
    assert!(config.validate().is_empty());
    let effective = config.model_policy.effective(
        &crate::Catalogue::bundled().unwrap(),
        &ModelId::parse("codex/gpt-6-luna")?,
        None,
        provenant_core::WallTime::from_unix_seconds(1)?,
    );
    assert!(
        effective
            .dropped_fallback
            .is_some_and(|(model, _)| model == unsupported_effort_fallback)
    );
    Ok(())
}

#[test]
fn bundled_pools_are_weight_neutral_and_exclude_other_opencode_models()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let junior = &catalogue.pools["junior"];
    let mut pairs = 0;
    for members in catalogue.pools.values() {
        for member in members {
            let name = member.model.as_str();
            if name.starts_with("opencode/") {
                assert!(
                    name.contains("flash"),
                    "unexpected default pool model: {name}"
                );
            }
        }
    }
    for free in junior
        .iter()
        .filter(|member| catalogue.is_free_model(&member.model))
    {
        let Some(name) = free
            .model
            .as_str()
            .strip_prefix("opencode/opencode/")
            .and_then(|name| name.strip_suffix("-free"))
        else {
            continue;
        };
        let paid = ModelId::parse(&format!("opencode/opencode-go/{name}"))?;
        if let Some(paid) = junior.iter().find(|member| member.model == paid) {
            assert_eq!(
                free.weight, paid.weight,
                "bundled weights must stay capability-neutral, not provider- or plan-specific"
            );
            pairs += 1;
        }
    }
    assert!(
        pairs > 0,
        "a free flash model and its paid counterpart should both be pooled"
    );
    Ok(())
}

/// Bundled pools carry no owner, account or plan annotations: every
/// standard member carries explicit weight `2` while usage-constrained
/// routes (`agy`, `cursor`) use `default_weight()` (`1`), and the source
/// rows have no comments.
#[test]
fn bundled_pools_have_no_owner_or_plan_annotations() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    for (pool, members) in &catalogue.pools {
        for member in members {
            let expected = match member.model.adapter() {
                "agy" | "cursor" => default_weight(),
                _ => 2,
            };
            assert_eq!(
                member.weight,
                expected,
                "bundled pool {pool} member {} has unexpected weight",
                member.model.as_str()
            );
        }
    }
    let text = include_str!("../assets/catalogue.toml");
    let rows: Vec<&str> = text
        .lines()
        .skip_while(|line| *line != "[pools]")
        .skip(1)
        .take_while(|line| !line.starts_with(['[', '#']))
        .collect();
    assert!(!rows.is_empty(), "catalogue has no pools table");
    assert!(
        rows.iter().all(|row| !row.contains('#')),
        "bundled [pools] rows must not carry owner or plan comments"
    );
    Ok(())
}

#[test]
fn bundled_go_models_resolve_and_only_flash_models_have_provisional_pool_weights()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config = RoutingConfig::default();
    let model_names: Vec<_> = catalogue
        .models()
        .iter()
        .filter_map(|entry| entry.model.as_str().strip_prefix("opencode/opencode-go/"))
        .collect();
    assert!(!model_names.is_empty());
    let availability = [AvailabilityRecord {
        schema_version: 1,
        adapter: "opencode".into(),
        executable: "/fixture/opencode".into(),
        version: "fixture".into(),
        fixture_ref: "test".into(),
        catalogue_revision: catalogue.revision().into(),
        account_key: Some("go".into()),
        plan: Some("go".into()),
        observed_models: model_names
            .iter()
            .map(|name| format!("opencode-go/{name}"))
            .collect(),
        observed_unix_seconds: 80,
        expires_unix_seconds: 120,
        last_update: None,
    }];
    let access = [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let context = RoutingContext {
        policy_account_hint: None,
        choice_context: None,
        selection_evidence: None,
        catalogue: &catalogue,
        config: &config,
        registry: REGISTRY,
        availability: &availability,
        quota: &[],
        capacity_cooldowns: &[],
        accounts: &[],
        quota_policies: &[],
        launch_specs: None,
        readiness: None,
        access: &access,
        now: provenant_core::WallTime::from_micros(100_000_000)?,
        host: "test-host",
    };
    for name in model_names {
        let model = ModelId::parse(&format!("opencode/opencode-go/{name}"))?;
        let entry = catalogue.model(&model).ok_or("Go model missing")?;
        assert!(!entry.evidence_ref.is_empty());
        let route = resolve(
            &RouteRequest {
                intent: Default::default(),
                choice: RouteChoice::Model(model.clone()),
                effort: None,
                confidential: false,
                fallback: Fallback::None,
            },
            &context,
        )?;
        assert_eq!(route.model, format!("opencode-go/{name}"));
        for member in catalogue
            .pools
            .values()
            .flatten()
            .filter(|member| member.model == model)
        {
            assert!(name.contains("flash"));
            assert!(!member.free);
            assert!(member.effort.is_none());
            assert!(member.efforts.is_empty());
        }
    }
    Ok(())
}

#[test]
fn bundled_opencode_flash_models_are_catalogued_and_free() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = Catalogue::bundled()?;
    for id in [
        "opencode/opencode-go/hy4-flash-free",
        "opencode/opencode/hy4-flash-free",
        "opencode/opencode-go/deepseek-v4.1-flash",
        "opencode/opencode/deepseek-v4.1-flash-free",
    ] {
        let model = ModelId::parse(id)?;
        let entry = catalogue
            .model(&model)
            .ok_or_else(|| format!("{id} is missing from the bundled catalogue"))?;
        assert!(!entry.evidence_ref.is_empty(), "{id} has no evidence");
        if id.ends_with("-free") {
            assert!(catalogue.is_free_model(&model), "{id} must be free");
        }
    }
    for short in ["opencode/hy4-flash-free", "opencode/deepseek-v4.1-flash"] {
        let id = ModelId::parse(short)?;
        assert!(
            !catalogue.namespaced_candidates(&id).is_empty(),
            "{short} has no catalogued provider segment"
        );
    }
    Ok(())
}

// Spec: docs/specs/fabric.md#one-model-spelling-per-route
#[test]
fn canonical_route_model_resolves_a_relative_opencode_id() -> Result<(), Box<dyn std::error::Error>>
{
    let catalogue = Catalogue::bundled()?;
    let (full, warning) =
        super::canonical_route_model("opencode", "opencode/hy4-flash-free", Some(&catalogue));
    assert_eq!(full, "opencode/opencode/hy4-flash-free");
    assert!(warning.is_none(), "{warning:?}");
    // The provider segment selects the catalogued provider: the Go
    // account's id is a different, equally exact, route.
    let (go, warning) =
        super::canonical_route_model("opencode", "opencode-go/hy4-flash-free", Some(&catalogue));
    assert_eq!(go, "opencode/opencode-go/hy4-flash-free");
    assert!(warning.is_none(), "{warning:?}");
    // A bare relative model with a non-namespaced adapter becomes a full
    // id (parseable downstream) and warns, rather than staying bare.
    let (full, warning) =
        super::canonical_route_model("anthropic", "claude-sonnet-5-5", Some(&catalogue));
    assert_eq!(full, "anthropic/claude-sonnet-5-5");
    assert!(warning.is_some(), "an uncatalogued id must warn");
    Ok(())
}

// Spec: docs/specs/fabric.md#one-model-spelling-per-route
#[test]
fn canonical_route_model_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    for (adapter, model) in [
        ("opencode", "opencode/opencode/hy4-flash-free"),
        ("opencode", "opencode/opencode/uncatalogued-fixture-model"),
        ("opencode", "opencode/hy4-flash-free"),
        ("anthropic", "claude-sonnet-5-5"),
    ] {
        let (once, first_warning) = super::canonical_route_model(adapter, model, Some(&catalogue));
        let (twice, second_warning) =
            super::canonical_route_model(adapter, &once, Some(&catalogue));
        assert_eq!(twice, once, "f(f({model})) rewrote a stored id");
        assert_eq!(
            second_warning.as_ref().map(|warning| warning.code.clone()),
            first_warning.as_ref().map(|warning| warning.code.clone()),
            "warning code changed on the second pass for {model}"
        );
    }
    // A full id that names the adapter but is absent from the catalogue is
    // kept, not turned into `adapter/adapter/...`; it warns with one code.
    let (full, warning) = super::canonical_route_model(
        "opencode",
        "opencode/opencode/uncatalogued-fixture-model",
        Some(&catalogue),
    );
    assert_eq!(full, "opencode/opencode/uncatalogued-fixture-model");
    let warning = warning.expect("an uncatalogued full id must warn");
    assert_eq!(warning.code, ROUTE_MODEL_NOT_CATALOGUED.as_str());
    assert!(warning.message.contains("uncatalogued-fixture-model"));
    Ok(())
}
