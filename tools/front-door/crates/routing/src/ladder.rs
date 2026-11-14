//! The depletion ladder: where work goes when its account or model is spent.
//!
//! A stopped attempt that ran out of quota, lost its model or lost its login
//! hands the work to another agent at once. The ladder is pure: it reads the catalogue, the
//! configuration and caller-supplied readiness facts, and performs no I/O.
//!
//! The rungs are tried in the order the policy lists them. By default:
//! the same model on another account; another provider or family at the same
//! tier; an adjacent tier; a host-native sub-agent where the harness declares
//! one. When no rung yields a candidate the answer is the chair.
//!
//! Exclusions are authority: a route a rule, a scheduled exclusion, enforced
//! model policy or confidentiality removes is never a candidate, and readiness
//! that was not observed is not ready.
use crate::{
    Catalogue, ExclusionRule, ExclusionSelector, FallbackOutcome, ModelId, RouteIntent,
    RoutingConfig,
};
use provenant_config::SectionError;
use provenant_core::WallTime;
use serde::{Deserialize, Serialize};

/// One step of the ladder, named so policy can order and omit them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rung {
    /// The stopped model, served by an account that has not stopped.
    SameModelOtherAccount,
    /// A model of the stopped tier from another provider or family.
    OtherProviderSameTier,
    /// A model of the next tier in the policy's order.
    AdjacentTier,
    /// A sub-agent the harness declares for itself.
    HostNative,
}

/// Which way from the stopped tier an adjacent tier lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TierStep {
    Lower,
    Higher,
}

/// Ladder order and tier adjacency. The bundled values are in
/// `assets/review-defaults.toml`; an empty `rungs` list hands every depletion
/// to the chair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DepletionPolicy {
    /// Rungs in the order they are tried.
    pub rungs: Vec<Rung>,
    /// Pool names from the lowest tier to the highest. Adjacent tiers are
    /// the neighbours of the stopped tier in this list.
    pub tiers: Vec<String>,
    /// Neighbours to try, in order, when a rung is [`Rung::AdjacentTier`].
    pub adjacent: Vec<TierStep>,
}

impl DepletionPolicy {
    /// The shipped ladder.
    pub fn bundled() -> Self {
        RoutingConfig::default().depletion
    }

    pub(crate) fn validate(&self, errors: &mut Vec<SectionError>) {
        let mut refuse = |field: &str| {
            errors.push(SectionError {
                detail: None,
                field: format!("depletion.{field}"),
                code: "invalid",
            });
        };
        if self.rungs.len() > 8 || has_duplicate(&self.rungs) {
            refuse("rungs");
        }
        if self.tiers.len() > 32
            || has_duplicate(&self.tiers)
            || self
                .tiers
                .iter()
                .any(|tier| !crate::request::identifier(tier))
        {
            refuse("tiers");
        }
        if self.adjacent.len() > 2 || has_duplicate(&self.adjacent) {
            refuse("adjacent");
        }
    }
}

fn has_duplicate<T: PartialEq>(items: &[T]) -> bool {
    items
        .iter()
        .enumerate()
        .any(|(index, item)| items[..index].contains(item))
}

/// Why the attempt stopped, narrowed to the causes the ladder serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepletionCause {
    /// A rate, quota or free-pool limit on the account.
    UsageLimited,
    /// The provider does not serve the model for this account.
    ModelUnavailable,
    /// The account's login is missing or expired.
    AuthRequired,
}

impl DepletionCause {
    pub fn from_stop(cause: provenant_records::StopCause) -> Option<Self> {
        use provenant_records::StopCause as Stop;
        match cause {
            Stop::UsageLimited => Some(Self::UsageLimited),
            Stop::ModelUnavailable => Some(Self::ModelUnavailable),
            Stop::AuthRequired => Some(Self::AuthRequired),
            _ => None,
        }
    }

    pub fn from_outcome(outcome: FallbackOutcome) -> Option<Self> {
        match outcome {
            FallbackOutcome::UsageLimited => Some(Self::UsageLimited),
            FallbackOutcome::ModelUnavailable => Some(Self::ModelUnavailable),
            FallbackOutcome::AuthRequired => Some(Self::AuthRequired),
            FallbackOutcome::AdapterUnavailable | FallbackOutcome::Other => None,
        }
    }
}

/// The route that stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub model: ModelId,
    /// Account key. `None` when the route had no known account.
    pub account: Option<String>,
    pub effort: Option<String>,
    /// The pool the route was drawn from. When absent, the first tier in the
    /// policy's order whose pool lists the model stands in.
    pub tier: Option<String>,
    /// The catalogued answering family, `None` when the catalogue names none.
    pub family: Option<String>,
}

