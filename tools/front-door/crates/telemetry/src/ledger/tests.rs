use super::*;
use crate::{
    BillingKind, ConfiguredModel, ConfiguredPrices, Rates, Window, stats, validate_prices,
};
use provenant_core::{AttemptId, ProjectId};
use provenant_os::clock::SystemClock;
use provenant_records::{AttemptOutcome, RouteFact, Usage, WorkKey};
use std::path::PathBuf;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const DAY: i64 = 86_400_000_000;

fn state() -> Result<(tempfile::TempDir, PathBuf)> {
    let temp = tempfile::tempdir()?;
    let state = std::fs::canonicalize(temp.path())?.join("state");
    provenant_os::fixture::private_state_dir(&state)?;
    Ok((temp, state))
}

fn prices() -> Result<PriceTable> {
    let model = |model: &str, rate: u64| ConfiguredModel {
        provider: "p".into(),
        model: model.into(),
        sources: vec!["recorded test rates".into()],
        billing: BillingKind::Api,
        zero_rate_verified: false,
        effective_from: None,
        effective_to: None,
        openrouter_id: None,
        litellm_id: None,
        rates: Rates {
            uncached_input: Some(rate),
            cache_read: Some(rate / 10),
            cache_write: Some(rate),
            output_excluding_reasoning: Some(rate * 5),
        },
    };
    Ok(validate_prices(&ConfiguredPrices {
        schema_version: 1,
        id: "ledger-test-prices".into(),
        reviewed_at: "2026-10-22".into(),
        units: "usd_microdollars_per_million_tokens".into(),
        max_review_age_days: None,
        discrepancy_threshold_bps: None,
        models: vec![model("m", 3_000_000), model("n", 1_234_567)],
    })?)
}

/// A terminal attempt that ended `day` days after the epoch.
fn fact(number: u64, day: i64, model: &str, input: Option<u64>) -> Result<AttemptFact> {
    let finished = WallTime::from_micros(day * DAY + i64::try_from(number)?)?;
    Ok(AttemptFact {
        context: None,
        attempt_id: AttemptId::from_bytes(u128::from(number).to_be_bytes()),
        key: WorkKey::new(
            ProjectId::from_bytes([2; 16]),
            "g",
            "n",
            u32::try_from(number)?,
        )?,
        attempt_number: number,
        project_id: ProjectId::from_bytes([2; 16]),
        terminal_state: AttemptOutcome::Succeeded,
        exit_code: Some(0),
        queued_at: finished,
        started_at: Some(finished),
        provider_ms: None,
        finished_at: finished,
        resolved_route: Some(RouteFact {
            provider: Some("p".into()),
            model: Some(model.into()),
            effort: Some("high".into()),
            account_label: Some("account".into()),
        }),
        usage: Usage {
            uncached_input: input,
            cache_read: Some(7_777),
            cache_write: Some(333),
            cache_write_1h: None,
            output_excluding_reasoning: Some(1_001),
            reasoning: Some(9),
        },
        reviewer: None,
        delivery_id: None,
        subject: None,
        head: None,
        output_refs: vec![],
        requested_route: None,
        applied_route: None,
        observed_route: None,
        verified_owner: None,
        lineage: vec![],
        tier: None,
        template: None,
        parent_node: None,
        slot_wait_ms: None,
        reason: None,
        trace: Default::default(),
        transcript: None,
    })
}

fn key(number: u64) -> Result<WorkKey> {
    Ok(WorkKey::new(
        ProjectId::from_bytes([2; 16]),
        "ledger-graph",
        format!("node-{number}"),
        1,
    )?)
}

fn settle(state: &Path, row: &Row) -> Result<Appended> {
    Ok(append(
        state,
        std::slice::from_ref(row),
        &SystemClock::new(),
        &CancelToken::new(),
    )?)
}

#[test]
fn a_settled_attempt_appends_one_row_even_when_settlement_is_retried() -> Result {
    let (_temp, state) = state()?;
    let prices = prices()?;
    let fact = fact(1, 20_000, "m", Some(12_345))?;
    let row = row(&fact, &key(1)?, Some(&prices))?;
    assert_eq!(settle(&state, &row)?.added, 1);
    // A retried settlement adds nothing.
    let retried = settle(&state, &row)?;
    assert_eq!((retried.added, retried.present), (0, 1));
    let ledger = read(&state)?;
    assert!(ledger.unrecognised.is_empty(), "{:?}", ledger.unrecognised);
    assert_eq!(ledger.rows, [row]);
    let stored = &ledger.rows[0];
    assert_eq!(stored.attempt, fact.attempt_id.to_string());
    assert_eq!(stored.graph.as_str(), "ledger-graph");
    assert_eq!(stored.node.as_str(), "node-1");
    assert_eq!(stored.outcome, "succeeded");
    assert_eq!(stored.tokens.reasoning, Some(9));
    Ok(())
}

