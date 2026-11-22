//! The one resolve-then-admit-then-narrow loop used by route preview, dispatch
//! and launch. Every consumer reads the same ordered admitted candidates and
//! the same typed exclusions, so the three cannot disagree about what a route
//! can serve in a given quota state.

use super::resolution::record_pool_redraw;
use super::*;
use provenant_records::{ResolvedRoute, RouteExclusionReason};

/// Why an otherwise routed candidate was withheld.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdmissionExclusion {
    pub model: ModelId,
    pub reason: provenant_records::RouteExclusionReason,
    pub detail: String,
}

/// The ordered admission result: the candidates routing offers that admission
/// accepts, and every candidate it refused with its typed cause.
#[derive(Debug, Default)]
pub(crate) struct AdmittedRoutes {
    pub candidates: Vec<ResolvedRoute>,
    pub exclusions: Vec<AdmissionExclusion>,
    /// The admission error that ended the search, when one did. Callers that
    /// refuse rather than warn turn it into their own error shape.
    pub refusal: Option<EdgeError>,
    /// The last candidate admission refused. A read-only preview still has to
    /// display a route when every member is blocked; dispatch never uses this.
    pub last_refused: Option<ResolvedRoute>,
}

impl AdmittedRoutes {
    /// Dispatch takes the first admitted candidate in routing order. One
    /// refusal is the original error; several are reported together so every
    /// redraw's typed cause reaches the caller.
    pub(crate) fn into_first(self) -> Result<ResolvedRoute, EdgeError> {
        match self.candidates.into_iter().next() {
            Some(route) => Ok(route),
            None => match self.refusal {
                Some(_) if self.exclusions.len() > 1 => Err(EdgeError::Admission(
                    self.exclusions
                        .iter()
                        .map(|exclusion| exclusion.detail.clone())
                        .collect::<Vec<_>>()
                        .join("; "),
                )),
                Some(error) => Err(error),
                None => Err(EdgeError::Admission("no eligible pool member".into())),
            },
        }
    }

    /// The router's own refusal when it offered nothing for admission to judge:
    /// no candidate and no exclusion. Preparation turns it into its typed
    /// route-resolution refusal so a login or account classification survives;
    /// a search that admission emptied reports its exclusions instead.
    pub(crate) fn router_refusal(&mut self) -> Option<EdgeError> {
        if self.candidates.is_empty() && self.exclusions.is_empty() {
            self.refusal.take()
        } else {
            None
        }
    }

    /// Whether the first candidate has live provider readiness. Family
    /// relaxation uses this to avoid trading one unavailable route for another.
    pub(crate) fn first_ready(&self) -> bool {
        self.candidates
            .first()
            .is_some_and(|route| matches!(route.readiness, RouteReadiness::Ready { .. }))
    }
}

/// How admission judges a routed candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdmitMode {
    /// Dispatch and launch: judge the account routing selected, but only once
    /// its provider readiness is known. Preview has no probe of its own.
    SelectedAccount,
    /// A read-only preview: no account is selected, so every cached account of
    /// the candidate's provider is probed as if it were ready. A snapshot that
    /// says the window is spent still cannot serve.
    CachedAccounts,
}

/// The narrowing a caller applies before asking routing for the next pick.
pub(crate) struct Narrowing<'a> {
    pub excluded: &'a [ModelId],
    pub refused_accounts: &'a [(String, String)],
}

/// Caller-specific resolution settings that do not change the admission rules.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AdmissionOptions<'a> {
    pub account: Option<&'a str>,
    pub mode: AdmitMode,
    /// Models already ruled out before the search starts (family separation,
    /// models tried in an earlier launch step).
    pub initial_excluded: &'a [ModelId],
    /// Dispatch and launch may move to another account of the same model; a
    /// read-only preview judges every cached account at once and does not.
    pub narrow_accounts: bool,
}

/// Quota facts one resolution admits against. Built once from the same cache
/// and policies for every candidate in the search.
pub(crate) struct AdmissionFacts<'a> {
    pub freshness: &'a provenant_quota::FreshnessPolicy,
    pub policy: &'a provenant_records::policy::Policy,
    pub policies: &'a [provenant_quota::QuotaPolicy],
    pub now_unix_seconds: i64,
    pub records: &'a [provenant_quota::AccountRecord],
    pub catalogue: &'a Catalogue,
}

impl AdmissionFacts<'_> {
    fn admission<'a>(&'a self, pools: &'a [String]) -> provenant_quota::Admission<'a> {
        provenant_quota::Admission {
            freshness: self.freshness,
            policy: self.policy,
            policies: self.policies,
            now_unix_seconds: self.now_unix_seconds,
            records: self.records,
            pools,
        }
    }
}

