// Integration fixtures use real UTC timestamps in their filesystem records.
#![allow(clippy::disallowed_methods)]
mod support;
use provenant_core::{ProjectId, WallTime};
use provenant_telemetry::ledger::{self, Cost, Labels, Row, Source, Tokens};
use serde_json::{Value, json};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use support::{Result, TestRig, code};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0) as i64
}

fn reading(provider: &str, label: &str, at: i64, used: u64) -> Value {
    json!({
        "schema_version": 1,
        "observed_unix_seconds": at,
        "account_key": label,
        "label": label,
        "provider": provider,
        "status": "available",
        "plan": "pro",
        "tier": null,
        "windows": [{"pool":"five_hour","window_seconds":18000,"used_basis_points":used,"resets_unix_seconds":at + 3600}],
        "credit_balance": null,
        "reset_credits": null,
        "markers": [],
    })
}

fn row(
    attempt: &str,
    provider: &str,
    account: Option<&str>,
    cost: Option<(u64, bool)>,
    finished: i64,
) -> Result<Row> {
    let some = Some(10);
    Ok(Row {
        format: ledger::FORMAT.into(),
        source: Source::Settlement,
        project: ProjectId::from_bytes([3; 16]),
        graph: None,
        node: None,
        attempt: attempt.into(),
        labels: Labels::default(),
        provider: Some(provider.into()),
        model: Some("m".into()),
        effort: None,
        account: account.map(str::to_owned),
        outcome: "succeeded".into(),
        started: None,
        finished: WallTime::from_micros(finished * 1_000_000)?,
        tokens: Tokens {
            uncached_input: some,
            cache_read: some,
            cache_write: some,
            cache_write_1h: None,
            output_excluding_reasoning: some,
            reasoning: some,
        },
        cost: cost.map(|(picodollars, complete)| Cost {
            picodollars,
            complete,
            subscription: false,
            prices: "fixture".into(),
        }),
        context_band: None,
    })
}

fn fixture() -> Result<TestRig> {
    let rig = TestRig::new()?;
    fs::create_dir_all(rig.state.join("quota/history"))?;
    let at = now();
    for (provider, label) in [("claude", "work"), ("codex", "play")] {
        fs::write(
            rig.state
                .join(format!("quota/history/{label}.{provider}.jsonl")),
            format!("{}\n", reading(provider, label, at - 60, 2_500)),
        )?;
    }
    let mut config = String::from("schema_version = 1\n");
    for (provider, label) in [("claude", "work"), ("codex", "play")] {
        config.push_str(&format!(
            "[domains.accounts.entries.{provider}-{label}]\nadapter = \"{provider}\"\nlabel = \"{label}\"\nkey = \"{label}\"\ndirectory = \"{}\"\n[[domains.accounts.entries.{provider}-{label}.plans]]\nname = \"pro\"\nprice = 20.0\ncurrency = \"USD\"\neffective_from = \"2026-01-01\"\n",
            rig.root.join(label).display()
        ));
    }
    fs::write(rig.state.join("config.toml"), config)?;
    // $2 priced and one unpriced attempt on claude/work; $3 priced on
    // codex/play; one attempt with no account.
    let rows = [
        row(
            "aaaaaaaa0000000000000000000000aa",
            "claude",
            Some("work"),
            Some((2_000_000_000_000, true)),
            at - 120,
        )?,
        row(
            "bbbbbbbb0000000000000000000000bb",
            "claude",
            Some("work"),
            None,
            at - 100,
        )?,
        row(
            "cccccccc0000000000000000000000cc",
            "codex",
            Some("play"),
            Some((3_000_000_000_000, true)),
            at - 80,
        )?,
        row(
            "dddddddd0000000000000000000000dd",
            "codex",
            None,
            Some((1_000_000_000_000, true)),
            at - 60,
        )?,
    ];
    let clock = provenant_os::clock::SystemClock::new();
    ledger::append(
        &rig.state,
        &rows,
        &clock,
        &provenant_core::CancelToken::new(),
    )?;
    Ok(rig)
}

