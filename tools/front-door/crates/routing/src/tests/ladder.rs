use super::*;

const NOW: i64 = 100;

fn model(text: &str) -> ModelId {
    ModelId::parse(text).unwrap_or_else(|error| panic!("{text}: {error}"))
}
fn ready(model_id: &str, account: &str) -> ReadinessFact {
    fact(model_id, account, Readiness::Ready)
}
fn fact(model_id: &str, account: &str, state: Readiness) -> ReadinessFact {
    ReadinessFact {
        model: model(model_id),
        account: Some(account.into()),
        state,
    }
}
fn route(model_id: &str, account: &str) -> Target {
    Target::Route {
        model: model(model_id),
        account: Some(account.into()),
    }
}

const SONNET: &str = "claude/claude-sonnet-5-5";
const HAIKU: &str = "claude/claude-haiku-5-5";
const OPUS: &str = "claude/claude-opus-5-5";
const SOL: &str = "codex/gpt-6.1-sol";
const TERRA: &str = "codex/gpt-6.1-terra";
const LUNA: &str = "codex/gpt-6-luna";

/// A four-tier ladder over the bundled catalogue. Senior holds a heavier
/// other-provider member, a lighter one and a same-family sibling.
struct Case {
    catalogue: Catalogue,
    config: RoutingConfig,
    stopped: Stopped,
    cause: DepletionCause,
    facts: Vec<ReadinessFact>,
    tried: Vec<Target>,
    native: Vec<HostNativeRoute>,
    exclusions: Vec<ExclusionRule>,
    intent: RouteIntent,
}
impl Case {
    fn new(facts: Vec<ReadinessFact>) -> Self {
        let catalogue = Catalogue::bundled().unwrap();
        let config: RoutingConfig = serde_json::from_value(serde_json::json!({
            "pools": {
                "utility": [{"model": HAIKU, "weight": 1, "efforts": []}],
                "junior": [{"model": LUNA, "weight": 1, "efforts": []}],
                "senior": [
                    {"model": SONNET, "weight": 4, "efforts": []},
                    {"model": HAIKU, "weight": 9, "efforts": []},
                    {"model": TERRA, "weight": 2, "effort": "low", "efforts": ["low"]},
                    {"model": SOL, "weight": 6, "effort": "medium", "efforts": ["medium"]}
                ],
                "principal": [{"model": OPUS, "weight": 1, "efforts": []}]
            }
        }))
        .unwrap();
        let stopped = Stopped {
            model: model(SONNET),
            account: Some("a1".into()),
            effort: Some("high".into()),
            tier: Some("senior".into()),
            family: Some("anthropic".into()),
        };
        Self {
            catalogue,
            config,
            stopped,
            cause: DepletionCause::UsageLimited,
            facts,
            tried: Vec::new(),
            native: Vec::new(),
            exclusions: Vec::new(),
            intent: RouteIntent::default(),
        }
    }
    fn step(&self) -> DepletionStep {
        next_after_depletion(&DepletionInput {
            catalogue: &self.catalogue,
            config: &self.config,
            stopped: &self.stopped,
            cause: self.cause,
            facts: &self.facts,
            tried: &self.tried,
            host_native: &self.native,
            exclusions: &self.exclusions,
            intent: self.intent,
            confidential: false,
            now: provenant_core::WallTime::from_unix_seconds(NOW).unwrap(),
        })
    }
    fn handoff(&self) -> Option<(Rung, Target)> {
        match self.step() {
            DepletionStep::Handoff(h) => Some((h.rung, h.target)),
            _ => None,
        }
    }
}
fn native(name: &str) -> HostNativeRoute {
    HostNativeRoute {
        name: name.into(),
        model: None,
    }
}

#[test]
fn each_rung_answers_when_the_ones_before_it_cannot() {
    let spent = fact(
        SONNET,
        "a1",
        Readiness::Depleted {
            resets_unix_seconds: None,
        },
    );
    let mut cases = vec![
        (
            vec![
                spent.clone(),
                ready(SONNET, "a2"),
                ready(SOL, "a1"),
                ready(LUNA, "a1"),
            ],
            Some((Rung::SameModelOtherAccount, route(SONNET, "a2"))),
        ),
        // The heavier member wins; the same-provider same-family sibling does not
        // count as another family.
        (
            vec![
                spent.clone(),
                ready(HAIKU, "a1"),
                ready(TERRA, "a1"),
                ready(SOL, "a1"),
                ready(LUNA, "a1"),
            ],
            Some((Rung::OtherProviderSameTier, route(SOL, "a1"))),
        ),
        (
            vec![
                spent.clone(),
                ready(HAIKU, "a1"),
                ready(TERRA, "a1"),
                ready(LUNA, "a1"),
                ready(OPUS, "a2"),
            ],
            Some((Rung::OtherProviderSameTier, route(TERRA, "a1"))),
        ),
        // Adjacent tiers: lower first by default.
        (
            vec![
                spent.clone(),
                ready(HAIKU, "a1"),
                ready(LUNA, "a1"),
                ready(OPUS, "a2"),
            ],
            Some((Rung::AdjacentTier, route(LUNA, "a1"))),
        ),
        (
            vec![spent.clone(), ready(HAIKU, "a1"), ready(OPUS, "a2")],
            Some((Rung::AdjacentTier, route(OPUS, "a2"))),
        ),
    ];
    cases.push((vec![spent.clone(), ready(HAIKU, "a1")], None));
    for (index, (facts, expected)) in cases.into_iter().enumerate() {
        assert_eq!(Case::new(facts).handoff(), expected, "case {index}");
    }
    let mut last = Case::new(vec![spent]);
    last.native = vec![native("fixer")];
    assert_eq!(
        last.handoff(),
        Some((
            Rung::HostNative,
            Target::HostNative {
                name: "fixer".into()
            }
        ))
    );
}