/// A snapshot whose status says spent and whose window has not reset still
/// cannot serve, even when the rest of admission would allow it.
pub(super) fn spent(record: &provenant_quota::AccountRecord, now: i64) -> bool {
    record.snapshot.as_ref().is_some_and(|snapshot| {
        snapshot.status == provenant_quota::QuotaStatus::Exhausted
            && snapshot.observed_unix_seconds <= now
            && snapshot.windows.iter().any(|window| {
                window.used_basis_points >= 10_000
                    && window.resets_unix_seconds.is_none_or(|reset| reset > now)
            })
    })
}

/// The typed cause of an admission error, shared by redraw receipts and skips.
pub(crate) fn exclusion_reason(error: &provenant_quota::QuotaError) -> RouteExclusionReason {
    match error {
        provenant_quota::QuotaError::AllowanceExhausted => RouteExclusionReason::Exhausted,
        provenant_quota::QuotaError::Refused("uncalibrated partial coverage") => {
            RouteExclusionReason::UnknownPartialHeadroom
        }
        _ => RouteExclusionReason::AdmissionRefused,
    }
}

/// A refusal, its typed cause and the account it belongs to when one was
/// selected. The search uses the account to redraw before dropping the model.
struct Refusal {
    reason: RouteExclusionReason,
    detail: String,
    account: Option<(String, String)>,
    // Boxed: the refusal travels in every judged candidate's Result.
    error: Box<EdgeError>,
}

fn judge(
    route: &ResolvedRoute,
    facts: &AdmissionFacts<'_>,
    mode: AdmitMode,
    account: Option<&str>,
) -> Result<(), Refusal> {
    let model =
        ModelId::parse(&format!("{}/{}", route.adapter, route.model)).map_err(|error| Refusal {
            reason: RouteExclusionReason::UnverifiedModel,
            detail: error.to_string(),
            account: None,
            error: Box::new(EdgeError::from(error)),
        })?;
    let pools = facts.catalogue.admission_pools(&model);
    let admission = facts.admission(&pools);
    match mode {
        AdmitMode::SelectedAccount => {
            if !matches!(route.readiness, RouteReadiness::Ready { .. }) {
                return Ok(());
            }
            admission.check(route).map_err(|error| Refusal {
                reason: exclusion_reason(&error),
                detail: error.to_string(),
                account: route
                    .account_key
                    .clone()
                    .map(|key| (route.adapter.clone(), key)),
                error: Box::new(EdgeError::from(error)),
            })
        }
        AdmitMode::CachedAccounts => {
            let mut members = Vec::new();
            for record in facts
                .records
                .iter()
                .filter(|record| record.account.adapter == route.adapter)
                // An account pin narrows the preview the way it narrows
                // dispatch: another account's free allowance cannot mask a
                // spent pinned one.
                .filter(|record| {
                    account.is_none_or(|pin| {
                        let (adapter, name) = pin
                            .split_once('/')
                            .map_or((None, pin), |(adapter, name)| (Some(adapter), name));
                        adapter.is_none_or(|adapter| adapter == record.account.adapter)
                            && (record.account.account_key == name || record.account.label == name)
                    })
                })
            {
                let mut probe = route.clone();
                probe.account_key = Some(record.account.account_key.clone());
                probe.readiness = RouteReadiness::Ready {
                    availability_warning: None,
                    extra: Default::default(),
                    availability_ref: "preview".into(),
                    expires_unix_seconds: facts.now_unix_seconds.saturating_add(60),
                };
                match admission.check(&probe) {
                    Ok(()) if !spent(record, facts.now_unix_seconds) => return Ok(()),
                    Ok(()) => members.push("allowance exhausted".to_owned()),
                    Err(error) => members.push(error.to_string()),
                }
            }
            if members.is_empty() {
                // Nothing is known about this provider: it may run once
                // dispatch probes it, so a preview cannot block it.
                return Ok(());
            }
            Err(Refusal {
                reason: RouteExclusionReason::AdmissionRefused,
                detail: format!("{}/{}: {}", route.adapter, route.model, members.join("; ")),
                account: None,
                error: Box::new(EdgeError::Quota(provenant_quota::QuotaError::Refused(
                    "provider readiness",
                ))),
            })
        }
    }
}

