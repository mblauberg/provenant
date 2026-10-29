use crate::{BillingKind, Categories, PriceError, PriceIdentity, PriceTable};
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
    pub lane_cache: CacheSummary,
    pub chair_sessions: u64,
    /// Chair sessions whose transcript could not be read, so they add no tokens.
    pub chair_sessions_unread: u64,
    pub chair_usage: Categories<TokenTotal>,
    pub chair_cache: CacheSummary,
}

#[derive(Default)]
struct QuotaRow {
    lane: Accumulator,
    chair: Accumulator,
    sessions: u64,
    unread: u64,
}

/// Recorded lane and chair tokens, grouped by account label and project. Chair
/// tokens come from the sessions' transcripts (`attach_chair_usage`); a chair
/// session with none is counted as unread rather than as zero.
pub fn quota_usage(
    facts: impl IntoIterator<Item = AttemptFact>,
    chairs: impl IntoIterator<Item = crate::usage::ChairSession>,
    window: &Window,
) -> Result<Vec<AccountUsage>, TelemetryError> {
    if window.since.zip(window.until).is_some_and(|(s, u)| s >= u) {
        return Err(TelemetryError::InvalidWindow);
    }
    let mut rows = BTreeMap::<_, QuotaRow>::new();
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
        rows.entry(key).or_default().lane.record_usage(
            &fact.usage,
            Some(fact.terminal_state),
            0,
            false,
            false,
        )?;
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
            let row = rows.entry(key).or_default();
            add(&mut row.sessions, 1)?;
            add(&mut row.unread, u64::from(chair.unread.is_some()))?;
            for model in &chair.usage {
                row.chair
                    .record_usage(&model.usage, None, 0, false, false)?;
            }
        }
        if rows.len() as u64 > MAX_GROUPS {
            return Err(TelemetryError::TooLarge("10,000 groups"));
        }
    }
    Ok(rows
        .into_iter()
        .map(|((adapter, account, project), row)| AccountUsage {
            adapter,
            account,
            project,
            lane_attempts: row.lane.attempts,
            lane_usage: row.lane.usage,
            lane_cache: row.lane.cache.finish(),
            chair_sessions: row.sessions,
            chair_sessions_unread: row.unread,
            chair_usage: row.chair.usage,
            chair_cache: row.chair.cache.finish(),
        })
        .collect())
}