#[test]
fn effort_and_tier_follow_the_rung() {
    let mut case = Case::new(vec![ready(SONNET, "a2")]);
    let DepletionStep::Handoff(same) = case.step() else {
        panic!("expected a hand-off");
    };
    assert_eq!(
        (same.effort.as_deref(), same.tier.as_deref()),
        (Some("high"), Some("senior"))
    );
    case.facts = vec![ready(SOL, "a1")];
    let DepletionStep::Handoff(other) = case.step() else {
        panic!("expected a hand-off");
    };
    assert_eq!(
        (other.effort.as_deref(), other.tier.as_deref()),
        (Some("medium"), Some("senior"))
    );
    case.facts = vec![ready(LUNA, "a1")];
    let DepletionStep::Handoff(adjacent) = case.step() else {
        panic!("expected a hand-off");
    };
    assert_eq!(adjacent.tier.as_deref(), Some("junior"));
}

#[test]
fn nothing_eligible_is_the_chair() {
    assert_eq!(Case::new(Vec::new()).step(), DepletionStep::Chair);
    let mut case = Case::new(vec![ready(SONNET, "a2")]);
    case.config.depletion.rungs.clear();
    assert_eq!(case.step(), DepletionStep::Chair);
}

#[test]
fn unknown_and_unready_routes_are_never_offered() {
    // No fact for sol, a refused terra, a spent luna: all not ready.
    let case = Case::new(vec![
        fact(TERRA, "a1", Readiness::Unavailable),
        fact(
            LUNA,
            "a1",
            Readiness::Depleted {
                resets_unix_seconds: Some(NOW + 5),
            },
        ),
    ]);
    assert_eq!(case.step(), DepletionStep::Chair);
    // An unobserved route projects to unavailable, never ready.
    assert_eq!(
        Readiness::from_route(&provenant_records::RouteReadiness::unknown()),
        Readiness::Unavailable
    );
}

#[test]
fn exclusions_and_enforced_policy_remove_candidates_on_every_rung() {
    let facts = vec![
        ready(SONNET, "a2"),
        ready(SOL, "a1"),
        ready(TERRA, "a1"),
        ready(LUNA, "a1"),
        ready(OPUS, "a1"),
    ];
    let rule = |selector| ExclusionRule {
        selector,
        source: ExclusionSource::Config,
    };
    let account = |adapter: &str, key: &str| ExclusionSelector::Account {
        adapter: adapter.into(),
        key: key.into(),
    };
    for (excluded, expected) in [
        // The excluded account is skipped; the same model has no other.
        (
            vec![account("claude", "a2")],
            Some((Rung::OtherProviderSameTier, route(SOL, "a1"))),
        ),
        (
            vec![ExclusionSelector::Model(model(SONNET))],
            Some((Rung::OtherProviderSameTier, route(SOL, "a1"))),
        ),
        (
            vec![
                ExclusionSelector::Provider("claude".into()),
                ExclusionSelector::Model(model(SOL)),
            ],
            Some((Rung::OtherProviderSameTier, route(TERRA, "a1"))),
        ),
        (
            vec![
                ExclusionSelector::Provider("claude".into()),
                ExclusionSelector::Provider("codex".into()),
            ],
            None,
        ),
        (
            vec![
                account("claude", "a2"),
                account("codex", "a1"),
                account("claude", "a1"),
            ],
            None,
        ),
    ] {
        let mut case = Case::new(facts.clone());
        case.exclusions = excluded.into_iter().map(rule).collect();
        assert_eq!(case.handoff(), expected);
    }
    // A scheduled exclusion in the configuration binds like a rule.
    let mut configured = Case::new(vec![ready(SOL, "a1")]);
    configured.config.model_exclusions.push(ModelExclusion {
        model: model(SOL),
        schedule: Schedule::default(),
    });
    assert_eq!(configured.step(), DepletionStep::Chair);
    // So does enforced model policy: a read-only route takes no writer work.
    let mut policy = Case::new(vec![ready(SOL, "a1"), ready(TERRA, "a1")]);
    policy.config = serde_json::from_value(serde_json::json!({
        "pools": policy.config.pools,
        "model_policy": {"rules": [{"model": SOL, "policy": {"lane": {"read_only": true}}}]}
    }))
    .unwrap();
    policy.intent.writes = true;
    assert_eq!(
        policy.handoff(),
        Some((Rung::OtherProviderSameTier, route(TERRA, "a1")))
    );
    policy.intent.writes = false;
    assert_eq!(
        policy.handoff(),
        Some((Rung::OtherProviderSameTier, route(SOL, "a1")))
    );
    // A host-native sub-agent's model is held to the exclusions too.
    let mut subagent = Case::new(Vec::new());
    subagent.native = vec![HostNativeRoute {
        name: "fixer".into(),
        model: Some(model(OPUS)),
    }];
    assert!(subagent.handoff().is_some());
    subagent.exclusions = vec![rule(ExclusionSelector::Provider("claude".into()))];
    assert_eq!(subagent.step(), DepletionStep::Chair);
}