fn data(rig: &TestRig, args: &[&str]) -> Result<Value> {
    let output = rig.command(args)?;
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(serde_json::from_slice::<Value>(&output.stdout.bytes)?["data"].clone())
}

#[test]
fn cost_for_one_attempt_is_small_and_works_inside_a_lane() -> Result {
    let rig = fixture()?;
    let output = rig.command(&["telemetry", "cost", "aaaaaaaa", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    assert!(
        output.stdout.bytes.len() < 1024,
        "{}",
        output.stdout.bytes.len()
    );
    let report: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["data"]["totals"]["usd"], "2.000000");
    assert_eq!(report["data"]["totals"]["attempts"], 1);
    let human = rig.command(&["telemetry", "cost", "aaaaaaaa"])?;
    assert_eq!(code(&human), 0);
    assert!(human.stdout.bytes.len() < 1024);
    let missing = rig.command(&["telemetry", "cost", "ffffffff"])?;
    assert_ne!(code(&missing), 0);
    Ok(())
}

#[test]
fn cost_reports_unpriced_and_unattributed_attempts_without_zero_filling() -> Result {
    let rig = fixture()?;
    let report = data(&rig, &["telemetry", "cost", "--by", "account", "--json"])?;
    let totals = &report["totals"];
    assert_eq!(totals["usd"], "6.000000");
    assert_eq!(totals["unpriced_attempts"], 1);
    assert_eq!(totals["complete"], false);
    let group = |key: &str| {
        report["groups"]
            .as_array()
            .and_then(|groups| groups.iter().find(|g| g["key"] == key))
    };
    let work = group("work").ok_or("work group")?;
    assert_eq!(work["usd"], "2.000000");
    assert_eq!(work["unpriced_attempts"], 1);
    let gap = group("unattributed").ok_or("gap group")?;
    assert_eq!(gap["unattributed_attempts"], 1);
    // An unpriced attempt on its own has no cost, not a zero one.
    let alone = data(&rig, &["telemetry", "cost", "bbbbbbbb", "--json"])?;
    assert!(alone["totals"]["usd"].is_null(), "{alone}");
    assert_eq!(alone["totals"]["unpriced_attempts"], 1);
    Ok(())
}

#[test]
fn value_has_a_row_per_account_with_readings_and_states_coverage() -> Result {
    let rig = fixture()?;
    let report = data(&rig, &["telemetry", "value", "--json"])?;
    let accounts = report["accounts"].as_array().ok_or("accounts")?;
    assert_eq!(accounts.len(), 2, "{report}");
    let account = |name: &str| accounts.iter().find(|a| a["account"] == name);
    let work = account("work").ok_or("work")?;
    assert_eq!(work["provider"], "claude");
    assert_eq!(work["usage"]["usd"], "2.000000");
    assert_eq!(work["coverage"]["attempts"], 2);
    assert_eq!(work["coverage"]["unpriced_attempts"], 1);
    assert_eq!(work["quota_windows"][0]["used_basis_points"], 2_500);
    assert_eq!(work["monthly_price"]["amount"], 20.0);
    let play = account("play").ok_or("play")?;
    assert_eq!(play["provider"], "codex");
    assert_eq!(play["usage"]["usd"], "3.000000");
    assert_eq!(play["usage"]["complete"], true);
    // The attempt with no account is stated, not folded into another row.
    assert_eq!(report["unattributed"]["attempts"], 1);
    // The fixture records no accepted receipt, so no cost per outcome is stated.
    assert_eq!(report["accepted_outcomes"]["denominator"], 0);
    assert!(report["accepted_outcomes"]["microdollars_per_accepted_outcome"].is_null());
    let narrowed = data(
        &rig,
        &[
            "telemetry",
            "value",
            "--account",
            "work",
            "--window",
            "weekly",
            "--json",
        ],
    )?;
    assert_eq!(narrowed["accounts"].as_array().map(Vec::len), Some(1));
    assert_eq!(narrowed["accounts"][0]["quota_windows"], json!([]));
    Ok(())
}