/// Cache economics for a group of usage records. Cache reads are the cheap
/// input, so their share of all input is the figure to raise.
///
/// Only records whose provider reported cache reads enter the share: counting
/// the input of a provider that reports no cache split would read as a miss.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CacheSummary {
    /// Records that reported cache reads.
    pub reporting: u64,
    /// Records that did not.
    pub missing: u64,
    /// Uncached input, cache reads and cache writes of the reporting records.
    pub input_tokens: u64,
    pub read_tokens: u64,
    pub write_tokens: u64,
    /// The long-TTL part of `write_tokens`, where the provider splits writes.
    pub write_1h_tokens: u64,
    /// Cache reads as a share of all input, in basis points.
    pub read_share_basis_points: Option<u64>,
}
impl CacheSummary {
    fn record(&mut self, usage: &Usage) -> Result<(), TelemetryError> {
        let Some(read) = usage.cache_read else {
            return add(&mut self.missing, 1);
        };
        add(&mut self.reporting, 1)?;
        add(&mut self.read_tokens, read)?;
        let write = usage.cache_write.unwrap_or(0);
        add(&mut self.write_tokens, write)?;
        add(&mut self.write_1h_tokens, usage.cache_write_1h.unwrap_or(0))?;
        let input = usage
            .uncached_input
            .unwrap_or(0)
            .checked_add(read)
            .and_then(|n| n.checked_add(write))
            .ok_or(TelemetryError::Overflow)?;
        add(&mut self.input_tokens, input)
    }
    fn merge(&mut self, other: &Self) -> Result<(), TelemetryError> {
        add(&mut self.reporting, other.reporting)?;
        add(&mut self.missing, other.missing)?;
        add(&mut self.input_tokens, other.input_tokens)?;
        add(&mut self.read_tokens, other.read_tokens)?;
        add(&mut self.write_tokens, other.write_tokens)?;
        add(&mut self.write_1h_tokens, other.write_1h_tokens)
    }
    fn finish(mut self) -> Self {
        self.read_share_basis_points = (self.input_tokens > 0).then(|| {
            // Reads are part of input, so the share cannot pass 10,000.
            u64::try_from(u128::from(self.read_tokens) * 10_000 / u128::from(self.input_tokens))
                .unwrap_or(10_000)
                .min(10_000)
        });
        self
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Money {
    pub microdollars: u64,
    pub usd: String,
}
impl Money {
    pub(crate) fn from_numerator(numerator: u128) -> Result<Self, TelemetryError> {
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
    pub cost_basis: &'static str,
    /// API-equivalent estimate, not a bill. Absent when any cost is unknown.
    pub complete: Option<Money>,
    /// Absent when no attempt is priced and no partial cost is known.
    pub known_lower_bound: Option<Money>,
    pub priced_attempts: u64,
    pub missing_attempts: u64,
    /// Attempts marked subscription by the model price table, not account billing.
    pub subscription_attempts: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Totals {
    pub attempts: u64,
    pub usage: Categories<TokenTotal>,
    pub cache: CacheSummary,
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
    /// Cache-read share, the figure to raise, by model, account, provider and
    /// source (lane or chair).
    pub cache: CacheRows,
    /// Present when the caller supplied chair sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chair: Option<ChairStats>,
}

#[derive(Default)]
struct Accumulator {
    attempts: u64,
    usage: Categories<TokenTotal>,
    cache: CacheSummary,
    numerator: u128,
    missing_cost: u64,
    subscription_attempts: u64,
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
        subscription: bool,
    ) -> Result<(), TelemetryError> {
        self.record_usage(
            &fact.usage,
            Some(fact.terminal_state),
            numerator,
            missing,
            subscription,
        )
    }
    /// One usage record: an attempt, or one model's total in a chair session
    /// (which has no outcome).
    fn record_usage(
        &mut self,
        usage: &Usage,
        outcome: Option<AttemptOutcome>,
        numerator: u128,
        missing: bool,
        subscription: bool,
    ) -> Result<(), TelemetryError> {
        add(&mut self.attempts, 1)?;
        for (total, count) in self.usage.values_mut().into_iter().zip(counts(usage)) {
            match count {
                Some(n) => add(&mut total.known, n)?,
                None => add(&mut total.missing, 1)?,
            }
        }
        self.cache.record(usage)?;
        self.numerator = self
            .numerator
            .checked_add(numerator)
            .ok_or(TelemetryError::Overflow)?;
        add(&mut self.missing_cost, u64::from(missing))?;
        add(&mut self.subscription_attempts, u64::from(subscription))?;
        if let Some(outcome) = outcome {
            add(self.outcomes.entry(outcome).or_default(), 1)?;
        }
        Ok(())
    }
    fn finish(self) -> Result<Totals, TelemetryError> {
        let known_lower_bound = Money::from_numerator(self.numerator)?;
        Ok(Totals {
            attempts: self.attempts,
            usage: self.usage,
            cache: self.cache.finish(),
            cost: CostTotal {
                cost_basis: "api_equivalent_estimate",
                complete: (self.missing_cost == 0).then(|| known_lower_bound.clone()),
                known_lower_bound: (self.attempts == 0
                    || self.missing_cost < self.attempts
                    || self.numerator > 0)
                    .then_some(known_lower_bound),
                priced_attempts: self.attempts - self.missing_cost,
                missing_attempts: self.missing_cost,
                subscription_attempts: self.subscription_attempts,
            },
            outcomes: self.outcomes,
        })
    }
}

/// What a usage record costs at the price table's rates.
struct Priced {
    numerator: u128,
    /// A positive category lacks its exact price.
    missing_price: bool,
    /// The cost is a lower bound: a count or a price is unknown.
    missing_cost: bool,
    subscription: bool,
    /// Per category, in `Categories` order, whether the count is absent.
    absent: [bool; 5],
}
/// Unknown routes and models have no rates. A validated table only holds
/// all-zero rates for verified-free entries.
///
/// Reasoning tokens use the output rate and every provider counts them inside
/// its output total, so an absent reasoning count leaves the cost exact.
fn priced(
    usage: &Usage,
    provider: Option<&str>,
    model: Option<&str>,
    prices: &PriceTable,
) -> Result<Priced, TelemetryError> {
    let price = provider.zip(model).and_then(|(p, m)| prices.get(p, m));
    let subscription = price.is_some_and(|(_, billing)| *billing == BillingKind::Subscription);
    let rates = price.map(|(rates, _)| rates.clone()).unwrap_or_default();
    let verified_free = rates.values() == [Some(0); 4];
    let mut out = Priced {
        numerator: 0,
        missing_price: false,
        missing_cost: false,
        subscription,
        absent: [false; 5],
    };
    for (index, (count, rate)) in counts(usage)
        .into_iter()
        .zip(rates.usage_rates())
        .enumerate()
    {
        match (count, rate) {
            (None, _) => {
                out.absent[index] = true;
                out.missing_cost |= !verified_free && index != REASONING;
            }
            (Some(0), _) => {}
            (Some(n), Some(r)) => {
                out.numerator = out
                    .numerator
                    .checked_add(u128::from(n) * u128::from(r))
                    .ok_or(TelemetryError::Overflow)?;
            }
            (Some(_), None) => {
                out.missing_price = true;
                out.missing_cost = true;
            }
        }
    }
    Ok(out)
}
/// Position of `reasoning` in [`Categories`] order.
const REASONING: usize = 4;

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
    stats_with_chair(facts, std::iter::empty(), prices, window)
}

/// As [`stats`], with the host's chair sessions beside the lanes. Chair
/// sessions are whatever the caller selected; they are not filtered by the
/// window's project, since a session spans projects.
pub fn stats_with_chair(
    facts: impl IntoIterator<Item = AttemptFact>,
    chairs: impl IntoIterator<Item = crate::usage::ChairSession>,
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
        let cost = priced(
            &fact.usage,
            key.provider.as_deref(),
            key.model.as_deref(),
            prices,
        )?;
        for (missing, absent) in coverage
            .missing_categories
            .values_mut()
            .into_iter()
            .zip(cost.absent)
        {
            add(missing, u64::from(absent))?;
        }
        add(
            &mut coverage.missing_usage,
            u64::from(cost.absent.iter().all(|absent| *absent)),
        )?;
        add(&mut coverage.missing_price, u64::from(cost.missing_price))?;
        add(
            &mut coverage.missing_account,
            u64::from(key.account_label.is_none()),
        )?;
        budget.group(&mut routes, key.clone(), bytes)?.record(
            &fact,
            cost.numerator,
            cost.missing_cost,
            cost.subscription,
        )?;
        if window.detail {
            let day = fact
                .finished_at
                .as_timestamp()
                .to_zoned(jiff::tz::TimeZone::UTC)
                .date()
                .to_string();
            let bytes = bytes + day.len();
            budget.group(&mut daily, (day, key), bytes)?.record(
                &fact,
                cost.numerator,
                cost.missing_cost,
                cost.subscription,
            )?;
        }
        let current = latest
            .entry((fact.project_id, fact.task_id))
            .or_insert((0, fact.terminal_state));
        if fact.attempt_number > current.0 {
            *current = (fact.attempt_number, fact.terminal_state);
        }
        total.record(&fact, cost.numerator, cost.missing_cost, cost.subscription)?;
    }
    coverage.selected_attempts = total.attempts;
    let chair = chair_stats(chairs, prices)?;
    let cache = cache_report(&routes, chair.as_ref())?;
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
        cache: cache.rows,
        chair: chair.map(|chair| chair.report),
    })
}