impl Stopped {
    /// Describe a resolved route. `tier` is the named route or pool it came
    /// from, if it came from one.
    pub fn from_resolved(
        route: &provenant_records::ResolvedRoute,
        tier: Option<String>,
        catalogue: &Catalogue,
    ) -> Result<Self, crate::RoutingError> {
        let model = ModelId::parse(&format!("{}/{}", route.adapter, route.model))?;
        Ok(Self {
            family: family_of(catalogue, &model),
            model,
            account: route.account_key.clone(),
            effort: route.effort.clone(),
            tier,
        })
    }
}

fn family_of(catalogue: &Catalogue, model: &ModelId) -> Option<String> {
    catalogue
        .model(model)
        .and_then(|entry| entry.family.clone())
}

/// What one model on one account can do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    /// Spent. `resets_unix_seconds` is when every spent window has reset, when
    /// the quota reading says.
    Depleted {
        resets_unix_seconds: Option<i64>,
    },
    /// Refused or not launchable for another reason.
    Unavailable,
}

impl Readiness {
    /// Project a resolved route's readiness. A route that was never observed
    /// carries the unknown projection, which is not ready.
    pub fn from_route(readiness: &provenant_records::RouteReadiness) -> Self {
        match readiness {
            provenant_records::RouteReadiness::Ready { .. } => Self::Ready,
            provenant_records::RouteReadiness::Unavailable { .. } => Self::Unavailable,
        }
    }
}

/// A readiness observation for one model on one account. A model and account
/// with no fact are unknown, and unknown is not ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessFact {
    pub model: ModelId,
    pub account: Option<String>,
    pub state: Readiness,
}

/// A sub-agent the harness declares, such as a `domains.fabric.host_native`
/// entry. Its model, when known, is subject to the same exclusions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostNativeRoute {
    pub name: String,
    pub model: Option<ModelId>,
}

/// One thing work can be handed to, and the key of one thing already tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Route {
        model: ModelId,
        account: Option<String>,
    },
    HostNative {
        name: String,
    },
}

/// A hand-off the ladder chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    pub rung: Rung,
    pub target: Target,
    pub effort: Option<String>,
    /// The tier the target was found in, for a route on a tiered rung.
    pub tier: Option<String>,
}

/// The ladder's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepletionStep {
    /// Give the work to this agent now.
    Handoff(Handoff),
    /// No route is eligible: the chair decides.
    Chair,
}

/// Everything the ladder reads. Borrowed, so the caller assembles it once per
/// stop.
pub struct DepletionInput<'a> {
    pub catalogue: &'a Catalogue,
    /// Pools, scheduled exclusions, model policy and the ladder policy.
    pub config: &'a RoutingConfig,
    pub stopped: &'a Stopped,
    pub cause: DepletionCause,
    /// Readiness per model and account. Anything absent is unknown.
    pub facts: &'a [ReadinessFact],
    /// Targets this node already ran or was offered. The stopped route counts.
    pub tried: &'a [Target],
    pub host_native: &'a [HostNativeRoute],
    /// Standing, graph and flag exclusions. They bind every rung.
    pub exclusions: &'a [ExclusionRule],
    pub intent: RouteIntent,
    pub confidential: bool,
    pub now: WallTime,
}

/// Choose where the stopped work goes next.
pub fn next_after_depletion(input: &DepletionInput<'_>) -> DepletionStep {
    input
        .config
        .depletion
        .rungs
        .iter()
        .find_map(|rung| rung_handoff(input, *rung))
        .map_or(DepletionStep::Chair, DepletionStep::Handoff)
}

fn rung_handoff(input: &DepletionInput<'_>, rung: Rung) -> Option<Handoff> {
    match rung {
        Rung::SameModelOtherAccount => same_model_other_account(input),
        Rung::OtherProviderSameTier => other_provider_same_tier(input),
        Rung::AdjacentTier => adjacent_tier(input),
        Rung::HostNative => host_native(input),
    }
}

fn same_model_other_account(input: &DepletionInput<'_>) -> Option<Handoff> {
    let stopped = input.stopped;
    let account = input
        .facts
        .iter()
        .filter(|fact| fact.model == stopped.model && fact.account != stopped.account)
        .find(|fact| eligible(input, &fact.model, fact.account.as_deref()))?
        .account
        .clone();
    Some(Handoff {
        rung: Rung::SameModelOtherAccount,
        target: Target::Route {
            model: stopped.model.clone(),
            account,
        },
        effort: stopped.effort.clone(),
        tier: stopped_tier(input),
    })
}

fn other_provider_same_tier(input: &DepletionInput<'_>) -> Option<Handoff> {
    let tier = stopped_tier(input)?;
    let stopped = input.stopped;
    pool_handoff(input, Rung::OtherProviderSameTier, &tier, |model| {
        model.adapter() != stopped.model.adapter() || {
            let family = family_of(input.catalogue, model);
            family.is_some() && family != stopped.family
        }
    })
}

