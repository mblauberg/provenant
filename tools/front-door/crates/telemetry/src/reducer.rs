use crate::{Categories, PriceError, PriceIdentity, PriceTable};
use provenant_core::{AttemptId, ProjectId, TaskId, WallTime};
use provenant_records::{AttemptFact, AttemptOutcome, Usage};
use serde::Serialize;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};
use thiserror::Error;

pub const MAX_ATTEMPTS: u64 = 100_000;
pub const MAX_GROUPS: u64 = 10_000;
pub const MAX_KEY_BYTES: u64 = 1_048_576;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TelemetryError {
    #[error("telemetry aggregation exceeds {0}")]
    TooLarge(&'static str),
    #[error("invalid terminal attempt or selection: {0}")]
    InvalidFact(&'static str),
    #[error(transparent)]
    InvalidPrice(#[from] PriceError),
    #[error("telemetry arithmetic overflow")]
    Overflow,
}
pub type Stats = Result<StatsReport, TelemetryError>;

/// Explicit selection supplied by CLI. Bounds are already parsed UTC instants.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Window {
    pub since: Option<WallTime>,
    pub until: Option<WallTime>,
    pub project: Option<ProjectId>,
    pub detail: bool,
}
impl Window {
    fn validate(&self) -> Result<(), TelemetryError> {
        if self.since.zip(self.until).is_some_and(|(s, u)| s >= u) {
            return Err(TelemetryError::InvalidFact("since must precede until"));
        }
        Ok(())
    }
    fn includes(&self, fact: &AttemptFact) -> bool {
        self.since.is_none_or(|s| fact.finished_at >= s)
            && self.until.is_none_or(|u| fact.finished_at < u)
            && self.project.is_none_or(|p| fact.project_id == p)
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TokenTotal {
    pub known: u64,
    pub missing: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Money {
    pub microdollars: u64,
    pub usd: String,
}
impl Money {
    fn from_numerator(numerator: u128) -> Result<Self, TelemetryError> {
        // Quotient/remainder avoids overflowing the numerator when rounding.
        let rounded = (numerator / 1_000_000)
            .checked_add(u128::from(numerator % 1_000_000 >= 500_000))
            .ok_or(TelemetryError::Overflow)?;
        let microdollars = u64::try_from(rounded).map_err(|_| TelemetryError::Overflow)?;
        Ok(Self {
            microdollars,
            usd: format!(
                "{}.{:06}",
                microdollars / 1_000_000,
                microdollars % 1_000_000
            ),
        })
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct CostTotal {
    /// API-equivalent estimate, not a bill. Absent when any cost is unknown.
    pub complete: Option<Money>,
    pub known_lower_bound: Money,
    pub missing_attempts: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Totals {
    pub attempts: u64,
    pub usage: Categories<TokenTotal>,
    pub cost: CostTotal,
    pub outcomes: BTreeMap<String, u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct RouteKey {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account_label: Option<String>,
}
impl RouteKey {
    fn from_fact(fact: &AttemptFact) -> Self {
        let route = fact.resolved_route.as_ref();
        Self {
            provider: route.and_then(|r| r.provider.clone()),
            model: route.and_then(|r| r.model.clone()),
            effort: route.and_then(|r| r.effort.clone()),
            account_label: route.and_then(|r| r.account_label.clone()),
        }
    }
    fn fields(&self) -> [&Option<String>; 4] {
        [
            &self.provider,
            &self.model,
            &self.effort,
            &self.account_label,
        ]
    }
    fn bytes(&self) -> Result<u64, TelemetryError> {
        self.fields().iter().try_fold(0u64, |sum, s| {
            let bytes = u64::try_from(s.as_ref().map_or(0, String::len))
                .map_err(|_| TelemetryError::Overflow)?;
            sum.checked_add(bytes).ok_or(TelemetryError::Overflow)
        })
    }
    fn report_order(&self, other: &Self) -> Ordering {
        for (a, b) in self.fields().into_iter().zip(other.fields()) {
            let ordering = a.is_none().cmp(&b.is_none()).then_with(|| a.cmp(b));
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct RouteGroup {
    pub route: RouteKey,
    pub totals: Totals,
}
#[derive(Clone, Debug, Serialize)]
pub struct DailyGroup {
    pub day: String,
    pub route: RouteKey,
    pub totals: Totals,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Coverage {
    pub selected_attempts: u64,
    /// Attempts with no measured category at all.
    pub missing_usage: u64,
    pub missing_categories: Categories<u64>,
    /// Attempts with a known positive category lacking its exact price.
    pub missing_price: u64,
    pub missing_account: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct StatsReport {
    pub scope: &'static str,
    pub window: Window,
    pub prices: PriceIdentity,
    pub totals: Totals,
    pub routes: Vec<RouteGroup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily_accounts: Option<Vec<DailyGroup>>,
    /// Highest numbered selected terminal attempt per task; never active work.
    pub latest_task_outcomes: BTreeMap<String, u64>,
    pub coverage: Coverage,
}

#[derive(Default)]
struct Accumulator {
    attempts: u64,
    usage: Categories<TokenTotal>,
    numerator: u128,
    missing_cost: u64,
    outcomes: BTreeMap<String, u64>,
}
fn add(value: &mut u64, increment: u64) -> Result<(), TelemetryError> {
    *value = value
        .checked_add(increment)
        .ok_or(TelemetryError::Overflow)?;
    Ok(())
}
fn outcome(state: AttemptOutcome) -> &'static str {
    use AttemptOutcome::*;
    match state {
        Succeeded => "succeeded",
        Failed => "failed",
        Rejected => "rejected",
        Cancelled => "cancelled",
        TimedOut => "timed_out",
        RateLimited => "rate_limited",
        QuotaLimited => "quota_limited",
        AuthRequired => "auth_required",
        ModelUnavailable => "model_unavailable",
        PermissionDenied => "permission_denied",
        Quarantined => "quarantined",
        Lost => "lost",
        InputRequired => "input_required",
    }
}
fn counts(usage: &Usage) -> [Option<u64>; 5] {
    [
        usage.uncached_input,
        usage.cache_read,
        usage.cache_write,
        usage.output_excluding_reasoning,
        usage.reasoning,
    ]
}
impl Accumulator {
    fn record(
        &mut self,
        fact: &AttemptFact,
        numerator: u128,
        missing: bool,
    ) -> Result<(), TelemetryError> {
        add(&mut self.attempts, 1)?;
        for (total, count) in self.usage.values_mut().into_iter().zip(counts(&fact.usage)) {
            match count {
                Some(n) => add(&mut total.known, n)?,
                None => add(&mut total.missing, 1)?,
            }
        }
        self.numerator = self
            .numerator
            .checked_add(numerator)
            .ok_or(TelemetryError::Overflow)?;
        add(&mut self.missing_cost, u64::from(missing))?;
        add(
            self.outcomes
                .entry(outcome(fact.terminal_state).into())
                .or_default(),
            1,
        )
    }
    fn finish(self) -> Result<Totals, TelemetryError> {
        let known_lower_bound = Money::from_numerator(self.numerator)?;
        Ok(Totals {
            attempts: self.attempts,
            usage: self.usage,
            cost: CostTotal {
                complete: (self.missing_cost == 0).then(|| known_lower_bound.clone()),
                known_lower_bound,
                missing_attempts: self.missing_cost,
            },
            outcomes: self.outcomes,
        })
    }
}

#[derive(Default)]
struct GroupBudget {
    groups: u64,
    bytes: u64,
}
impl GroupBudget {
    fn reserve(&mut self, bytes: u64) -> Result<(), TelemetryError> {
        let new_bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or(TelemetryError::Overflow)?;
        if self.groups >= MAX_GROUPS {
            return Err(TelemetryError::TooLarge("10,000 groups"));
        }
        if new_bytes > MAX_KEY_BYTES {
            return Err(TelemetryError::TooLarge("1 MiB of group keys"));
        }
        self.groups += 1;
        self.bytes = new_bytes;
        Ok(())
    }
}

/// Reduces selected facts without I/O. Invalid duplicates are refused, never
/// silently deduplicated; retries with distinct IDs/numbers each contribute once.
/// Run owns production filtering and read failures; CLI owns the output byte cap.
pub fn stats(
    facts: impl IntoIterator<Item = AttemptFact>,
    prices: &PriceTable,
    window: Window,
) -> Stats {
    window.validate()?;
    let mut total = Accumulator::default();
    let mut routes = BTreeMap::<RouteKey, Accumulator>::new();
    let mut daily = BTreeMap::<(String, RouteKey), Accumulator>::new();
    let mut attempts = BTreeSet::<AttemptId>::new();
    let mut numbers = BTreeSet::new();
    let mut latest = BTreeMap::<(ProjectId, TaskId), (u64, AttemptOutcome)>::new();
    let mut coverage = Coverage::default();
    let mut budget = GroupBudget::default();
    for fact in facts {
        if !window.includes(&fact) {
            continue;
        }
        if total.attempts >= MAX_ATTEMPTS {
            return Err(TelemetryError::TooLarge("100,000 selected attempts"));
        }
        if fact.started_at > fact.finished_at || fact.attempt_number == 0 {
            return Err(TelemetryError::InvalidFact(
                "time order or zero attempt number",
            ));
        }
        if !attempts.insert(fact.attempt_id)
            || !numbers.insert((fact.project_id, fact.task_id, fact.attempt_number))
        {
            return Err(TelemetryError::InvalidFact(
                "duplicate attempt ID or task/attempt number",
            ));
        }
        // Check retained bytes before cloning a potentially large route key.
        let route_ref = fact.resolved_route.as_ref();
        let fields = route_ref.map(|r| [&r.provider, &r.model, &r.effort, &r.account_label]);
        let bytes = fields.into_iter().flatten().try_fold(0u64, |sum, field| {
            if field.as_ref().is_some_and(|s| s.trim().is_empty()) {
                return Err(TelemetryError::InvalidFact("empty route identity"));
            }
            sum.checked_add(
                u64::try_from(field.as_ref().map_or(0, String::len))
                    .map_err(|_| TelemetryError::Overflow)?,
            )
            .ok_or(TelemetryError::Overflow)
        })?;
        if bytes > MAX_KEY_BYTES {
            return Err(TelemetryError::TooLarge("1 MiB of group keys"));
        }
        let key = RouteKey::from_fact(&fact);
        let price = key
            .provider
            .as_deref()
            .zip(key.model.as_deref())
            .and_then(|(p, m)| prices.get(p, m));
        let unknown_rates = Categories::default();
        let rates = price.map_or(&unknown_rates, |p| &p.rates);
        let mut numerator = 0u128;
        let mut missing_price = false;
        let mut missing_cost = false;
        for ((count, rate), missing) in counts(&fact.usage)
            .into_iter()
            .zip(rates.values())
            .zip(coverage.missing_categories.values_mut())
        {
            match (count, rate) {
                (None, _) => {
                    add(missing, 1)?;
                    missing_cost = true;
                }
                (Some(0), _) => {}
                (Some(n), Some(r)) => {
                    numerator = numerator
                        .checked_add(u128::from(n) * u128::from(*r))
                        .ok_or(TelemetryError::Overflow)?;
                }
                (Some(_), None) => {
                    missing_price = true;
                    missing_cost = true;
                }
            }
        }
        add(
            &mut coverage.missing_usage,
            u64::from(counts(&fact.usage).iter().all(Option::is_none)),
        )?;
        add(&mut coverage.missing_price, u64::from(missing_price))?;
        add(
            &mut coverage.missing_account,
            u64::from(key.account_label.is_none()),
        )?;
        if !routes.contains_key(&key) {
            budget.reserve(key.bytes()?)?;
        }
        routes
            .entry(key.clone())
            .or_default()
            .record(&fact, numerator, missing_cost)?;
        if window.detail {
            let day = fact
                .finished_at
                .as_timestamp()
                .to_zoned(jiff::tz::TimeZone::UTC)
                .date()
                .to_string();
            let daily_key = (day, key);
            if !daily.contains_key(&daily_key) {
                budget.reserve(
                    bytes
                        .checked_add(
                            u64::try_from(daily_key.0.len())
                                .map_err(|_| TelemetryError::Overflow)?,
                        )
                        .ok_or(TelemetryError::Overflow)?,
                )?;
            }
            daily
                .entry(daily_key)
                .or_default()
                .record(&fact, numerator, missing_cost)?;
        }
        let current = latest
            .entry((fact.project_id, fact.task_id))
            .or_insert((fact.attempt_number, fact.terminal_state));
        if fact.attempt_number > current.0 {
            *current = (fact.attempt_number, fact.terminal_state);
        }
        total.record(&fact, numerator, missing_cost)?;
    }
    coverage.selected_attempts = total.attempts;
    let mut latest_task_outcomes = BTreeMap::new();
    for (_, state) in latest.into_values() {
        add(
            latest_task_outcomes
                .entry(outcome(state).into())
                .or_default(),
            1,
        )?;
    }
    let mut routes = routes
        .into_iter()
        .map(|(route, total)| {
            Ok(RouteGroup {
                route,
                totals: total.finish()?,
            })
        })
        .collect::<Result<Vec<_>, TelemetryError>>()?;
    routes.sort_by(|a, b| a.route.report_order(&b.route));
    let daily_accounts = if window.detail {
        let mut groups = daily
            .into_iter()
            .map(|((day, route), total)| {
                Ok(DailyGroup {
                    day,
                    route,
                    totals: total.finish()?,
                })
            })
            .collect::<Result<Vec<_>, TelemetryError>>()?;
        groups.sort_by(|a, b| {
            a.day
                .cmp(&b.day)
                .then_with(|| a.route.report_order(&b.route))
        });
        Some(groups)
    } else {
        None
    };
    Ok(StatsReport {
        scope: "supervised_terminal_attempts",
        window,
        prices: prices.identity.clone(),
        totals: total.finish()?,
        routes,
        daily_accounts,
        latest_task_outcomes,
        coverage,
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
