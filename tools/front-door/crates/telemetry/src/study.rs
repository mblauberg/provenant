//! Offline token-per-merged-PR study (#2315).
//!
//! A pure, deterministic reducer over recorded attempt rows, chair rows and
//! merged-PR attribution. It performs no I/O, reads no clock and adds no
//! counters. Two cohorts, split at a cut-over, are reduced identically and
//! compared. Method and results template: `docs/telemetry/token-per-pr-study.md`.
use std::collections::{BTreeMap, BTreeSet};

use provenant_records::{AttemptFact, AttemptOutcome};
use serde::{Deserialize, Serialize};

/// Smallest merged-PR count for a cohort to be reported on.
pub const MIN_COHORT_PRS: usize = 10;
/// Coverage may differ between cohorts by at most this many basis points.
pub const MATERIAL_COVERAGE_GAP_BP: u64 = 1_000;
/// Inclusive changed-line ceilings of the small and medium buckets.
pub const SMALL_MAX_LINES: u64 = 50;
pub const MEDIUM_MAX_LINES: u64 = 300;

/// Mutually exclusive token categories. Precedence when classifying an
/// attempt: failed, review, repair, worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Chair,
    Worker,
    Review,
    Repair,
    FailedAttempt,
}

/// One attempt's recorded tokens. `tokens` is `None` when no usage category
/// was measured; it is never read as zero.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRow {
    pub id: String,
    /// Unix seconds the attempt finished.
    pub finished: i64,
    /// Retained work-item label, e.g. `#2315` or `pr:2461`.
    pub work_item: Option<String>,
    pub role: Option<String>,
    pub failed: bool,
    pub route: Option<String>,
    pub tokens: Option<u64>,
    pub cache_read: Option<u64>,
}

impl AttemptRow {
    /// Project a terminal attempt fact. Unmeasured usage stays `None`.
    pub fn from_fact(fact: &AttemptFact) -> Self {
        let u = &fact.usage;
        let parts = [
            u.uncached_input,
            u.cache_read,
            u.cache_write,
            u.output_excluding_reasoning,
            u.reasoning,
        ];
        let tokens = parts.iter().any(Option::is_some).then(|| {
            parts
                .iter()
                .flatten()
                .fold(0u64, |a, b| a.saturating_add(*b))
        });
        let route = fact.resolved_route.as_ref().map(|r| {
            format!(
                "{}/{}",
                r.provider.as_deref().unwrap_or("unknown"),
                r.model.as_deref().unwrap_or("unknown")
            )
        });
        Self {
            id: fact.attempt_id.to_string(),
            finished: fact.finished_at.unix_seconds(),
            work_item: fact.context.as_ref().and_then(|c| c.work_item.clone()),
            role: fact.context.as_ref().and_then(|c| c.role.clone()),
            failed: !matches!(
                fact.terminal_state,
                AttemptOutcome::Succeeded | AttemptOutcome::ReviewRequired
            ),
            route,
            tokens,
            cache_read: u.cache_read,
        }
    }

    fn category(&self) -> Category {
        let role = self.role.as_deref().unwrap_or("").to_ascii_lowercase();
        if self.failed {
            Category::FailedAttempt
        } else if role.contains("review") {
            Category::Review
        } else if role.contains("repair") || role.contains("fix") {
            Category::Repair
        } else {
            Category::Worker
        }
    }
}

/// One chair session. `pr` is supplied by the caller's attribution; the
/// transcript reader gives no PR link of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChairRow {
    pub session: String,
    pub finished: i64,
    pub pr: Option<u64>,
    /// `None` when the transcript could not be read (an unread counter).
    pub tokens: Option<u64>,
}

/// A merged pull request and the work-item labels that join attempts to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedPr {
    pub number: u64,
    pub merged: i64,
    /// Changed lines; `None` lands in the `unknown` size bucket.
    pub lines: Option<u64>,
    #[serde(default)]
    pub work_items: Vec<String>,
}