fn adjacent_tier(input: &DepletionInput<'_>) -> Option<Handoff> {
    let tiers = &input.config.depletion.tiers;
    let stopped = stopped_tier(input)?;
    let at = tiers.iter().position(|tier| *tier == stopped)?;
    input
        .config
        .depletion
        .adjacent
        .iter()
        .filter_map(|step| match step {
            TierStep::Lower => at.checked_sub(1),
            TierStep::Higher => Some(at + 1),
        })
        .filter_map(|index| tiers.get(index))
        .find_map(|tier| pool_handoff(input, Rung::AdjacentTier, tier, |_| true))
}

fn host_native(input: &DepletionInput<'_>) -> Option<Handoff> {
    let native = input.host_native.iter().find(|native| {
        let target = Target::HostNative {
            name: native.name.clone(),
        };
        !input.tried.contains(&target)
            && native
                .model
                .as_ref()
                .is_none_or(|model| permitted(input, model, None))
    })?;
    Some(Handoff {
        rung: Rung::HostNative,
        target: Target::HostNative {
            name: native.name.clone(),
        },
        effort: None,
        tier: None,
    })
}

/// The first eligible member of `tier` that `qualifies`, heaviest first.
fn pool_handoff(
    input: &DepletionInput<'_>,
    rung: Rung,
    tier: &str,
    qualifies: impl Fn(&ModelId) -> bool,
) -> Option<Handoff> {
    let members = pool(input.catalogue, input.config, tier)?;
    let mut order: Vec<_> = members.iter().collect();
    order.sort_by_key(|member| std::cmp::Reverse(member.weight));
    order
        .into_iter()
        .filter(|member| member.model != input.stopped.model && qualifies(&member.model))
        .find_map(|member| {
            let fact = input.facts.iter().find(|fact| {
                fact.model == member.model && eligible(input, &fact.model, fact.account.as_deref())
            })?;
            Some(Handoff {
                rung,
                target: Target::Route {
                    model: member.model.clone(),
                    account: fact.account.clone(),
                },
                effort: member.effort.clone(),
                tier: Some(tier.to_owned()),
            })
        })
}

/// The pool a tier names: configuration first, then the bundled catalogue.
pub(crate) fn pool<'a>(
    catalogue: &'a Catalogue,
    config: &'a RoutingConfig,
    name: &str,
) -> Option<&'a Vec<crate::RoutePoolMember>> {
    config.pools.get(name).or_else(|| catalogue.pools.get(name))
}

fn stopped_tier(input: &DepletionInput<'_>) -> Option<String> {
    input.stopped.tier.clone().or_else(|| {
        input
            .config
            .depletion
            .tiers
            .iter()
            .find(|tier| {
                pool(input.catalogue, input.config, tier).is_some_and(|members| {
                    members
                        .iter()
                        .any(|member| member.model == input.stopped.model)
                })
            })
            .cloned()
    })
}

/// A route the ladder may offer: observed ready, not tried, not the account
/// whose login failed, and permitted.
fn eligible(input: &DepletionInput<'_>, model: &ModelId, account: Option<&str>) -> bool {
    let stopped = input.stopped;
    let target = Target::Route {
        model: model.clone(),
        account: account.map(str::to_owned),
    };
    let stopped_target = Target::Route {
        model: stopped.model.clone(),
        account: stopped.account.clone(),
    };
    if target == stopped_target || input.tried.contains(&target) {
        return false;
    }
    // A failed login is the account's, whatever model it served.
    if input.cause == DepletionCause::AuthRequired
        && model.adapter() == stopped.model.adapter()
        && account == stopped.account.as_deref()
    {
        return false;
    }
    let ready = input.facts.iter().any(|fact| {
        fact.model == *model && fact.account.as_deref() == account && fact.state == Readiness::Ready
    });
    ready && permitted(input, model, account)
}

/// Exclusions, enforced model policy and confidentiality. None of them has an
/// override: an excluded route is never a candidate.
fn permitted(input: &DepletionInput<'_>, model: &ModelId, account: Option<&str>) -> bool {
    let adapter = model.adapter();
    let excluded_by_rule = input.exclusions.iter().any(|rule| match &rule.selector {
        ExclusionSelector::Provider(provider) => provider == adapter,
        ExclusionSelector::Model(excluded) => excluded == model,
        ExclusionSelector::Account { adapter: a, key } => {
            a == adapter && account == Some(key.as_str())
        }
    });
    let excluded_by_config = input
        .config
        .model_exclusions
        .iter()
        .any(|exclusion| exclusion.model == *model && exclusion.schedule.active(input.now));
    !excluded_by_rule
        && !excluded_by_config
        && (!input.confidential
            || input
                .catalogue
                .model(model)
                .is_some_and(|entry| entry.trains_on_prompts == Some(false)))
        && input
            .config
            .model_policy
            .effective(model, account, input.now)
            .policy
            .permits(input.intent)
}
