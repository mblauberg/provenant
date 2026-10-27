use crate::{Categories, PriceError, PriceIdentity, PriceTable};
use provenant_core::{AttemptId, ProjectId, TaskId, WallTime};
use provenant_records::{AttemptFact, AttemptOutcome, RouteFact, Usage};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use thiserror::Error;

pub const MAX_ATTEMPTS: u64 = 100_000;
pub const MAX_GROUPS: u64 = 10_000;
pub const MAX_KEY_BYTES: usize = 1_048_576;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TelemetryError {
    #[error("telemetry aggregation exceeds {0}")]
    TooLarge(&'static str),
    #[error("invalid terminal attempt or selection: {0}")]
    InvalidFact(&'static str),
    #[error("since must precede until")]
    InvalidWindow,
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

#[derive(Debug, Serialize)]
pub struct AccountUsage {
    pub adapter: Option<String>,
    pub account: Option<String>,
    pub project: Option<ProjectId>,
    pub lane_attempts: u64,
    pub lane_usage: Categories<TokenTotal>,
    pub chair_sessions: u64,
}

/// Recorded lane tokens and observed chair session counts, grouped by account
/// label and project. Chair records have no token measurements.
pub fn quota_usage(
    facts: impl IntoIterator<Item = AttemptFact>,
    chairs: impl IntoIterator<Item = crate::usage::ChairSession>,
    window: &Window,
) -> Result<Vec<AccountUsage>, TelemetryError> {
    if window.since.zip(window.until).is_some_and(|(s, u)| s >= u) {
        return Err(TelemetryError::InvalidWindow);
    }
    let mut rows = BTreeMap::<_, (Accumulator, u64)>::new();
    let mut attempts = BTreeSet::new();
    for fact in facts {
        if !window.includes(&fact) {
            continue;
        }
        if attempts.len() as u64 >= MAX_ATTEMPTS {
            return Err(TelemetryError::TooLarge("100,000 selected attempts"));
        }
        if !attempts.insert(fact.attempt_id) {
            return Err(TelemetryError::InvalidFact("duplicate attempt ID"));
        }
        let route = fact.resolved_route.clone().unwrap_or(RouteFact {
            provider: None,
            model: None,
            effort: None,
            account_label: None,
        });
        let key = (route.provider, route.account_label, Some(fact.project_id));
        rows.entry(key).or_default().0.record(&fact, 0, false)?;
        if rows.len() as u64 > MAX_GROUPS {
            return Err(TelemetryError::TooLarge("10,000 groups"));
        }
    }
    let mut sessions = BTreeSet::new();
    for chair in chairs {
        if window.project.is_some_and(|p| Some(p) != chair.project) {
            continue;
        }
        let key = (Some(chair.adapter), Some(chair.account), chair.project);
        if sessions.insert((key.clone(), chair.session)) {
            add(&mut rows.entry(key).or_default().1, 1)?;
        }
        if rows.len() as u64 > MAX_GROUPS {
            return Err(TelemetryError::TooLarge("10,000 groups"));
        }
    }
    Ok(rows
        .into_iter()
        .map(
            |((adapter, account, project), (lane, chair_sessions))| AccountUsage {
                adapter,
                account,
                project,
                lane_attempts: lane.attempts,
                lane_usage: lane.usage,
                chair_sessions,
            },
        )
        .collect())
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
    pub outcomes: BTreeMap<AttemptOutcome, u64>,
}
fn route_fields(route: &RouteFact) -> [&Option<String>; 4] {
    [
        &route.provider,
        &route.model,
        &route.effort,
        &route.account_label,
    ]
}
/// Exact field order with missing values last.
fn report_key(route: &RouteFact) -> [(bool, &Option<String>); 4] {
    route_fields(route).map(|field| (field.is_none(), field))
}
#[derive(Clone, Debug, Serialize)]
pub struct RouteGroup {
    pub route: RouteFact,
    pub totals: Totals,
}
#[derive(Clone, Debug, Serialize)]
pub struct DailyGroup {
    pub day: String,
    pub route: RouteFact,
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
    pub latest_task_outcomes: BTreeMap<AttemptOutcome, u64>,
    pub coverage: Coverage,
}

#[derive(Default)]
struct Accumulator {
    attempts: u64,
    usage: Categories<TokenTotal>,
    numerator: u128,
    missing_cost: u64,
    outcomes: BTreeMap<AttemptOutcome, u64>,
}
fn add(value: &mut u64, increment: u64) -> Result<(), TelemetryError> {
    *value = value
        .checked_add(increment)
        .ok_or(TelemetryError::Overflow)?;
    Ok(())
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
        add(self.outcomes.entry(fact.terminal_state).or_default(), 1)
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
    bytes: usize,
}
impl GroupBudget {
    /// Reserves a new group's key bytes before inserting it. Key lengths are of
    /// live strings and the total stays at most 1 MiB, so sums cannot overflow.
    fn group<'a, K: Ord>(
        &mut self,
        map: &'a mut BTreeMap<K, Accumulator>,
        key: K,
        bytes: usize,
    ) -> Result<&'a mut Accumulator, TelemetryError> {
        match map.entry(key) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                if self.groups >= MAX_GROUPS {
                    return Err(TelemetryError::TooLarge("10,000 groups"));
                }
                if self.bytes + bytes > MAX_KEY_BYTES {
                    return Err(TelemetryError::TooLarge("1 MiB of group keys"));
                }
                self.groups += 1;
                self.bytes += bytes;
                Ok(entry.insert(Accumulator::default()))
            }
        }
    }
}