/// Half-open merge-time interval `[from, until)` in unix seconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortSpec {
    pub label: String,
    pub from: i64,
    pub until: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CutOver {
    pub commit: String,
    pub date: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudyInput {
    pub cut_over: CutOver,
    pub baseline: CohortSpec,
    pub enabled: CohortSpec,
    pub prs: Vec<MergedPr>,
    pub attempts: Vec<AttemptRow>,
    pub chair: Vec<ChairRow>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CategoryTokens {
    pub chair: u64,
    pub worker: u64,
    pub review: u64,
    pub repair: u64,
    pub failed_attempt: u64,
}
impl CategoryTokens {
    fn add(&mut self, category: Category, n: u64) {
        let slot = match category {
            Category::Chair => &mut self.chair,
            Category::Worker => &mut self.worker,
            Category::Review => &mut self.review,
            Category::Repair => &mut self.repair,
            Category::FailedAttempt => &mut self.failed_attempt,
        };
        *slot = slot.saturating_add(n);
    }
    pub fn total(&self) -> u64 {
        [
            self.chair,
            self.worker,
            self.review,
            self.repair,
            self.failed_attempt,
        ]
        .iter()
        .fold(0, |a, b| a.saturating_add(*b))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Spread {
    pub prs: usize,
    pub total: u64,
    /// Mean rounded down; `None` for an empty set.
    pub mean: Option<u64>,
    /// Median; for an even count, the mean of the middle pair rounded down.
    pub median: Option<u64>,
}
fn spread(mut values: Vec<u64>) -> Spread {
    values.sort_unstable();
    let n = values.len();
    let total = values.iter().fold(0u64, |a, b| a.saturating_add(*b));
    let median = match n {
        0 => None,
        _ if n % 2 == 1 => Some(values[n / 2]),
        _ => Some(((u128::from(values[n / 2 - 1]) + u128::from(values[n / 2])) / 2) as u64),
    };
    Spread {
        prs: n,
        total,
        mean: (n > 0).then(|| total / n as u64),
        median,
    }
}

/// Work that counts toward no cohort PR, shown beside the totals.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Beside {
    /// Attempts carrying a work item that joins no merged PR in either cohort.
    pub abandoned_attempts: usize,
    pub abandoned_tokens: u64,
    /// Attempts with no work item, or an ambiguous one.
    pub unlinked_attempts: usize,
    pub unlinked_tokens: u64,
    pub unlinked_chair_sessions: usize,
    pub unlinked_chair_tokens: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Coverage {
    pub window_attempts: usize,
    pub linked_attempts: usize,
    pub linked_basis_points: Option<u64>,
    /// Linked attempts with no measured usage.
    pub linked_without_tokens: usize,
    pub chair_sessions: usize,
    pub chair_linked: usize,
    pub chair_unread: usize,
    /// Share of the cohort's PRs with at least one read chair counter.
    pub prs_with_chair_basis_points: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Cohort {
    pub label: String,
    pub from: i64,
    pub until: i64,
    pub merged_prs: usize,
    pub per_pr: Spread,
    pub by_category: CategoryTokens,
    pub cache_read_tokens: u64,
    pub by_size: BTreeMap<&'static str, Spread>,
    /// Attributed worker-side tokens by route; unknown routes are `unknown`.
    pub route_tokens: BTreeMap<String, u64>,
    pub coverage: Coverage,
    pub beside: Beside,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// A cohort has fewer than [`MIN_COHORT_PRS`] merged PRs.
    Pending,
    /// Counters are missing or coverage differs materially.
    Inconclusive,
    /// Both cohorts are complete and comparably covered.
    Comparable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Study {
    pub cut_over: CutOver,
    pub min_cohort_prs: usize,
    pub verdict: Verdict,
    pub reasons: Vec<String>,
    pub baseline: Cohort,
    pub enabled: Cohort,
    /// Signed basis-point change enabled vs baseline; only when `comparable`.
    pub mean_change_basis_points: Option<i64>,
    pub median_change_basis_points: Option<i64>,
}

fn size_bucket(lines: Option<u64>) -> &'static str {
    match lines {
        None => "unknown",
        Some(n) if n <= SMALL_MAX_LINES => "small",
        Some(n) if n <= MEDIUM_MAX_LINES => "medium",
        Some(_) => "large",
    }
}

fn bp(part: usize, whole: usize) -> Option<u64> {
    (whole > 0).then(|| (part as u64).saturating_mul(10_000) / whole as u64)
}

fn pr_label(number: u64) -> [String; 2] {
    [format!("#{number}"), format!("pr:{number}")]
}

/// Map each work-item label to its single PR. A label claimed by two PRs is
/// ambiguous and joins nothing.
fn label_index(prs: &[MergedPr]) -> BTreeMap<String, Option<u64>> {
    let mut index = BTreeMap::<String, Option<u64>>::new();
    for pr in prs {
        let labels = pr_label(pr.number)
            .into_iter()
            .chain(pr.work_items.iter().cloned());
        for label in labels {
            index
                .entry(label)
                .and_modify(|slot| {
                    if *slot != Some(pr.number) {
                        *slot = None;
                    }
                })
                .or_insert(Some(pr.number));
        }
    }
    index
}

/// Reduce the input. Duplicate attempt IDs, duplicate PR numbers and inverted
/// cohort intervals are rejected rather than guessed at.
pub fn reduce(input: &StudyInput) -> Result<Study, String> {
    for spec in [&input.baseline, &input.enabled] {
        if spec.from >= spec.until {
            return Err(format!("cohort {} has an empty interval", spec.label));
        }
    }
    if input.baseline.until > input.enabled.from {
        return Err("cohorts overlap".into());
    }
    let mut seen = BTreeSet::new();
    if !input.prs.iter().all(|p| seen.insert(p.number)) {
        return Err("duplicate PR number".into());
    }
    let mut seen = BTreeSet::new();
    if !input.attempts.iter().all(|a| seen.insert(a.id.as_str())) {
        return Err("duplicate attempt ID".into());
    }
    let index = label_index(&input.prs);
    let baseline = cohort(&input.baseline, input, &index);
    let enabled = cohort(&input.enabled, input, &index);

    let mut reasons = Vec::new();
    let mut verdict = Verdict::Comparable;
    for c in [&baseline, &enabled] {
        if c.merged_prs < MIN_COHORT_PRS {
            reasons.push(format!(
                "{}: {} merged PRs, need {MIN_COHORT_PRS}",
                c.label, c.merged_prs
            ));
            verdict = Verdict::Pending;
        }
    }
    if verdict == Verdict::Comparable {
        for c in [&baseline, &enabled] {
            if c.by_category.chair == 0 || c.coverage.prs_with_chair_basis_points.unwrap_or(0) == 0
            {
                reasons.push(format!("{}: no chair counters attributed", c.label));
            }
        }
        let gaps = [
            (
                "attribution",
                baseline.coverage.linked_basis_points,
                enabled.coverage.linked_basis_points,
            ),
            (
                "chair",
                baseline.coverage.prs_with_chair_basis_points,
                enabled.coverage.prs_with_chair_basis_points,
            ),
        ];
        for (name, a, b) in gaps {
            if a.unwrap_or(0).abs_diff(b.unwrap_or(0)) > MATERIAL_COVERAGE_GAP_BP {
                reasons.push(format!(
                    "{name} coverage differs materially between cohorts"
                ));
            }
        }
        if !reasons.is_empty() {
            verdict = Verdict::Inconclusive;
        }
    }
    let comparable = verdict == Verdict::Comparable;
    let change = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (Some(a), Some(b)) if comparable && a > 0 => {
            Some((i128::from(b) - i128::from(a)) * 10_000 / i128::from(a))
                .and_then(|v| i64::try_from(v).ok())
        }
        _ => None,
    };
    Ok(Study {
        cut_over: input.cut_over.clone(),
        min_cohort_prs: MIN_COHORT_PRS,
        mean_change_basis_points: change(baseline.per_pr.mean, enabled.per_pr.mean),
        median_change_basis_points: change(baseline.per_pr.median, enabled.per_pr.median),
        verdict,
        reasons,
        baseline,
        enabled,
    })
}

fn cohort(spec: &CohortSpec, input: &StudyInput, index: &BTreeMap<String, Option<u64>>) -> Cohort {
    let in_window = |t: i64| t >= spec.from && t < spec.until;
    let prs: BTreeMap<u64, &MergedPr> = input
        .prs
        .iter()
        .filter(|p| in_window(p.merged))
        .map(|p| (p.number, p))
        .collect();
    let mut per_pr: BTreeMap<u64, u64> = prs.keys().map(|n| (*n, 0)).collect();
    let mut chair_prs = BTreeSet::new();
    let mut by_category = CategoryTokens::default();
    let mut route_tokens = BTreeMap::<String, u64>::new();
    let mut cache_read_tokens = 0u64;
    let mut coverage = Coverage::default();
    let mut beside = Beside::default();

    for a in input.attempts.iter().filter(|a| in_window(a.finished)) {
        coverage.window_attempts += 1;
        let joined = a.work_item.as_ref().map(|w| index.get(w));
        let tokens = a.tokens.unwrap_or(0);
        match joined {
            Some(Some(Some(pr))) if prs.contains_key(pr) => {
                coverage.linked_attempts += 1;
                if a.tokens.is_none() {
                    coverage.linked_without_tokens += 1;
                }
                let category = a.category();
                by_category.add(category, tokens);
                cache_read_tokens = cache_read_tokens.saturating_add(a.cache_read.unwrap_or(0));
                *route_tokens
                    .entry(a.route.clone().unwrap_or_else(|| "unknown".into()))
                    .or_default() += tokens;
                if let Some(slot) = per_pr.get_mut(pr) {
                    *slot = slot.saturating_add(tokens);
                }
            }
            // A label that names a PR outside this cohort belongs to the other
            // cohort's accounting (or neither); it is not this cohort's work.
            Some(Some(Some(_))) => coverage.window_attempts -= 1,
            Some(None) | None | Some(Some(None)) => {
                // `Some(None)`: label unknown to any merged PR => abandoned.
                // `Some(Some(None))`: ambiguous => unlinked.
                // `None`: no label => unlinked.
                let abandoned = matches!(joined, Some(None));
                if abandoned {
                    beside.abandoned_attempts += 1;
                    beside.abandoned_tokens = beside.abandoned_tokens.saturating_add(tokens);
                } else {
                    beside.unlinked_attempts += 1;
                    beside.unlinked_tokens = beside.unlinked_tokens.saturating_add(tokens);
                }
            }
        }
    }
    for c in input.chair.iter().filter(|c| in_window(c.finished)) {
        coverage.chair_sessions += 1;
        if c.tokens.is_none() {
            coverage.chair_unread += 1;
        }
        let tokens = c.tokens.unwrap_or(0);
        match c.pr.filter(|p| prs.contains_key(p)) {
            Some(pr) => {
                coverage.chair_linked += 1;
                by_category.add(Category::Chair, tokens);
                if let Some(slot) = per_pr.get_mut(&pr) {
                    *slot = slot.saturating_add(tokens);
                }
                if c.tokens.is_some() {
                    chair_prs.insert(pr);
                }
            }
            None => {
                beside.unlinked_chair_sessions += 1;
                beside.unlinked_chair_tokens = beside.unlinked_chair_tokens.saturating_add(tokens);
            }
        }
    }
    coverage.linked_basis_points = bp(coverage.linked_attempts, coverage.window_attempts);
    coverage.prs_with_chair_basis_points = bp(chair_prs.len(), prs.len());

    let mut sized = BTreeMap::<&'static str, Vec<u64>>::new();
    for (n, pr) in &prs {
        sized
            .entry(size_bucket(pr.lines))
            .or_default()
            .push(per_pr.get(n).copied().unwrap_or(0));
    }
    Cohort {
        label: spec.label.clone(),
        from: spec.from,
        until: spec.until,
        merged_prs: prs.len(),
        per_pr: spread(per_pr.values().copied().collect()),
        by_category,
        cache_read_tokens,
        by_size: sized.into_iter().map(|(k, v)| (k, spread(v))).collect(),
        route_tokens,
        coverage,
        beside,
    }
}

/// Plain-text rendering for the results template. Deterministic.
pub fn render(study: &Study) -> String {
    let mut out = format!(
        "verdict: {:?}\ncut-over: {} ({})\n",
        study.verdict, study.cut_over.commit, study.cut_over.date
    );
    for reason in &study.reasons {
        out.push_str(&format!("reason: {reason}\n"));
    }
    for c in [&study.baseline, &study.enabled] {
        out.push_str(&format!(
            "\n[{}] merged_prs={} total={} mean={:?} median={:?}\n",
            c.label, c.merged_prs, c.per_pr.total, c.per_pr.mean, c.per_pr.median
        ));
        let t = &c.by_category;
        out.push_str(&format!(
            "  chair={} worker={} review={} repair={} failed={}\n",
            t.chair, t.worker, t.review, t.repair, t.failed_attempt
        ));
        for (bucket, s) in &c.by_size {
            out.push_str(&format!(
                "  size {bucket}: prs={} median={:?}\n",
                s.prs, s.median
            ));
        }
        for (route, n) in &c.route_tokens {
            out.push_str(&format!("  route {route}: {n}\n"));
        }
        let b = &c.beside;
        out.push_str(&format!(
            "  beside: abandoned={}/{} unlinked={}/{} unlinked_chair={}/{}\n",
            b.abandoned_attempts,
            b.abandoned_tokens,
            b.unlinked_attempts,
            b.unlinked_tokens,
            b.unlinked_chair_sessions,
            b.unlinked_chair_tokens
        ));
    }
    out
}

#[cfg(test)]
mod tests;
