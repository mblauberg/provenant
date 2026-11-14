//! The usage ledger: one logical row per terminal attempt, kept in dated
//! segments under the state root with no age limit.
//!
//! Settlement appends a row once per attempt, so lane usage outlives the run
//! store. Rows are keyed by project and attempt: appending a row already
//! present adds nothing. Concurrent retries may append duplicate rows; readers
//! reduce them by project and attempt so spend is counted once. A row holds graph and node identity, route, times,
//! token counts and the priced cost: never a prompt, a reply or a path.
//!
//! Every reader reports a line it does not recognise, by segment and line
//! number. None is skipped silently.
use crate::{CostTotal, PriceTable, TelemetryError, reducer::Money, segments};
use provenant_core::{CancelToken, Clock, ProjectId, WallTime};
use provenant_os::fs::{Directory, FsError, ledger::Ledger as JsonLedger};
use provenant_records::{AccountIdentity, AttemptFact, WorkKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::Path,
};
use thiserror::Error;

/// Directory in the state root holding the segments.
pub const DIRECTORY: &str = "usage-ledger";
/// The one row format this build reads and writes.
pub const FORMAT: &str = "provenant.usage-ledger.v3";
/// Largest segment read. A segment holds one day of attempts.
const SEGMENT_CAP: u64 = 64 << 20;

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error("cannot write the usage ledger: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot encode a usage ledger row: {0}")]
    Encode(#[from] serde_json::Error),
}

impl LedgerError {
    /// Stable machine-readable refusal code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Filesystem(_) => "filesystem",
            Self::Io(_) => "io",
            Self::Encode(_) => "encode",
        }
    }
}

/// Who wrote a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Settlement,
}

/// Disjoint token counts, as the provider reported them. Unknown stays absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub uncached_input: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub cache_write_1h: Option<u64>,
    pub output_excluding_reasoning: Option<u64>,
    pub reasoning: Option<u64>,
}

/// API-equivalent cost at settlement, from the bundled price table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    /// Known cost in millionths of a microdollar, unrounded, so totals round once.
    pub picodollars: u64,
    /// False when a count or a price is unknown: the cost is then a lower bound.
    pub complete: bool,
    /// The price table marks the model as subscription-billed.
    pub subscription: bool,
    /// The price table's identity.
    pub prices: String,
}

/// One terminal attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub format: String,
    pub source: Source,
    pub project: ProjectId,
    pub graph: String,
    pub node: String,
    pub attempt: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub identity: AccountIdentity,
    pub outcome: String,
    pub started: Option<WallTime>,
    pub finished: WallTime,
    pub tokens: Tokens,
    /// Absent when the price table could not be read at settlement.
    pub cost: Option<Cost>,
    /// Context band floor in tokens, when the provider reported one per attempt.
    pub context_band: Option<u64>,
}

impl Row {
    fn key(&self) -> (ProjectId, String) {
        (self.project, self.attempt.clone())
    }
    /// The daily segment, by the attempt's UTC end.
    fn segment(&self) -> String {
        segments::name(self.finished.as_timestamp())
    }
}

/// A line or file the ledger does not recognise. `line` is 1-based; a file
/// that is not a segment has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Unrecognised {
    pub segment: String,
    pub line: Option<u64>,
    pub reason: String,
}

impl std::fmt::Display for Unrecognised {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}:{line}: {}", self.segment, self.reason),
            None => write!(f, "{}: {}", self.segment, self.reason),
        }
    }
}