/// Resolve one admission-aware search. `resolve` is called with the models and
/// accounts already ruled out; every caller narrows its own availability from
/// that state. The first candidate admission accepts is returned in order and
/// every candidate it refused is recorded with a typed reason.
pub(crate) fn resolve_admitted(
    request: &provenant_routing::RouteRequest,
    facts: &AdmissionFacts<'_>,
    options: AdmissionOptions<'_>,
    mut resolve: impl FnMut(&Narrowing<'_>) -> Result<ResolvedRoute, EdgeError>,
) -> Result<AdmittedRoutes, EdgeError> {
    let mut excluded: Vec<ModelId> = options.initial_excluded.to_vec();
    let mut refused_accounts: Vec<(String, String)> = Vec::new();
    let mut result = AdmittedRoutes::default();
    // The redraw already resolved for a refused pool member. It carries the
    // redraw receipt, so the next iteration judges it instead of resolving a
    // fresh copy that would lose the receipt.
    let mut queued: Option<ResolvedRoute> = None;
    loop {
        let next = match queued.take() {
            Some(route) => Ok(route),
            None => resolve(&Narrowing {
                excluded: &excluded,
                refused_accounts: &refused_accounts,
            }),
        };
        let route = match next {
            Ok(route) => route,
            Err(error @ EdgeError::Routing(RoutingError::NoEligiblePoolMember { .. })) => {
                if result.refusal.is_none() {
                    result.refusal = Some(error);
                }
                break;
            }
            Err(error) if !result.exclusions.is_empty() => {
                if result.refusal.is_none() {
                    result.refusal = Some(error);
                }
                break;
            }
            Err(error) => return Err(error),
        };
        let model = ModelId::parse(&format!("{}/{}", route.adapter, route.model))?;
        // A resolver that ignores exclusions (a single-model route) returns
        // the same candidate again; every member has then been judged.
        if excluded.contains(&model) {
            break;
        }
        match judge(&route, facts, options.mode, options.account) {
            Ok(()) => {
                let admitted_model = model.clone();
                let has_pool = route.pool_selection.is_some();
                result.candidates.push(route);
                // Keep walking the pool in routing order: every member that
                // can serve is an admitted candidate, and callers that only
                // need one take the first. A route without a pool has no next
                // member; a single-model route stops on the cycle guard.
                if !matches!(request.choice, RouteChoice::NamedRoute(_)) || !has_pool {
                    break;
                }
                excluded.push(admitted_model);
            }
            Err(refusal) => {
                result.last_refused = Some(route.clone());
                result.exclusions.push(AdmissionExclusion {
                    model: model.clone(),
                    reason: refusal.reason.clone(),
                    detail: refusal.detail.clone(),
                });
                let earlier_refusal = result.refusal.replace(*refusal.error);
                // A spent account leaves the model servable by another
                // registered account. Unless the caller pinned one, try the
                // same model there before giving it up.
                if options.narrow_accounts
                    && options.account.is_none()
                    && let Some(account) = refusal.account.clone()
                    && !refused_accounts.contains(&account)
                {
                    refused_accounts.push(account);
                    if let Ok(replacement) = resolve(&Narrowing {
                        excluded: &excluded,
                        refused_accounts: &refused_accounts,
                    }) {
                        let replacement_model = ModelId::parse(&format!(
                            "{}/{}",
                            replacement.adapter, replacement.model
                        ))?;
                        if replacement_model == model
                            && replacement.account_key != route.account_key
                            && matches!(replacement.readiness, RouteReadiness::Ready { .. })
                            && judge(&replacement, facts, options.mode, options.account).is_ok()
                        {
                            // The model is servable on another account, so it
                            // is not an exclusion: preparation would otherwise
                            // reject the model the resolver just recovered.
                            result.exclusions.pop();
                            result.refusal = earlier_refusal;
                            result.candidates.push(replacement);
                            break;
                        }
                    }
                }
                // An exact model pin has no next model; only a named pool
                // redraws, and only while it has an untried member.
                if !matches!(request.choice, RouteChoice::NamedRoute(_))
                    || route.pool_selection.is_none()
                {
                    break;
                }
                excluded.push(model.clone());
                let mut replacement = match resolve(&Narrowing {
                    excluded: &excluded,
                    refused_accounts: &refused_accounts,
                }) {
                    Ok(replacement) => replacement,
                    Err(error @ EdgeError::Routing(RoutingError::NoEligiblePoolMember { .. })) => {
                        if result.refusal.is_none() {
                            result.refusal = Some(error);
                        }
                        break;
                    }
                    Err(error) => return Err(error),
                };
                let replacement_model =
                    ModelId::parse(&format!("{}/{}", replacement.adapter, replacement.model))?;
                if replacement_model == model {
                    break;
                }
                record_pool_redraw(&route, &mut replacement, &refusal.detail)?;
                if let Some(redraw) = replacement
                    .pool_selection
                    .as_mut()
                    .and_then(|selection| selection.redraws.last_mut())
                {
                    redraw.cause = Some(refusal.reason);
                }
                queued = Some(replacement);
            }
        }
    }
    Ok(result)
}