#[test]
fn a_failed_login_takes_the_whole_account_with_it() {
    let facts = vec![ready(OPUS, "a1")];
    let mut case = Case::new(facts);
    case.cause = DepletionCause::UsageLimited;
    assert_eq!(
        case.handoff(),
        Some((Rung::AdjacentTier, route(OPUS, "a1")))
    );
    case.cause = DepletionCause::AuthRequired;
    assert_eq!(case.step(), DepletionStep::Chair);
    case.cause = DepletionCause::ModelUnavailable;
    assert_eq!(
        case.handoff(),
        Some((Rung::AdjacentTier, route(OPUS, "a1")))
    );
}

#[test]
fn tried_targets_are_never_offered_twice() {
    let mut case = Case::new(vec![
        ready(SONNET, "a2"),
        ready(SONNET, "a3"),
        ready(SOL, "a1"),
        ready(TERRA, "a1"),
        ready(LUNA, "a1"),
        ready(OPUS, "a1"),
    ]);
    case.native = vec![native("one"), native("two")];
    let mut offered = Vec::new();
    while let DepletionStep::Handoff(handoff) = case.step() {
        assert!(!offered.contains(&handoff.target), "{:?}", handoff.target);
        offered.push(handoff.target.clone());
        case.tried.push(handoff.target);
        assert!(offered.len() <= 16, "the ladder must terminate");
    }
    assert_eq!(
        offered,
        vec![
            route(SONNET, "a2"),
            route(SONNET, "a3"),
            route(SOL, "a1"),
            route(TERRA, "a1"),
            route(LUNA, "a1"),
            route(OPUS, "a1"),
            Target::HostNative { name: "one".into() },
            Target::HostNative { name: "two".into() },
        ]
    );
    assert_eq!(case.step(), DepletionStep::Chair);
}

#[test]
fn policy_orders_the_rungs_and_the_adjacent_tiers() {
    let facts = vec![ready(SONNET, "a2"), ready(LUNA, "a1"), ready(OPUS, "a1")];
    let mut case = Case::new(facts);
    case.native = vec![native("fixer")];
    case.config.depletion.rungs = vec![Rung::HostNative, Rung::SameModelOtherAccount];
    assert_eq!(
        case.handoff(),
        Some((
            Rung::HostNative,
            Target::HostNative {
                name: "fixer".into()
            }
        ))
    );
    case.config.depletion.rungs = vec![Rung::AdjacentTier];
    case.config.depletion.adjacent = vec![TierStep::Higher, TierStep::Lower];
    assert_eq!(
        case.handoff(),
        Some((Rung::AdjacentTier, route(OPUS, "a1")))
    );
    case.config.depletion.adjacent = vec![TierStep::Lower];
    assert_eq!(
        case.handoff(),
        Some((Rung::AdjacentTier, route(LUNA, "a1")))
    );
    // The edge of the order has no neighbour beyond it.
    case.stopped.tier = Some("utility".into());
    case.config.depletion.adjacent = vec![TierStep::Lower];
    assert_eq!(case.step(), DepletionStep::Chair);
    // With no tier named, the first pool listing the model stands in.
    case.stopped.tier = None;
    case.config.depletion.adjacent = vec![TierStep::Higher];
    assert_eq!(
        case.handoff(),
        Some((Rung::AdjacentTier, route(OPUS, "a1")))
    );
}

