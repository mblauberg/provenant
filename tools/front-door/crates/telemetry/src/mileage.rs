//! Mileage and effective token rates from quota readings and ingested usage.
//!
//! Mileage is the API-equivalent cost of one percentage point of a quota
//! window between two distinct readings inside one reset period; capacity is
//! the cost of a full window. A reset, an advance of the reset time of a minute
//! or more, a fall in the used percentage or a plan change ends a run, so no
//! segment is negative. All money is integer microdollars and is rounded once.
use serde::Serialize;

/// The five disjoint usage buckets, in [`Reading::usage`] order.
pub const BUCKETS: [&str; 5] = crate::ingest::BUCKETS;
/// Least squares is reported only when it explains at least this much.
pub const MIN_FIT_R2: f64 = 0.5;
/// A reset time that advances by less than this is the same window reporting
/// its reset with jitter, not a new one.
const RESET_TOLERANCE_SECONDS: i64 = 60;
/// The note on every effective-rate report. Token use is not shown to cover
/// all of the account's usage, so it is a bound.
const BOUND_NOTE: &str = "token use is a lower bound and realised rates are upper bounds, because ingest coverage of the account is not checked";

#[derive(Clone, Debug)]
pub struct Reading {
    pub observed_unix_seconds: i64,
    pub used_basis_points: u64,
    pub resets_unix_seconds: Option<i64>,
    pub plan: Option<String>,
    pub tier: Option<String>,
    /// API-equivalent cost of the usage between the previous reading and this
    /// one, or none when any of that usage is unpriced.
    pub cost_microdollars: Option<u64>,
    /// Tokens used between the previous reading and this one, by bucket.
    pub usage: [u64; 5],
    /// Whether every token in the interval had a known price.
    pub priced: bool,
}

#[derive(Clone, Debug)]
pub struct Series {
    pub account_key: String,
    pub label: String,
    pub provider: String,
    pub window: String,
    /// Length of the window, when the provider reports it.
    pub window_seconds: Option<u64>,
    /// Oldest first; only distinct readings belong here.
    pub readings: Vec<Reading>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Runs needed on each side of a step change before it is judged.
    pub min_runs: u32,
    /// Percentage fall in capacity that is flagged rather than inconclusive.
    pub fall_percent: u32,
    /// Days in the recent side of a step change.
    pub recent_days: u32,
    /// Days in the prior side of a step change.
    pub prior_days: u32,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            min_runs: 10,
            fall_percent: 20,
            recent_days: 7,
            prior_days: 28,
        }
    }
}