/// A chair account's use of one model, summed over its sessions.
#[derive(Clone, Debug, Serialize)]
pub struct ChairGroup {
    pub adapter: String,
    pub account: String,
    pub model: String,
    pub sessions: u64,
    pub usage: Categories<TokenTotal>,
    pub cache: CacheSummary,
    pub cost: CostTotal,
}
/// Chair sessions observed on this host by the SessionStart hook, with the
/// tokens read from their transcripts.
#[derive(Clone, Debug, Serialize)]
pub struct ChairStats {
    pub scope: &'static str,
    pub sessions: u64,
    /// Sessions with no token counts, by reason.
    pub unread: BTreeMap<crate::Unread, u64>,
    pub usage: Categories<TokenTotal>,
    pub cache: CacheSummary,
    pub cost: CostTotal,
    pub groups: Vec<ChairGroup>,
}
struct Chair {
    report: ChairStats,
    /// Unfinished per-group cache tallies, for the cross-source roll-up.
    cache: Vec<(String, String, String, CacheSummary)>,
}

fn chair_stats(
    chairs: impl IntoIterator<Item = crate::usage::ChairSession>,
    prices: &PriceTable,
) -> Result<Option<Chair>, TelemetryError> {
    type Key = (String, String, String);
    let mut groups = BTreeMap::<Key, (Accumulator, BTreeSet<String>)>::new();
    let mut total = Accumulator::default();
    let mut sessions = BTreeSet::new();
    let mut unread = BTreeMap::<crate::Unread, u64>::new();
    let mut seen = false;
    for chair in chairs {
        seen = true;
        if !sessions.insert((
            chair.adapter.clone(),
            chair.account.clone(),
            chair.session.clone(),
        )) {
            continue;
        }
        if let Some(reason) = chair.unread {
            add(unread.entry(reason).or_default(), 1)?;
        }
        for model in &chair.usage {
            let name = if model.model.contains('/') {
                model.model.clone()
            } else {
                format!("{}/{}", chair.adapter, model.model)
            };
            let cost = priced(&model.usage, Some(&chair.adapter), Some(&name), prices)?;
            let key = (chair.adapter.clone(), chair.account.clone(), name);
            if !groups.contains_key(&key) && groups.len() as u64 >= MAX_GROUPS {
                return Err(TelemetryError::TooLarge("10,000 groups"));
            }
            let (accumulator, ids) = groups.entry(key).or_default();
            ids.insert(chair.session.clone());
            for target in [accumulator, &mut total] {
                target.record_usage(
                    &model.usage,
                    None,
                    cost.numerator,
                    cost.missing_cost,
                    cost.subscription,
                )?;
            }
        }
    }
    if !seen {
        return Ok(None);
    }
    let mut cache = Vec::new();
    let mut out = Vec::new();
    for ((adapter, account, model), (accumulator, ids)) in groups {
        cache.push((
            adapter.clone(),
            account.clone(),
            model.clone(),
            accumulator.cache.clone(),
        ));
        let totals = accumulator.finish()?;
        out.push(ChairGroup {
            adapter,
            account,
            model,
            sessions: ids.len() as u64,
            usage: totals.usage,
            cache: totals.cache,
            cost: totals.cost,
        });
    }
    let totals = total.finish()?;
    Ok(Some(Chair {
        report: ChairStats {
            scope: "host_chair_sessions",
            sessions: sessions.len() as u64,
            unread,
            usage: totals.usage,
            cache: totals.cache,
            cost: totals.cost,
            groups: out,
        },
        cache,
    }))
}