#[test]
fn settled_cost_for_a_past_window_equals_the_telemetry_report() -> Result {
    let (_temp, state) = state()?;
    let prices = prices()?;
    let mut facts = Vec::new();
    for number in 1..=40 {
        let model = if number % 3 == 0 { "n" } else { "m" };
        // Odd token counts so per-attempt rounding would drift.
        facts.push(fact(
            number,
            20_000 + i64::try_from(number % 5)?,
            model,
            Some(number * 997),
        )?);
    }
    // An unpriced model and an unknown count: the totals become lower bounds.
    facts.push(fact(41, 20_002, "unpriced", Some(5))?);
    facts.push(fact(42, 20_003, "m", None)?);
    let rows = facts
        .iter()
        .enumerate()
        .map(|(index, fact)| -> Result<_> {
            Ok(row(fact, &key(index as u64 + 1)?, Some(&prices))?)
        })
        .collect::<Result<Vec<_>>>()?;
    let first = append(&state, &rows, &SystemClock::new(), &CancelToken::new())?;
    assert_eq!(first.added, 42);
    let second = append(&state, &rows, &SystemClock::new(), &CancelToken::new())?;
    assert_eq!((second.added, second.present), (0, 42));
    let ledger = read(&state)?;
    assert_eq!(ledger.rows.len(), 42);
    let windows = [
        (20_001, 20_003),
        (20_000, 20_005),
        (20_003, 20_004),
        (20_004, 20_005),
        (19_000, 19_001),
    ];
    for (since, until) in windows {
        let since = WallTime::from_micros(since * DAY)?;
        let until = WallTime::from_micros(until * DAY)?;
        let window = Window {
            since: Some(since),
            until: Some(until),
            ..Window::default()
        };
        let report = stats(facts.clone(), &prices, window)?.totals;
        let spend = spend(&ledger.rows, Some(since), Some(until), None)?;
        assert_eq!(spend.attempts, report.attempts);
        assert_eq!(
            serde_json::to_value(&spend.cost)?,
            serde_json::to_value(&report.cost)?
        );
    }
    Ok(())
}

#[test]
fn an_unrecognised_line_is_reported_by_segment_and_line() -> Result {
    let (_temp, state) = state()?;
    let prices = prices()?;
    let first = row(&fact(1, 20_000, "m", Some(1))?, &key(1)?, Some(&prices))?;
    settle(&state, &first)?;
    let segment = first.segment();
    let path = state.join(DIRECTORY).join(&segment);
    let mut text = std::fs::read_to_string(&path)?;
    text.push_str("not json\n");
    text.push_str("{\"format\":\"provenant.usage-ledger.v3\"}\n");
    text.push_str("{\"format\":\"provenant.usage-ledger.v2\",\"attempt\":\"x\"}\n");
    text.push_str("{\"torn");
    std::fs::write(&path, text)?;
    std::fs::write(state.join(DIRECTORY).join("notes.txt"), "stray")?;
    let ledger = read(&state)?;
    assert_eq!(ledger.rows.len(), 1);
    let found: Vec<_> = ledger
        .unrecognised
        .iter()
        .map(|item| (item.segment.as_str(), item.line))
        .collect();
    assert_eq!(
        found,
        [
            ("notes.txt", None),
            (segment.as_str(), Some(2)),
            (segment.as_str(), Some(3)),
            (segment.as_str(), Some(4)),
            (segment.as_str(), Some(5)),
        ]
    );
    assert!(ledger.unrecognised[2].reason.contains("v3"));
    // Appending reports them too, and a torn line does not swallow the new row.
    let second = row(&fact(2, 20_000, "m", Some(2))?, &key(2)?, Some(&prices))?;
    let appended = settle(&state, &second)?;
    assert_eq!(appended.added, 1);
    assert_eq!(appended.unrecognised.len(), 4);
    let ledger = read(&state)?;
    assert_eq!(ledger.rows, [first, second]);
    assert_eq!(ledger.unrecognised.len(), 5);
    Ok(())
}

#[test]
fn a_missing_ledger_reads_empty_and_an_empty_append_creates_nothing() -> Result {
    let (_temp, state) = state()?;
    assert_eq!(settle_all(&state, &[])?, Appended::default());
    assert!(!state.join(DIRECTORY).exists());
    let ledger = read(&state)?;
    assert!(ledger.rows.is_empty() && ledger.unrecognised.is_empty());
    Ok(())
}

fn settle_all(state: &Path, rows: &[Row]) -> Result<Appended> {
    Ok(append(
        state,
        rows,
        &SystemClock::new(),
        &CancelToken::new(),
    )?)
}
