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

#[derive(Clone, Debug)]
pub struct Reading {
    pub observed_unix_seconds: i64,
    pub used_basis_points: u64,
    pub resets_unix_seconds: Option<i64>,
    pub plan: Option<String>,
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
    readings: u64,
    segments: Vec<Segment>,
}

/// Why `curr` starts a new run rather than extending the one `prev` is in.
fn boundary(prev: &Reading, curr: &Reading) -> Option<&'static str> {
    let known = |a: &Option<String>, b: &Option<String>| match (a, b) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    };
    if known(&prev.plan, &curr.plan) {
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
        let same_plan = last.is_none_or(|last| curr.plan == last.plan);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(at: i64, used: u64, cost: Option<u64>) -> Reading {
        Reading {
            observed_unix_seconds: at,
            used_basis_points: used,
            resets_unix_seconds: Some(10_000_000),
            plan: Some("plan-a".into()),
            cost_microdollars: cost,
            usage: [0; 5],
            priced: cost.is_some(),
        }
    }

    fn series(readings: Vec<Reading>) -> Series {
        Series {
            account_key: "k1".into(),
            label: "work".into(),
            provider: "test-provider".into(),
            window: "five_hour".into(),
            window_seconds: Some(18_000),
            readings,
        }
    }

    fn options(min_runs: u32) -> Options {
        Options {
            min_runs,
            ..Options::default()
        }
    }

    fn with_reset(mut reading: Reading, resets_unix_seconds: i64) -> Reading {
        reading.resets_unix_seconds = Some(resets_unix_seconds);
        reading
    }

    fn with_plan(mut reading: Reading, plan: &str) -> Reading {
        reading.plan = Some(plan.into());
        reading
    }

    /// `count` readings `step` seconds apart, each rising `rise` basis points
    /// with an API-equivalent cost of `cost` microdollars in the interval.
    fn rising(
        from: i64,
        step: i64,
        count: usize,
        base_used: u64,
        rise: u64,
        cost: u64,
    ) -> Vec<Reading> {
        (0..count)
            .map(|index| {
                reading(
                    from + step * index as i64,
                    base_used + rise * index as u64,
                    Some(cost),
                )
            })
            .collect()
    }

    #[test]
    fn mileage_and_capacity_reconcile_with_priced_usage() {
        // Mileage is cost * 100 / rise in basis points; capacity is 100 points.
        // Varied but equivalent cost/rise pairs must give the same per-point
        // mileage, so the arithmetic itself is the oracle.
        for (cost, rise) in [
            (20_000_000u64, 2_000u64),
            (10_000_000, 1_000),
            (30_000_000, 3_000),
        ] {
            let readings = vec![
                reading(0, 1_000, Some(0)),
                reading(100, 1_000 + rise, Some(cost)),
                reading(200, 1_000 + 2 * rise, Some(cost)),
            ];
            let report = analyse(&series(readings), options(1));
            let summary = &report.mileage;
            assert_eq!(summary.n, 2);
            assert_eq!(summary.priced, summary.n);
            assert_eq!(summary.priced_share_basis_points, Some(10_000));
            let mileage = summary.mileage.as_ref().unwrap().microdollars;
            let capacity = summary.capacity.as_ref().unwrap().microdollars;
            assert_eq!(mileage, cost * 100 / rise);
            // Capacity is 100 percentage points: exact, with no tolerance.
            assert_eq!(capacity, cost * 100 * 100 / rise);
            assert_eq!(capacity, mileage * 100);
        }
    }

    #[test]
    fn runs_partition_readings_and_segments_are_nonnegative() {
        struct Case {
            name: &'static str,
            readings: Vec<Reading>,
            reasons: Vec<Option<&'static str>>,
        }
        let cases = vec![
            Case {
                name: "monotonic",
                readings: vec![
                    reading(0, 1_000, Some(0)),
                    reading(100, 2_000, Some(1_000_000)),
                    reading(200, 3_000, Some(1_000_000)),
                ],
                reasons: vec![None],
            },
            Case {
                name: "fall",
                readings: vec![
                    reading(0, 1_000, Some(0)),
                    reading(100, 500, Some(1_000_000)),
                    reading(200, 1_500, Some(1_000_000)),
                ],
                reasons: vec![None, Some("inferred")],
            },
            Case {
                name: "reported reset",
                readings: vec![
                    reading(0, 1_000, Some(0)),
                    with_reset(reading(100, 500, Some(1_000_000)), 10_000_060),
                    reading(200, 1_500, Some(1_000_000)),
                ],
                reasons: vec![None, Some("reported")],
            },
            Case {
                name: "jitter under a minute",
                readings: vec![
                    reading(0, 1_000, Some(0)),
                    with_reset(reading(100, 2_000, Some(1_000_000)), 10_000_040),
                    with_reset(reading(200, 3_000, Some(1_000_000)), 10_000_080),
                ],
                reasons: vec![None],
            },
            Case {
                name: "reset a full window after jittering resets",
                readings: vec![
                    reading(0, 1_000, Some(0)),
                    with_reset(reading(100, 2_000, Some(1_000_000)), 10_000_040),
                    with_reset(reading(200, 3_000, Some(1_000_000)), 10_000_080),
                    with_reset(reading(300, 4_000, Some(1_000_000)), 10_000_120),
                    with_reset(reading(400, 4_500, Some(1_000_000)), 10_018_000),
                ],
                reasons: vec![None, Some("reported")],
            },
            Case {
                name: "plan change",
                readings: vec![
                    reading(0, 1_000, Some(0)),
                    with_plan(reading(100, 2_000, Some(1_000_000)), "plan-b"),
                    with_plan(reading(200, 3_000, Some(1_000_000)), "plan-b"),
                ],
                reasons: vec![None, Some("plan_change")],
            },
        ];
        for Case {
            name,
            readings,
            reasons,
        } in cases
        {
            let report = analyse(&series(readings.clone()), options(1));
            let actual: Vec<_> = report.runs.iter().map(|run| run.reset).collect();
            assert_eq!(actual, reasons, "{name}: boundary reasons");
            let mut offset = 0;
            for run in &report.runs {
                let part = &readings[offset..offset + run.readings as usize];
                let rising_pairs = part
                    .windows(2)
                    .filter(|pair| pair[1].used_basis_points > pair[0].used_basis_points)
                    .count();
                assert_eq!(
                    run.segments as usize, rising_pairs,
                    "{name}: only rising pairs form segments"
                );
                assert!(run.priced_segments <= run.segments);
                if run.segments > 0 {
                    let mileage = run.mileage.as_ref().expect("priced rises have mileage");
                    assert!(mileage.microdollars > 0, "{name}: mileage must be positive");
                }
                offset += run.readings as usize;
            }
            assert_eq!(
                offset,
                readings.len(),
                "{name}: runs must partition readings"
            );
        }
    }

    #[test]
    fn null_reset_times_segment_by_a_drop_and_are_inferred() {
        let without_reset = |mut reading: Reading| {
            reading.resets_unix_seconds = None;
            reading
        };
        // The drop to 500 starts a new run; its one rising pair costs
        // `cost` for `rise` basis points, so capacity is cost * 100 / rise
        // percentage points times 100.
        let (cost, rise) = (9_000_000u64, 1_000u64);
        let report = analyse(
            &series(vec![
                without_reset(reading(0, 5_000, Some(0))),
                without_reset(reading(100, 500, Some(4_000_000))),
                without_reset(reading(200, 500 + rise, Some(cost))),
            ]),
            options(1),
        );
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.runs[1].reset, Some("inferred"));
        let capacity = report.runs[1].capacity.as_ref().unwrap().microdollars;
        assert_eq!(capacity, cost * 100 * 100 / rise);
        assert_eq!(capacity, 90_000_000);
    }

    #[test]
    fn summary_tracks_priced_evidence_without_inventing_mileage() {
        let readings = vec![
            reading(0, 1_000, None),
            reading(100, 2_000, Some(20_000_000)),
            reading(200, 2_500, None),
            reading(300, 3_500, Some(20_000_000)),
        ];
        let report = analyse(&series(readings), options(1));
        let summary = &report.mileage;
        assert!(summary.priced < summary.n);
        assert_eq!(
            summary.priced_share_basis_points,
            Some(summary.priced * 10_000 / summary.n)
        );
        assert_eq!(summary.mileage.as_ref().unwrap().microdollars, 2_000_000);

        let unpriced = [reading(0, 1_000, None), reading(100, 2_000, None)];
        let report = analyse(&series(unpriced.to_vec()), options(1));
        let summary = &report.mileage;
        assert!(summary.n > 0);
        assert_eq!(summary.priced, 0);
        assert_eq!(summary.priced_share_basis_points, Some(0));
        assert!(summary.mileage.is_none());
        assert!(summary.capacity.is_none());
    }

    #[test]
    fn step_change_flags_only_sufficient_falls() {
        struct Case {
            costs: (u64, u64),
            // Readings in the prior and recent groups.
            counts: (usize, usize),
            status: &'static str,
            percent: Option<f64>,
        }
        let cases = [
            Case {
                costs: (10_000_000, 7_000_000),
                counts: (11, 10),
                status: "flagged",
                percent: Some(30.0),
            },
            Case {
                costs: (10_000_000, 9_000_000),
                counts: (11, 10),
                status: "inconclusive",
                percent: Some(10.0),
            },
            Case {
                costs: (10_000_000, 5_000_000),
                counts: (6, 6),
                status: "inconclusive",
                percent: Some(50.0),
            },
        ];
        for case in cases {
            let mut readings = rising(-29 * 86_400, 86_400, case.counts.0, 100, 100, case.costs.0);
            readings.extend(rising(
                -2 * 86_400,
                3_600,
                case.counts.1,
                1_200,
                100,
                case.costs.1,
            ));
            let report = analyse(&series(readings), options(10));
            let change = &report.step_change;
            assert_eq!(change.status, case.status, "costs {:?}", case.costs);
            // n readings give n - 1 prior segments; the first recent reading
            // rises from the last prior one, so recent keeps all n segments.
            assert_eq!(
                (change.prior_n as usize, change.recent_n as usize),
                (case.counts.0 - 1, case.counts.1),
                "costs {:?}",
                case.costs
            );
            match case.percent {
                Some(expected) => {
                    let actual = change.percent.expect("percent present");
                    assert!((actual - expected).abs() < 0.01, "{actual} vs {expected}");
                }
                None => assert!(change.percent.is_none()),
            }
        }
    }

    #[test]
    fn plan_change_reports_capacity_gain_from_its_own_evidence() {
        let mut readings = rising(0, 100, 5, 100, 100, 10_000_000);
        for reading in &mut readings[2..] {
            reading.plan = Some("plan-b".into());
            reading.cost_microdollars = Some(15_000_000);
        }
        let report = analyse(&series(readings), options(1));
        let upgrade = report.upgrade.as_ref().unwrap();
        assert_eq!(upgrade.plan_from.as_deref(), Some("plan-a"));
        assert_eq!(upgrade.plan_to.as_deref(), Some("plan-b"));

        let rise = 100u64;
        // Capacity is the cost of a full window: mileage (cost * 100 / rise)
        // times 100 percentage points.
        let before = 10_000_000u64 * 100 / rise * 100;
        let after = 15_000_000u64 * 100 / rise * 100;
        assert_eq!(upgrade.before.as_ref().unwrap().microdollars, before);
        assert_eq!(upgrade.after.as_ref().unwrap().microdollars, after);
        let expected = (after as f64 - before as f64) / before as f64 * 100.0;
        assert!((upgrade.percent.unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn fit_requires_enough_rows_and_explains_linear_variance() {
        for (a, b) in [(2u64, 3u64), (3, 1)] {
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
            .map(|[x, y]| ([x, y, 0, 0, 0], a * x + b * y))
            .to_vec();
            let fitted = fit(&rows, 10).unwrap();
            assert_eq!(fitted.n, rows.len() as u64);
            assert!(fitted.r_squared > 0.99);
            assert!(
                fitted
                    .weights_basis_points_per_million_tokens
                    .iter()
                    .all(|weight| *weight >= 0.0)
            );
            assert!(
                (fitted.weights_basis_points_per_million_tokens[0] - a as f64 * 1_000_000.0).abs()
                    < 1.0
            );
            assert!(
                (fitted.weights_basis_points_per_million_tokens[1] - b as f64 * 1_000_000.0).abs()
                    < 1.0
            );
            assert!(
                fit(&rows[..5], 10).is_none(),
                "fewer rows than the minimum must not report a fit"
            );
        }
        let noisy: Vec<Observation> = (1..=10)
            .map(|n| ([n, 0, 0, 0, 0], if n % 2 == 0 { 1 } else { 100 }))
            .collect();
        assert!(fit(&noisy, 10).is_none(), "a poor fit must not be reported");
    }
}