/// One roll-up row of the cache-read share, from lanes or the chair.
#[derive(Clone, Debug, Serialize)]
pub struct CacheGroup {
    /// `lane` for dispatched attempts, `chair` for interactive sessions.
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub cache: CacheSummary,
}
/// Cache-read share per model, per account, per provider and per source.
#[derive(Clone, Debug, Serialize)]
pub struct CacheRows {
    pub by_model: Vec<CacheGroup>,
    pub by_account: Vec<CacheGroup>,
    pub by_provider: Vec<CacheGroup>,
    pub by_source: Vec<CacheGroup>,
}
struct CacheReport {
    rows: CacheRows,
}

fn cache_report(
    routes: &BTreeMap<RouteFact, Accumulator>,
    chair: Option<&Chair>,
) -> Result<CacheReport, TelemetryError> {
    type Key = (&'static str, Option<String>, Option<String>, Option<String>);
    let mut by_model = BTreeMap::<Key, CacheSummary>::new();
    let mut by_account = BTreeMap::<Key, CacheSummary>::new();
    let mut by_provider = BTreeMap::<Key, CacheSummary>::new();
    let mut by_source = BTreeMap::<Key, CacheSummary>::new();
    let mut fold = |source: &'static str,
                    provider: Option<&String>,
                    model: Option<&String>,
                    account: Option<&String>,
                    cache: &CacheSummary|
     -> Result<(), TelemetryError> {
        let keep = |value: Option<&String>| value.cloned();
        for (map, key) in [
            (&mut by_model, (source, keep(provider), keep(model), None)),
            (
                &mut by_account,
                (source, keep(provider), None, keep(account)),
            ),
            (&mut by_provider, (source, keep(provider), None, None)),
            (&mut by_source, (source, None, None, None)),
        ] {
            map.entry(key).or_default().merge(cache)?;
        }
        Ok(())
    };
    for (route, accumulator) in routes {
        fold(
            "lane",
            route.provider.as_ref(),
            route.model.as_ref(),
            route.account_label.as_ref(),
            &accumulator.cache,
        )?;
    }
    for (adapter, account, model, cache) in chair.iter().flat_map(|chair| &chair.cache) {
        fold("chair", Some(adapter), Some(model), Some(account), cache)?;
    }
    let rows = |map: BTreeMap<Key, CacheSummary>| -> Vec<CacheGroup> {
        map.into_iter()
            .map(|((source, provider, model, account), cache)| CacheGroup {
                source,
                provider,
                model,
                account,
                cache: cache.finish(),
            })
            .collect()
    };
    Ok(CacheReport {
        rows: CacheRows {
            by_model: rows(by_model),
            by_account: rows(by_account),
            by_provider: rows(by_provider),
            by_source: rows(by_source),
        },
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