/// An exact microdollar amount with its decimal form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Money {
    pub microdollars: u64,
    pub usd: String,
}
impl Money {
    pub fn new(microdollars: u64) -> Self {
        Self {
            microdollars,
            usd: format!(
                "{}.{:06}",
                microdollars / 1_000_000,
                microdollars % 1_000_000
            ),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    /// Median cost of one percentage point, over priced segments.
    pub mileage: Option<Money>,
    /// Median cost of a full window: mileage times 100.
    pub capacity: Option<Money>,
    /// Segments in the window, priced or not.
    pub n: u64,
    pub priced: u64,
    /// Priced segments as a share of `n`, in basis points.
    pub priced_share_basis_points: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RunView {
    pub start_unix_seconds: i64,
    pub end_unix_seconds: i64,
    /// Why the run began: absent for the first, else `reported`, `inferred`
    /// (a drop with no reset time) or `plan_change`.
    pub reset: Option<&'static str>,
    pub plan: Option<String>,
    pub tier: Option<String>,
    pub readings: u64,
    pub segments: u64,
    pub priced_segments: u64,
    pub mileage: Option<Money>,
    pub capacity: Option<Money>,
}

#[derive(Clone, Debug, Serialize)]
pub struct StepChange {
    pub status: &'static str,
    /// Fall in capacity, in percent; negative is a rise.
    pub percent: Option<f64>,
    pub recent_n: u64,
    pub prior_n: u64,
    pub recent_days: u32,
    pub prior_days: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct Upgrade {
    pub plan_from: Option<String>,
    pub plan_to: Option<String>,
    pub before: Option<Money>,
    pub after: Option<Money>,
    /// Rise in capacity, in percent; negative is a fall.
    pub percent: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SeriesReport {
    pub account_key: String,
    pub label: String,
    pub provider: String,
    pub window: String,
    pub window_seconds: Option<u64>,
    pub mileage: Summary,
    pub runs: Vec<RunView>,
    pub step_change: StepChange,
    pub upgrade: Option<Upgrade>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Segment {
    end_at: i64,
    /// `cost_microdollars * 100`: microdollars per percentage point when
    /// divided by `used_basis_points`.
    num: u128,
    den: u128,
    priced: bool,
}

#[derive(Clone, Debug)]
struct Run {
    start_at: i64,
    reset: Option<&'static str>,
    plan: Option<String>,
    tier: Option<String>,
    readings: u64,
    segments: Vec<Segment>,
}

/// Why `curr` starts a new run rather than extending the one `prev` is in.
fn boundary(prev: &Reading, curr: &Reading) -> Option<&'static str> {
    let known = |a: &Option<String>, b: &Option<String>| match (a, b) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    };
    if known(&prev.plan, &curr.plan) || known(&prev.tier, &curr.tier) {
        return Some("plan_change");
    }
    if let (Some(before), Some(after)) = (prev.resets_unix_seconds, curr.resets_unix_seconds)
        && after.saturating_sub(before) >= RESET_TOLERANCE_SECONDS
    {
        return Some("reported");
    }
    if curr.used_basis_points < prev.used_basis_points {
        // A drop with no advancing reset time is a reset the provider did not
        // report; segmentation is the evidence, and it is flagged as inferred.
        return Some("inferred");
    }
    None
}

fn partition(readings: &[Reading]) -> Vec<Run> {
    let Some(first) = readings.first() else {
        return Vec::new();
    };
    let mut run = Run {
        start_at: first.observed_unix_seconds,
        reset: None,
        plan: first.plan.clone(),
        tier: first.tier.clone(),
        readings: 1,
        segments: Vec::new(),
    };
    let mut runs = Vec::new();
    for pair in readings.windows(2) {
        let (prev, curr) = (&pair[0], &pair[1]);
        if let Some(reason) = boundary(prev, curr) {
            runs.push(run);
            run = Run {
                start_at: curr.observed_unix_seconds,
                reset: Some(reason),
                plan: curr.plan.clone(),
                tier: curr.tier.clone(),
                readings: 1,
                segments: Vec::new(),
            };
            continue;
        }
        run.readings += 1;
        if curr.used_basis_points > prev.used_basis_points {
            run.segments.push(Segment {
                end_at: curr.observed_unix_seconds,
                num: u128::from(curr.cost_microdollars.unwrap_or(0)) * 100,
                den: u128::from(curr.used_basis_points - prev.used_basis_points),
                priced: curr.priced && curr.cost_microdollars.is_some(),
            });
        }
    }
    runs.push(run);
    runs
}

fn round_div(numerator: u128, denominator: u128) -> Option<u64> {
    if denominator == 0 {
        return None;
    }
    let rounded = numerator
        .checked_add(denominator / 2)?
        .checked_div(denominator)?;
    u64::try_from(rounded).ok()
}

/// Median ratio as microdollars per percentage point, rounded once.
fn median_mileage(segments: impl Iterator<Item = Segment>) -> Option<Money> {
    let mut priced: Vec<Segment> = segments.filter(|segment| segment.priced).collect();
    if priced.is_empty() {
        return None;
    }
    priced.sort_by(|a, b| (a.num * b.den).cmp(&(b.num * a.den)));
    let middle = priced.len() / 2;
    let (num, den) = if priced.len() % 2 == 1 {
        (priced[middle].num, priced[middle].den)
    } else {
        let (a, b) = (priced[middle - 1], priced[middle]);
        (a.num * b.den + b.num * a.den, 2 * a.den * b.den)
    };
    round_div(num, den).map(Money::new)
}

fn capacity_of(segments: impl Iterator<Item = Segment>) -> Option<Money> {
    let mut priced: Vec<Segment> = segments.filter(|segment| segment.priced).collect();
    if priced.is_empty() {
        return None;
    }
    priced.sort_by(|a, b| (a.num * b.den).cmp(&(b.num * a.den)));
    let middle = priced.len() / 2;
    let (num, den) = if priced.len() % 2 == 1 {
        (priced[middle].num, priced[middle].den)
    } else {
        let (a, b) = (priced[middle - 1], priced[middle]);
        (a.num * b.den + b.num * a.den, 2 * a.den * b.den)
    };
    // Capacity is 100 percentage points.
    round_div(num.checked_mul(100)?, den).map(Money::new)
}

fn summary(segments: &[Segment], since: i64) -> Summary {
    let selected: Vec<Segment> = segments
        .iter()
        .copied()
        .filter(|segment| segment.end_at >= since)
        .collect();
    let n = selected.len() as u64;
    let priced = selected.iter().filter(|segment| segment.priced).count() as u64;
    Summary {
        mileage: median_mileage(selected.iter().copied()),
        capacity: capacity_of(selected.iter().copied()),
        n,
        priced,
        priced_share_basis_points: (n > 0).then(|| priced * 10_000 / n),
    }
}

fn median_of(mut values: Vec<u64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[middle] as f64)
    } else {
        Some((values[middle - 1] as f64 + values[middle] as f64) / 2.0)
    }
}

fn step_change(readings: &[Reading], latest: i64, options: Options) -> StepChange {
    let day = 86_400;
    let recent_start = latest - i64::from(options.recent_days) * day;
    let prior_start = recent_start - i64::from(options.prior_days) * day;
    let last = readings.last();
    let mut recent = Vec::new();
    let mut prior = Vec::new();
    for pair in readings.windows(2) {
        let (prev, curr) = (&pair[0], &pair[1]);
        if boundary(prev, curr).is_some() || curr.used_basis_points <= prev.used_basis_points {
            continue;
        }
        let same_plan = last.is_none_or(|last| curr.plan == last.plan && curr.tier == last.tier);
        if !same_plan {
            continue;
        }
        let Some(cost) = curr.cost_microdollars.filter(|_| curr.priced) else {
            continue;
        };
        let Some(capacity) = round_div(
            u128::from(cost) * 10_000,
            u128::from(curr.used_basis_points - prev.used_basis_points),
        ) else {
            continue;
        };
        if curr.observed_unix_seconds > recent_start {
            recent.push(capacity);
        } else if curr.observed_unix_seconds > prior_start {
            prior.push(capacity);
        }
    }
    let (recent_n, prior_n) = (recent.len() as u64, prior.len() as u64);
    let (recent_capacity, prior_capacity) = (median_of(recent), median_of(prior));
    let minimum = u64::from(options.min_runs);
    let percent = match (recent_capacity, prior_capacity) {
        (Some(recent), Some(prior)) if prior > 0.0 => Some((prior - recent) / prior * 100.0),
        _ => None,
    };
    let status = match percent {
        Some(percent)
            if recent_n >= minimum
                && prior_n >= minimum
                && percent >= f64::from(options.fall_percent) =>
        {
            "flagged"
        }
        _ => "inconclusive",
    };
    StepChange {
        status,
        percent,
        recent_n,
        prior_n,
        recent_days: options.recent_days,
        prior_days: options.prior_days,
    }
}

fn upgrade(runs: &[Run]) -> Option<Upgrade> {
    let index = runs
        .iter()
        .rposition(|run| run.reset == Some("plan_change"))?;
    let before = runs.get(index.checked_sub(1)?)?;
    let after = runs.get(index)?;
    let before_capacity = capacity_of(before.segments.iter().copied());
    let after_capacity = capacity_of(after.segments.iter().copied());
    let percent = match (&before_capacity, &after_capacity) {
        (Some(before), Some(after)) if before.microdollars > 0 => Some(
            (after.microdollars as f64 - before.microdollars as f64) / before.microdollars as f64
                * 100.0,
        ),
        _ => None,
    };
    Some(Upgrade {
        plan_from: before.plan.clone(),
        plan_to: after.plan.clone(),
        before: before_capacity,
        after: after_capacity,
        percent,
    })
}

/// Analyse one account's readings for one quota window.
pub fn analyse(series: &Series, options: Options) -> SeriesReport {
    let runs = partition(&series.readings);
    let segments: Vec<Segment> = runs
        .iter()
        .flat_map(|run| run.segments.iter().copied())
        .collect();
    let latest = series
        .readings
        .last()
        .map_or(0, |reading| reading.observed_unix_seconds);
    let since = latest - i64::from(options.recent_days) * 86_400;
    SeriesReport {
        account_key: series.account_key.clone(),
        label: series.label.clone(),
        provider: series.provider.clone(),
        window: series.window.clone(),
        window_seconds: series.window_seconds,
        mileage: summary(&segments, since),
        runs: runs
            .iter()
            .map(|run| RunView {
                start_unix_seconds: run.start_at,
                end_unix_seconds: run
                    .segments
                    .last()
                    .map_or(run.start_at, |segment| segment.end_at),
                reset: run.reset,
                plan: run.plan.clone(),
                tier: run.tier.clone(),
                readings: run.readings,
                segments: run.segments.len() as u64,
                priced_segments: run.segments.iter().filter(|s| s.priced).count() as u64,
                mileage: median_mileage(run.segments.iter().copied()),
                capacity: capacity_of(run.segments.iter().copied()),
            })
            .collect(),
        step_change: step_change(&series.readings, latest, options),
        upgrade: upgrade(&runs),
    }
}

/// Whether `curr` extends the run `prev` is in and raises the used amount, so
/// the pair is one mileage segment. The CLI uses this to fit weights.
pub fn continues(prev: &Reading, curr: &Reading) -> bool {
    boundary(prev, curr).is_none() && curr.used_basis_points > prev.used_basis_points
}

/// API rates per million tokens for an ingested row, in [`BUCKETS`] order.
/// The one-hour cache write uses the cache-write rate, as elsewhere.
pub fn api_rates(
    prices: &crate::PriceTable,
    provider: &str,
    model: &str,
) -> Option<[Option<u64>; 5]> {
    let prefix = format!("{provider}/");
    let canonical = if model.starts_with(&prefix) {
        model.to_owned()
    } else {
        format!("{prefix}{model}")
    };
    let (rates, _) = prices.get(provider, &canonical)?;
    let [uncached_input, cache_read, cache_write, output] = rates.values();
    Some([uncached_input, cache_read, cache_write, cache_write, output])
}

/// One observation for the fitted weights: tokens by bucket and the rise in
/// used percentage points over the interval, in basis points.
pub type Observation = ([u64; 5], u64);

#[derive(Clone, Debug, Serialize)]
pub struct Fit {
    /// Non-negative basis points of the window consumed per million tokens.
    pub weights_basis_points_per_million_tokens: [f64; 5],
    pub n: u64,
    pub r_squared: f64,
}

fn dot(left: &[f64; 5], right: &[f64; 5]) -> f64 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

/// Non-negative least squares of `delta used = sum w_b * tokens_b`. Reported
/// only when there are at least `min_n` observations and the fit explains at
/// least [`MIN_FIT_R2`] of the variance.
pub fn fit(rows: &[Observation], min_n: u32) -> Option<Fit> {
    let n = rows.len();
    if n < usize::try_from(min_n).ok()?.max(1) {
        return None;
    }
    let x: Vec<[f64; 5]> = rows
        .iter()
        .map(|(tokens, _)| tokens.map(|count| count as f64))
        .collect();
    let y: Vec<f64> = rows.iter().map(|(_, used)| *used as f64).collect();
    let mut weights = [0.0_f64; 5];
    for _ in 0..500 {
        let mut moved = false;
        for column in 0..5 {
            let numerator: f64 = (0..n)
                .map(|row| {
                    let partial = dot(&x[row], &weights) - weights[column] * x[row][column];
                    x[row][column] * (y[row] - partial)
                })
                .sum();
            let denominator: f64 = x.iter().map(|row| row[column] * row[column]).sum();
            let next = if denominator > 0.0 {
                (numerator / denominator).max(0.0)
            } else {
                0.0
            };
            if (next - weights[column]).abs() > 1e-12 {
                weights[column] = next;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    let mean = y.iter().sum::<f64>() / n as f64;
    let sse: f64 = (0..n)
        .map(|row| (y[row] - dot(&x[row], &weights)).powi(2))
        .sum();
    let sst: f64 = y.iter().map(|value| (value - mean).powi(2)).sum();
    let r_squared = if sst > 0.0 { 1.0 - sse / sst } else { 0.0 };
    if !r_squared.is_finite() || r_squared < MIN_FIT_R2 {
        return None;
    }
    Some(Fit {
        weights_basis_points_per_million_tokens: weights.map(|weight| weight * 1_000_000.0),
        n: n as u64,
        r_squared,
    })
}

#[derive(Clone, Debug, Default)]
pub struct EffectiveInput {
    /// Plan price in force for the account, prorated to `period_days`, in
    /// microdollars.
    pub price_microdollars: Option<u64>,
    /// Tokens actually used in the period, by bucket.
    pub usage: [u64; 5],
    /// Of those, tokens whose price is known.
    pub priced_usage: [u64; 5],
    /// Average API-equivalent rate per million tokens, by bucket.
    pub api_rates: [Option<u64>; 5],
    /// Segments backing the mileage, for `n`.
    pub n: u64,
    /// Length of the reported billing period.
    pub period_days: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct BucketRate {
    pub bucket: &'static str,
    pub tokens: u64,
    /// Rate at full use of the window, per million tokens.
    pub potential: Option<Money>,
    /// Plan price over tokens actually used, per million tokens.
    pub realised: Option<Money>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Effective {
    pub price: Option<Money>,
    pub capacity: Option<Money>,
    /// The window's used share at its latest reading, in basis points. It is
    /// not averaged over the period.
    pub latest_used_basis_points: Option<u64>,
    pub n: u64,
    /// Priced tokens as a share of all used tokens, in basis points.
    pub coverage_basis_points: Option<u64>,
    /// Length of the billing period the price and tokens cover.
    pub period_days: u32,
    /// Always set: token use is a lower bound, so realised rates are upper
    /// bounds, until ingest coverage of the account is checked.
    pub note: &'static str,
    pub buckets: Vec<BucketRate>,
}

/// Potential and realised rates against one window's capacity. Potential
/// applies the plan's discount factor `price / capacity` to the API rate;
/// realised divides the plan price by tokens actually used.
pub fn effective(
    input: &EffectiveInput,
    capacity: Option<Money>,
    latest_used_basis_points: Option<u64>,
) -> Effective {
    let total: u64 = input.usage.iter().sum();
    let priced: u64 = input.priced_usage.iter().sum();
    let coverage = (total > 0)
        .then(|| u64::try_from(u128::from(priced) * 10_000 / u128::from(total)).unwrap_or(10_000));
    let price = input.price_microdollars.map(Money::new);
    let buckets = (0..5)
        .map(|index| {
            let tokens = input.usage[index];
            let realised = match (input.price_microdollars, tokens) {
                (Some(price), tokens) if tokens > 0 => {
                    round_div(u128::from(price) * 1_000_000, u128::from(tokens)).map(Money::new)
                }
                _ => None,
            };
            let potential = match (
                input.api_rates[index],
                input.price_microdollars,
                capacity.as_ref().map(|money| money.microdollars),
            ) {
                (Some(rate), Some(price), Some(capacity)) if capacity > 0 => u128::from(rate)
                    .checked_mul(u128::from(price))
                    .and_then(|numerator| round_div(numerator, u128::from(capacity)))
                    .map(Money::new),
                _ => None,
            };
            BucketRate {
                bucket: BUCKETS[index],
                tokens,
                potential,
                realised,
            }
        })
        .collect();
    Effective {
        price,
        capacity,
        latest_used_basis_points,
        n: input.n,
        coverage_basis_points: coverage,
        period_days: input.period_days,
        note: BOUND_NOTE,
        buckets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(at: i64, used: u64, cost: Option<u64>) -> Reading {
        Reading {
            observed_unix_seconds: at,
            used_basis_points: used,
            resets_unix_seconds: Some(10_000_000),
            plan: Some("max_5x".into()),
            tier: None,
            cost_microdollars: cost,
            usage: [0; 5],
            priced: cost.is_some(),
        }
    }

    fn series(readings: Vec<Reading>) -> Series {
        Series {
            account_key: "k1".into(),
            label: "work".into(),
            provider: "claude".into(),
            window: "five_hour".into(),
            window_seconds: Some(18_000),
            readings,
        }
    }

    fn options() -> Options {
        Options {
            min_runs: 1,
            ..Options::default()
        }
    }

    #[test]
    fn three_readings_give_exact_mileage_and_capacity() {
        let report = analyse(
            &series(vec![
                reading(0, 1_000, Some(0)),
                reading(100, 3_000, Some(20_000_000)),
                reading(200, 6_000, Some(30_000_000)),
            ]),
            options(),
        );
        // 20% for $20 and 30% for $30 are each $1 per point.
        let mileage = report.mileage.mileage.as_ref().unwrap();
        assert_eq!(mileage.microdollars, 1_000_000);
        assert_eq!(mileage.usd, "1.000000");
        assert_eq!(
            report.mileage.capacity.as_ref().unwrap().microdollars,
            100_000_000
        );
        assert_eq!(report.mileage.capacity.as_ref().unwrap().usd, "100.000000");
        assert_eq!((report.mileage.n, report.mileage.priced), (2, 2));
        assert_eq!(report.mileage.priced_share_basis_points, Some(10_000));
        assert_eq!(report.runs.len(), 1);
        assert_eq!(report.runs[0].segments, 2);
    }

    #[test]
    fn a_drop_or_a_reset_splits_runs_without_negative_mileage() {
        let mut reset = reading(200, 100, Some(90_000_000));
        reset.resets_unix_seconds = Some(20_000_000);
        let report = analyse(
            &series(vec![
                reading(0, 1_000, Some(0)),
                reading(100, 3_000, Some(20_000_000)),
                reset,
                reading(300, 2_000, Some(10_000_000)),
                reading(400, 500, Some(2_000_000)),
            ]),
            options(),
        );
        assert_eq!(report.runs.len(), 3);
        assert_eq!(report.runs[1].reset, Some("reported"));
        assert_eq!(report.runs[2].reset, Some("inferred"));
        assert!(report.runs.iter().all(|run| {
            run.mileage
                .as_ref()
                .is_none_or(|mileage| mileage.microdollars > 0)
        }));
        // The drops themselves contribute no segment.
        assert_eq!(report.mileage.n, 2);
    }

    #[test]
    fn a_null_reset_time_is_segmented_by_a_drop_and_flagged_inferred() {
        let mut first = reading(0, 5_000, Some(0));
        first.resets_unix_seconds = None;
        let mut dropped = reading(100, 500, Some(4_000_000));
        dropped.resets_unix_seconds = None;
        let mut later = reading(200, 1_500, Some(9_000_000));
        later.resets_unix_seconds = None;
        let report = analyse(&series(vec![first, dropped, later]), options());
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[1].reset, Some("inferred"));
        assert_eq!(
            report.runs[1].capacity.as_ref().unwrap().microdollars,
            90_000_000
        );
    }

    #[test]
    fn reset_time_jitter_under_a_minute_stays_in_one_window() {
        // The reset time drifts 40 s a reading: no single step is a reset.
        let mut readings: Vec<Reading> = [0, 40, 80, 120]
            .into_iter()
            .enumerate()
            .map(|(index, drift)| {
                let mut reading = reading(
                    100 * index as i64,
                    1_000 * (index as u64 + 1),
                    Some(10_000_000),
                );
                reading.resets_unix_seconds = Some(10_000_000 + drift);
                reading
            })
            .collect();
        let report = analyse(&series(readings.clone()), options());
        assert_eq!(report.runs.len(), 1);
        assert_eq!(report.mileage.n, 3);
        // A reset a full window later still starts a new run.
        let mut reset = reading(400, 1_500, Some(5_000_000));
        reset.resets_unix_seconds = Some(10_018_000);
        readings.push(reset);
        let report = analyse(&series(readings), options());
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[1].reset, Some("reported"));
    }

    fn capacity_series(
        from: i64,
        each: i64,
        count: usize,
        cost: u64,
        base_used: u64,
    ) -> Vec<Reading> {
        let mut readings = Vec::new();
        for index in 0..count {
            readings.push(reading(
                from + each * index as i64,
                base_used + 100 * index as u64,
                Some(cost),
            ));
        }
        readings
    }

    #[test]
    fn a_thirty_percent_fall_at_a_constant_plan_is_flagged() {
        // Ten prior segments at $1000 capacity, then ten recent at $700.
        let mut readings = capacity_series(-29 * 86_400, 86_400, 11, 10_000_000, 100);
        readings.extend(capacity_series(-2 * 86_400, 3_600, 10, 7_000_000, 1_200));
        let report = analyse(
            &series(readings),
            Options {
                min_runs: 10,
                ..Options::default()
            },
        );
        assert_eq!(report.step_change.prior_n, 10);
        assert_eq!(report.step_change.recent_n, 10);
        assert_eq!(report.step_change.status, "flagged");
        assert!(
            report
                .step_change
                .percent
                .is_some_and(|p| (p - 30.0).abs() < 0.01)
        );
    }

    #[test]
    fn a_ten_percent_fall_or_too_few_runs_is_inconclusive() {
        let mut readings = capacity_series(-29 * 86_400, 86_400, 11, 10_000_000, 100);
        readings.extend(capacity_series(-2 * 86_400, 3_600, 10, 9_000_000, 1_200));
        let report = analyse(
            &series(readings),
            Options {
                min_runs: 10,
                ..Options::default()
            },
        );
        assert!(
            report
                .step_change
                .percent
                .is_some_and(|p| (p - 10.0).abs() < 0.01)
        );
        assert_eq!(report.step_change.status, "inconclusive");
        let mut few = capacity_series(-20 * 86_400, 86_400, 6, 10_000_000, 100);
        few.extend(capacity_series(-2 * 86_400, 3_600, 6, 5_000_000, 700));
        let report = analyse(
            &series(few),
            Options {
                min_runs: 10,
                ..Options::default()
            },
        );
        assert_eq!(report.step_change.status, "inconclusive");
        assert_eq!(
            (report.step_change.prior_n, report.step_change.recent_n),
            (5, 6)
        );
    }

    #[test]
    fn a_plan_change_reports_the_upgrade_gain() {
        let mut readings = capacity_series(0, 100, 5, 10_000_000, 100);
        for reading in &mut readings[2..] {
            reading.plan = Some("max_20x".into());
            reading.cost_microdollars = Some(15_000_000);
        }
        let report = analyse(&series(readings), options());
        let upgrade = report.upgrade.unwrap();
        assert_eq!(upgrade.plan_from.as_deref(), Some("max_5x"));
        assert_eq!(upgrade.plan_to.as_deref(), Some("max_20x"));
        assert!(upgrade.percent.is_some_and(|p| (p - 50.0).abs() < 0.01));
    }

    #[test]
    fn fitted_weights_need_enough_rows_and_a_good_fit() {
        let rows: Vec<Observation> = [
            [1u64, 0],
            [0, 1],
            [1, 1],
            [2, 0],
            [0, 2],
            [2, 1],
            [1, 2],
            [3, 0],
            [0, 3],
            [2, 2],
        ]
        .map(|[a, b]| ([a, b, 0, 0, 0], 2 * a + 3 * b))
        .to_vec();
        let fitted = fit(&rows, 10).unwrap();
        assert_eq!(fitted.n, 10);
        assert!(fitted.r_squared > 0.99);
        assert!((fitted.weights_basis_points_per_million_tokens[0] - 2_000_000.0).abs() < 1.0);
        assert!((fitted.weights_basis_points_per_million_tokens[1] - 3_000_000.0).abs() < 1.0);
        assert!(fit(&rows[..5], 10).is_none());
        let noisy: Vec<Observation> = (1..=10)
            .map(|n| ([n, 0, 0, 0, 0], if n % 2 == 0 { 1 } else { 100 }))
            .collect();
        assert!(fit(&noisy, 10).is_none());
    }

    #[test]
    fn effective_rates_report_utilisation_coverage_and_a_lower_bound() {
        let input = EffectiveInput {
            price_microdollars: Some(100_000_000),
            usage: [1_000_000, 0, 0, 0, 1_000_000],
            priced_usage: [1_000_000, 0, 0, 0, 500_000],
            api_rates: [
                Some(1_000_000),
                Some(100_000),
                Some(1_250_000),
                None,
                Some(10_000_000),
            ],
            n: 4,
            period_days: 30,
        };
        let effective = effective(&input, Some(Money::new(200_000_000)), Some(4_000));
        assert_eq!(effective.price.unwrap().microdollars, 100_000_000);
        assert_eq!(effective.latest_used_basis_points, Some(4_000));
        assert_eq!(effective.coverage_basis_points, Some(7_500));
        assert_eq!(effective.n, 4);
        assert!(effective.note.contains("lower bound"));
        // Potential halves the API rate because the plan costs half capacity.
        assert_eq!(
            effective.buckets[0]
                .potential
                .as_ref()
                .unwrap()
                .microdollars,
            500_000
        );
        // Realised is the price over the million tokens actually used.
        assert_eq!(
            effective.buckets[0].realised.as_ref().unwrap().microdollars,
            100_000_000
        );
        assert_eq!(
            effective.buckets[4].realised.as_ref().unwrap().microdollars,
            100_000_000
        );
        assert!(effective.buckets[1].realised.is_none());
    }

    #[test]
    fn full_price_coverage_still_reports_the_bound_note() {
        // Priced coverage alone does not show that ingest covers the account.
        let input = EffectiveInput {
            price_microdollars: Some(100_000_000),
            usage: [1_000_000, 0, 0, 0, 0],
            priced_usage: [1_000_000, 0, 0, 0, 0],
            api_rates: [Some(1_000_000), None, None, None, None],
            n: 2,
            period_days: 7,
        };
        let effective = effective(&input, Some(Money::new(200_000_000)), None);
        assert_eq!(effective.coverage_basis_points, Some(10_000));
        assert!(effective.note.contains("lower bound"));
        assert!(effective.note.contains("upper bounds"));
    }
}