/// Build the row for a terminal attempt using the store's authoritative work
/// identity. Without a price table the cost stays absent.
pub fn row(
    fact: &AttemptFact,
    key: &WorkKey,
    prices: Option<&PriceTable>,
) -> Result<Row, TelemetryError> {
    if key.project() != fact.project_id {
        return Err(TelemetryError::InvalidFact("work key project mismatch"));
    }
    let route = fact.resolved_route.as_ref();
    let field = |pick: fn(&provenant_records::RouteFact) -> &Option<String>| {
        route.and_then(|route| pick(route).clone())
    };
    let (provider, model) = (field(|r| &r.provider), field(|r| &r.model));
    let cost = prices
        .map(|prices| {
            let priced =
                crate::reducer::priced(&fact.usage, provider.as_deref(), model.as_deref(), prices)?;
            Ok::<_, TelemetryError>(Cost {
                picodollars: u64::try_from(priced.numerator)
                    .map_err(|_| TelemetryError::Overflow)?,
                complete: !priced.missing_cost,
                subscription: priced.subscription,
                prices: prices.identity.id.clone(),
            })
        })
        .transpose()?;
    let outcome = serde_json::to_value(fact.terminal_state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or(TelemetryError::InvalidFact("outcome"))?;
    let usage = &fact.usage;
    Ok(Row {
        format: FORMAT.into(),
        source: Source::Settlement,
        project: fact.project_id,
        graph: key.graph().to_owned(),
        node: key.node().to_owned(),
        attempt: fact.attempt_id.to_string(),
        provider,
        model,
        effort: field(|r| &r.effort),
        identity: AccountIdentity::unknown(),
        outcome,
        started: fact.started_at,
        finished: fact.finished_at,
        tokens: Tokens {
            uncached_input: usage.uncached_input,
            cache_read: usage.cache_read,
            cache_write: usage.cache_write,
            cache_write_1h: usage.cache_write_1h,
            output_excluding_reasoning: usage.output_excluding_reasoning,
            reasoning: usage.reasoning,
        },
        cost,
        context_band: None,
    })
}

/// What an append did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Appended {
    pub added: u64,
    /// Rows whose attempt the ledger already held.
    pub present: u64,
    /// Lines in the segments written to that this build cannot read.
    pub unrecognised: Vec<Unrecognised>,
}

/// Append rows not already observed in the ledger, without a lock. Creates the
/// ledger directory, never the state root. Each segment is read once.
pub fn append(
    state: &Path,
    rows: &[Row],
    _clock: &dyn Clock,
    _cancel: &CancelToken,
) -> Result<Appended, LedgerError> {
    let mut appended = Appended::default();
    if rows.is_empty() {
        return Ok(appended);
    }
    let root = Directory::open_anchor(state)?;
    match root.mkdir(OsStr::new(DIRECTORY), 0o700) {
        Ok(()) => root.sync()?,
        Err(error) if error.is_exists() => {}
        Err(error) => return Err(error.into()),
    }
    let directory = root.open_directory(OsStr::new(DIRECTORY))?;
    let mut segments = BTreeMap::<String, Vec<&Row>>::new();
    for row in rows {
        segments.entry(row.segment()).or_default().push(row);
    }
    for (segment, rows) in segments {
        let existing = read_segment(&directory, &segment)?;
        let created = existing.is_none();
        let bytes = existing.unwrap_or_default();
        let mut held = BTreeSet::new();
        for line in lines(&segment, &bytes) {
            match line {
                Ok(row) => {
                    held.insert(row.key());
                }
                Err(unrecognised) => appended.unrecognised.push(unrecognised),
            }
        }
        // A torn last line keeps its own line; the new rows start fresh.
        let mut prefix = if bytes.last().is_some_and(|last| *last != b'\n') {
            vec![b'\n']
        } else {
            Vec::new()
        };
        let ledger = JsonLedger::new(&directory, OsStr::new(&segment));
        for row in rows {
            if held.insert(row.key()) {
                let mut line = std::mem::take(&mut prefix);
                serde_json::to_writer(&mut line, row)?;
                line.push(b'\n');
                ledger.append(&line)?;
                appended.added += 1;
            } else {
                appended.present += 1;
            }
        }
        if created && appended.added > 0 {
            directory.sync()?;
        }
    }
    Ok(appended)
}

/// Every row, and every line or file that is not one.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    pub rows: Vec<Row>,
    pub unrecognised: Vec<Unrecognised>,
}