#[test]
fn depleted_routes_do_not_park_a_ready_alternative_until_reset() {
    let mut case = Case::new(vec![
        fact(
            SONNET,
            "a1",
            Readiness::Depleted {
                resets_unix_seconds: Some(NOW + 300),
            },
        ),
        ready(SONNET, "a2"),
    ]);
    assert_eq!(
        case.handoff(),
        Some((Rung::SameModelOtherAccount, route(SONNET, "a2")))
    );
    case.facts.pop();
    assert_eq!(case.step(), DepletionStep::Chair);
}

#[test]
fn causes_map_from_stops_and_outcomes() {
    use provenant_records::StopCause as Stop;
    for (stop, cause) in [
        (Stop::UsageLimited, DepletionCause::UsageLimited),
        (Stop::ModelUnavailable, DepletionCause::ModelUnavailable),
        (Stop::AuthRequired, DepletionCause::AuthRequired),
    ] {
        assert_eq!(DepletionCause::from_stop(stop), Some(cause));
    }
    for stop in [
        Stop::ProviderStop,
        Stop::Blocked,
        Stop::Cancelled,
        Stop::AdapterUnavailable,
    ] {
        assert_eq!(DepletionCause::from_stop(stop), None);
    }
    assert_eq!(
        DepletionCause::from_outcome(FallbackOutcome::ModelUnavailable),
        Some(DepletionCause::ModelUnavailable)
    );
    assert_eq!(DepletionCause::from_outcome(FallbackOutcome::Other), None);
}

#[test]
fn bundled_policy_is_the_documented_ladder_and_validates() {
    let policy = DepletionPolicy::bundled();
    assert_eq!(
        policy.rungs,
        vec![
            Rung::SameModelOtherAccount,
            Rung::OtherProviderSameTier,
            Rung::AdjacentTier,
            Rung::HostNative
        ]
    );
    assert_eq!(policy.tiers, ["utility", "junior", "senior", "principal"]);
    assert_eq!(policy.adjacent, vec![TierStep::Lower, TierStep::Higher]);
    assert!(DomainConfig::validate(&RoutingConfig::default()).is_empty());
    let mut config = RoutingConfig::default();
    config.depletion.rungs = vec![Rung::AdjacentTier, Rung::AdjacentTier];
    config.depletion.tiers = vec!["a".into(), "a".into()];
    config.depletion.adjacent = vec![TierStep::Lower, TierStep::Lower];
    let fields: Vec<_> = DomainConfig::validate(&config)
        .into_iter()
        .map(|error| error.field)
        .collect();
    assert_eq!(
        fields,
        ["depletion.rungs", "depletion.tiers", "depletion.adjacent"]
    );
}

#[test]
fn depletion_facts_judge_each_account_with_the_dispatch_checks()
-> Result<(), Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({
        "pools": {"trial": [
            {"model": LUNA, "weight": 1, "efforts": ["high"]},
            {"model": SONNET, "weight": 1, "efforts": ["high"]}
        ]}
    }))?;
    let registry = [provenant_providers::Registration {
        quota_source: None,
        ..provenant_providers::codex::REGISTRATION
    }];
    let records = [
        availability(),
        AvailabilityRecord {
            account_key: Some("second".into()),
            ..availability()
        },
    ];
    let access = [AccessContext {
        adapter: "codex",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let snapshot = provenant_quota::QuotaSnapshot {
        schema_version: 1,
        account: provenant_quota::AccountScope {
            adapter: "codex".into(),
            account_key: "second".into(),
            label: "second".into(),
        },
        revision: 1,
        status: provenant_quota::QuotaStatus::Exhausted,
        observed_unix_seconds: 90,
        source_ref: "test".into(),
        authority_ref: None,
        windows: vec![provenant_quota::QuotaWindow {
            pool: "primary".into(),
            window_seconds: Some(18_000),
            used_basis_points: 10_000,
            resets_unix_seconds: Some(NOW + 600),
        }],
        details: None,
    };
    let quota = [QuotaObservation {
        model: model(LUNA),
        snapshot,
        expires_unix_seconds: NOW + 60,
    }];
    let mut context = context(&catalogue, &config, now()?);
    context.registry = &registry;
    context.availability = &records;
    context.access = &access;
    context.quota = &quota;
    let facts = depletion_facts(&context, RouteIntent::default(), &[])?;
    let state = |account: &str| {
        facts
            .iter()
            .find(|fact| fact.model == model(LUNA) && fact.account.as_deref() == Some(account))
            .map(|fact| fact.state)
    };
    assert_eq!(state("opaque-account"), Some(Readiness::Ready));
    assert!(matches!(
        state("second"),
        Some(Readiness::Depleted { resets_unix_seconds: Some(reset) }) if reset == NOW + 600
    ));
    // No claude account is registered, so that model has no fact: unknown.
    assert!(facts.iter().all(|fact| fact.model != model(SONNET)));
    Ok(())
}