fn groups<K, G>(
    map: BTreeMap<K, Accumulator>,
    group: impl Fn(K, Totals) -> G,
) -> Result<Vec<G>, TelemetryError> {
    map.into_iter()
        .map(|(key, total)| Ok(group(key, total.finish()?)))
        .collect()
}

/// Reduces selected facts without I/O. Invalid duplicates are refused, never
/// silently deduplicated; retries with distinct IDs/numbers each contribute once.
/// Run owns production filtering and read failures; CLI owns the output byte cap.
pub fn stats(
    facts: impl IntoIterator<Item = AttemptFact>,
    prices: &PriceTable,
    window: Window,
) -> Stats {
    if window.since.zip(window.until).is_some_and(|(s, u)| s >= u) {
        return Err(TelemetryError::InvalidWindow);
    }
    let mut total = Accumulator::default();
    let mut routes = BTreeMap::<RouteFact, Accumulator>::new();
    let mut daily = BTreeMap::<(String, RouteFact), Accumulator>::new();
    let mut attempts = BTreeSet::<AttemptId>::new();
    let mut numbers = BTreeSet::new();
    let mut latest = BTreeMap::<(ProjectId, TaskId), (u64, AttemptOutcome)>::new();
    let mut coverage = Coverage::default();
    let mut budget = GroupBudget::default();
    for mut fact in facts {
        if !window.includes(&fact) {
            continue;
        }
        if total.attempts >= MAX_ATTEMPTS {
            return Err(TelemetryError::TooLarge("100,000 selected attempts"));
        }
        if fact
            .started_at
            .is_some_and(|start| start > fact.finished_at)
            || fact.queued_at > fact.finished_at
            || fact.attempt_number == 0
        {
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
        let fields = fact.resolved_route.iter().flat_map(route_fields).flatten();
        if fields.clone().any(|field| field.trim().is_empty()) {
            return Err(TelemetryError::InvalidFact("empty route identity"));
        }
        let bytes: usize = fields.map(String::len).sum();
        let key = fact.resolved_route.take().unwrap_or(RouteFact {
            provider: None,
            model: None,
            effort: None,
            account_label: None,
        });
        // Unknown routes and models have no rates. A validated table only holds
        // all-zero rates for verified-free entries.
        let rates = key
            .provider
            .as_deref()
            .zip(key.model.as_deref())
            .and_then(|(p, m)| prices.get(p, m))
            .cloned()
            .unwrap_or_default();
        let verified_free = rates.values() == [Some(0); 4];
        let counts = counts(&fact.usage);
        let mut numerator = 0u128;
        let mut missing_price = false;
        let mut missing_cost = false;
        for ((count, rate), missing) in counts
            .into_iter()
            .zip(rates.usage_rates())
            .zip(coverage.missing_categories.values_mut())
        {
            match (count, rate) {
                (None, _) => {
                    add(missing, 1)?;
                    missing_cost |= !verified_free;
                }
                (Some(0), _) => {}
                (Some(n), Some(r)) => {
                    numerator = numerator
                        .checked_add(u128::from(n) * u128::from(r))
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
            u64::from(counts.iter().all(Option::is_none)),
        )?;
        add(&mut coverage.missing_price, u64::from(missing_price))?;
        add(
            &mut coverage.missing_account,
            u64::from(key.account_label.is_none()),
        )?;
        budget
            .group(&mut routes, key.clone(), bytes)?
            .record(&fact, numerator, missing_cost)?;
        if window.detail {
            let day = fact
                .finished_at
                .as_timestamp()
                .to_zoned(jiff::tz::TimeZone::UTC)
                .date()
                .to_string();
            let bytes = bytes + day.len();
            budget
                .group(&mut daily, (day, key), bytes)?
                .record(&fact, numerator, missing_cost)?;
        }
        let current = latest
            .entry((fact.project_id, fact.task_id))
            .or_insert((0, fact.terminal_state));
        if fact.attempt_number > current.0 {
            *current = (fact.attempt_number, fact.terminal_state);
        }
        total.record(&fact, numerator, missing_cost)?;
    }
    coverage.selected_attempts = total.attempts;
    let mut latest_task_outcomes = BTreeMap::new();
    for (_, state) in latest.into_values() {
        add(latest_task_outcomes.entry(state).or_default(), 1)?;
    }
    let mut routes = groups(routes, |route, totals| RouteGroup { route, totals })?;
    routes.sort_by(|a, b| report_key(&a.route).cmp(&report_key(&b.route)));
    let mut daily = groups(daily, |(day, route), totals| DailyGroup {
        day,
        route,
        totals,
    })?;
    daily.sort_by(|a, b| (&a.day, report_key(&a.route)).cmp(&(&b.day, report_key(&b.route))));
    Ok(StatsReport {
        scope: "supervised_terminal_attempts",
        prices: prices.identity.clone(),
        totals: total.finish()?,
        routes,
        daily_accounts: window.detail.then_some(daily),
        window,
        latest_task_outcomes,
        coverage,
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