/// Read the whole ledger, oldest segment first. A missing ledger is empty.
pub fn read(state: &Path) -> Result<Ledger, LedgerError> {
    let mut ledger = Ledger::default();
    let directory = match Directory::open_anchor(state)
        .and_then(|root| root.open_directory(OsStr::new(DIRECTORY)))
    {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(ledger),
        Err(error) => return Err(error.into()),
    };
    let mut names = Vec::new();
    for entry in directory.entries()? {
        let entry = entry?;
        if entry == "." || entry == ".." {
            continue;
        }
        match entry.to_str().filter(|name| segments::is_segment(name)) {
            Some(name) => names.push(name.to_owned()),
            None => ledger.unrecognised.push(Unrecognised {
                segment: entry.to_string_lossy().into_owned(),
                line: None,
                reason: "not a ledger segment".into(),
            }),
        }
    }
    names.sort();
    let mut seen = BTreeMap::<(ProjectId, String), Row>::new();
    for name in names {
        let bytes = read_segment(&directory, &name)?.unwrap_or_default();
        for (line, number) in lines(&name, &bytes).zip(1u64..) {
            match line {
                Ok(row) => {
                    if let Some(prior) = seen.get(&row.key()) {
                        if prior != &row {
                            ledger.unrecognised.push(Unrecognised {
                                segment: name.clone(),
                                line: Some(number),
                                reason: "conflicting rows for one attempt".into(),
                            });
                        }
                    } else {
                        seen.insert(row.key(), row.clone());
                        ledger.rows.push(row);
                    }
                }
                Err(unrecognised) => ledger.unrecognised.push(unrecognised),
            }
        }
    }
    Ok(ledger)
}

/// Lane cost over rows, rounded once, in the shape `telemetry stats` reports.
#[derive(Clone, Debug, Serialize)]
pub struct Spend {
    pub attempts: u64,
    pub cost: CostTotal,
}

/// Total the rows that ended in `[since, until)`, optionally for one project.
pub fn spend(
    rows: &[Row],
    since: Option<WallTime>,
    until: Option<WallTime>,
    project: Option<ProjectId>,
) -> Result<Spend, TelemetryError> {
    let mut numerator = 0u128;
    let (mut attempts, mut missing, mut subscription) = (0u64, 0u64, 0u64);
    for row in rows.iter().filter(|row| {
        since.is_none_or(|s| row.finished >= s)
            && until.is_none_or(|u| row.finished < u)
            && project.is_none_or(|p| row.project == p)
    }) {
        attempts += 1;
        match &row.cost {
            Some(cost) => {
                numerator = numerator
                    .checked_add(u128::from(cost.picodollars))
                    .ok_or(TelemetryError::Overflow)?;
                missing += u64::from(!cost.complete);
                subscription += u64::from(cost.subscription);
            }
            None => missing += 1,
        }
    }
    let known = Money::from_numerator(numerator)?;
    Ok(Spend {
        attempts,
        cost: CostTotal {
            cost_basis: "api_equivalent_estimate",
            complete: (missing == 0).then(|| known.clone()),
            known_lower_bound: (attempts == 0 || missing < attempts || numerator > 0)
                .then_some(known),
            priced_attempts: attempts - missing,
            missing_attempts: missing,
            subscription_attempts: subscription,
        },
    })
}

fn read_segment(directory: &Directory, name: &str) -> Result<Option<Vec<u8>>, LedgerError> {
    match directory.read_bounded(OsStr::new(name), SEGMENT_CAP) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Each line of a segment, numbered from 1. The empty piece after a final
/// newline is not a line; every other piece is a row or is reported.
fn lines<'a>(
    segment: &'a str,
    bytes: &'a [u8],
) -> impl Iterator<Item = Result<Row, Unrecognised>> + 'a {
    let mut pieces: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    if pieces.last().is_some_and(|last| last.is_empty()) {
        pieces.pop();
    }
    pieces.into_iter().zip(1u64..).map(move |(line, number)| {
        parse(line).map_err(|reason| Unrecognised {
            segment: segment.to_owned(),
            line: Some(number),
            reason,
        })
    })
}

fn parse(line: &[u8]) -> Result<Row, String> {
    let value: serde_json::Value =
        serde_json::from_slice(line).map_err(|_| "not a JSON object".to_owned())?;
    match value.get("format").and_then(|format| format.as_str()) {
        Some(FORMAT) => {}
        Some(other) => return Err(format!("unrecognised format {other:?}")),
        None => return Err("no format".into()),
    }
    serde_json::from_value(value).map_err(|error| format!("invalid {FORMAT} row: {error}"))
}

pub mod report;

#[cfg(test)]
mod tests;
