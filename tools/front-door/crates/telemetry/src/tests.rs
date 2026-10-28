use super::*;
use crate::{BillingKind, ConfiguredModel, ConfiguredPrices, Hosts, Rates, validate_prices};
use provenant_core::{HostId, OperationId};
use provenant_records::RouteFact;
use serde_json::json;
use std::collections::BTreeMap;

fn configured(rate: Option<u64>) -> ConfiguredPrices {
    ConfiguredPrices {
        schema_version: 1,
        id: "test-prices".into(),
        reviewed_at: "2026-10-22".into(),
        units: "usd_microdollars_per_million_tokens".into(),
        models: vec![ConfiguredModel {
            provider: "p".into(),
            model: "m".into(),
            sources: vec!["recorded test rates".into()],
            billing: BillingKind::Subscription,
            zero_rate_verified: rate == Some(0),
            rates: Rates {
                uncached_input: rate,
                cache_read: rate,
                cache_write: rate,
                output_excluding_reasoning: rate,
            },
        }],
    }
}
fn prices(rate: Option<u64>) -> PriceTable {
    validate_prices(&configured(rate)).unwrap()
}
fn fact(number: u64, input: Option<u64>) -> AttemptFact {
    AttemptFact {
        attempt_id: AttemptId::from_bytes(u128::from(number).to_be_bytes()),
        task_id: TaskId::from_bytes([1; 16]),
        attempt_number: number,
        operation_id: OperationId::parse("test-operation").unwrap(),
        project_id: ProjectId::from_bytes([2; 16]),
        terminal_state: AttemptOutcome::Succeeded,
        exit_code: Some(0),
        queued_at: WallTime::from_micros(0).unwrap(),
        started_at: Some(WallTime::from_micros(0).unwrap()),
        provider_ms: None,
        finished_at: WallTime::from_micros(1).unwrap(),
        resolved_route: Some(RouteFact {
            provider: Some("p".into()),
            model: Some("m".into()),
            effort: Some("high".into()),
            account_label: Some("account".into()),
        }),
        usage: Usage {
            uncached_input: input,
            cache_read: Some(0),
            cache_write: Some(0),
            output_excluding_reasoning: Some(0),
            reasoning: Some(0),
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
        reason: None,
        trace: Default::default(),
        transcript: None,
    }
}
fn reduce(facts: impl IntoIterator<Item = AttemptFact>, rate: Option<u64>) -> Stats {
    stats(facts, &prices(rate), Window::default())
}
fn detail() -> Window {
    Window {
        detail: true,
        ..Window::default()
    }
}

#[test]
fn quota_usage_separates_accounts_projects_and_chairs_from_lane_tokens() {
    use crate::usage::ChairSession;
    let mut first = fact(1, Some(100));
    first.usage.cache_read = Some(900);
    first.usage.cache_write = None;
    let project = first.project_id;
    let mut second = fact(2, Some(200));
    second.project_id = ProjectId::from_bytes([3; 16]);
    let mut other_account = fact(3, Some(300));
    other_account.resolved_route.as_mut().unwrap().account_label = Some("other".into());
    let mut unknown = fact(4, None);
    unknown.resolved_route = None;
    let chair = || ChairSession {
        adapter: "p".into(),
        account: "account".into(),
        session: "session".into(),
        project: Some(project),
    };
    let rows = quota_usage(
        [first, second, other_account, unknown],
        [chair(), chair()],
        &Window::default(),
    )
    .unwrap();
    assert_eq!(rows.len(), 4);
    let row = rows
        .iter()
        .find(|r| r.project == Some(project) && r.account.as_deref() == Some("account"))
        .unwrap();
    assert_eq!(row.lane_attempts, 1);
    assert_eq!(row.chair_sessions, 1);
    assert_eq!(row.lane_usage.uncached_input.known, 100);
    assert_eq!(row.lane_usage.cache_read.known, 900);
    assert_eq!(row.lane_usage.cache_write.missing, 1);
    assert_eq!(row.lane_usage.cache_write.known, 0);
    assert_eq!(
        rows.iter()
            .find(|r| r.account.is_none())
            .unwrap()
            .lane_usage
            .uncached_input
            .missing,
        1
    );
    assert_eq!(
        rows.iter()
            .map(|r| r.lane_usage.uncached_input.known)
            .sum::<u64>(),
        600
    );
}

#[test]
fn quota_usage_respects_the_window_and_rejects_duplicates_and_overflow() {
    let window = Window {
        since: Some(WallTime::from_micros(1).unwrap()),
        until: Some(WallTime::from_micros(2).unwrap()),
        ..Window::default()
    };
    let mut old = fact(2, Some(900));
    old.finished_at = WallTime::from_micros(0).unwrap();
    let mut future = fact(3, Some(900));
    future.finished_at = WallTime::from_micros(2).unwrap();
    let rows = quota_usage([fact(1, Some(100)), old, future], [], &window).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].lane_usage.uncached_input.known, 100);
    assert!(matches!(
        quota_usage([fact(1, None), fact(1, None)], [], &window),
        Err(TelemetryError::InvalidFact(_))
    ));
    assert!(matches!(
        quota_usage([fact(1, Some(u64::MAX)), fact(2, Some(1))], [], &window),
        Err(TelemetryError::Overflow)
    ));
}
/// Complete cost in microdollars, absent when any cost is unknown.
fn cost(totals: &Totals) -> Option<u64> {
    totals
        .cost
        .complete
        .as_ref()
        .map(|money| money.microdollars)
}
fn account(n: u64, input: u64, label: &str) -> AttemptFact {
    let mut fact = fact(n, Some(input));
    fact.resolved_route.as_mut().unwrap().account_label = Some(label.into());
    fact
}

#[test]
fn empty_reader_has_complete_zero_cost() {
    let report = reduce([], None).unwrap();
    assert_eq!(report.totals.cost.complete.unwrap().usd, "0.000000");
    assert_eq!(report.coverage.selected_attempts, 0);
    assert!(report.routes.is_empty());
    assert!(report.daily_accounts.is_none());
    let wire = serde_json::to_value(&report.coverage).unwrap();
    assert_eq!(wire["missing_categories"]["reasoning"], 0);
}

#[test]
fn rounding_occurs_once_across_categories_attempts_and_groups() {
    let mut first = fact(1, Some(250_000));
    first.usage.reasoning = Some(250_000);
    let report = reduce([first, account(2, 250_000, "other")], Some(1)).unwrap();
    assert_eq!(cost(&report.totals), Some(1));
    // The first group rounds 0.5 up; the second rounds 0.25 down.
    assert_eq!(cost(&report.routes[0].totals), Some(1));
    assert_eq!(cost(&report.routes[1].totals), Some(0));
    let report = reduce([fact(1, Some(499_999))], Some(1)).unwrap();
    assert_eq!(cost(&report.totals), Some(0));
    let report = reduce([fact(1, Some(500_000))], Some(1)).unwrap();
    assert_eq!(report.totals.cost.complete.unwrap().usd, "0.000001");
    // Summing rounded group totals would return 0 instead of 1.
    let report = reduce(
        [account(1, 300_000, "a"), account(2, 300_000, "b")],
        Some(1),
    );
    assert_eq!(cost(&report.unwrap().totals), Some(1));
}

#[test]
fn missing_usage_and_prices_preserve_known_lower_bounds() {
    let mut first = fact(1, Some(1_000_000));
    first.usage.cache_read = None;
    let mut second = fact(2, Some(1_000_000));
    second.resolved_route = None;
    let mut third = fact(3, None);
    third.usage = Usage::default();
    let report = reduce([first, second, third], Some(2_000_000)).unwrap();
    assert!(report.totals.cost.complete.is_none());
    assert_eq!(
        report.totals.cost.known_lower_bound.as_ref().unwrap().usd,
        "2.000000"
    );
    assert_eq!(report.totals.cost.missing_attempts, 3);
    assert_eq!(report.totals.usage.uncached_input.known, 2_000_000);
    assert_eq!(report.totals.usage.uncached_input.missing, 1);
    assert_eq!(report.coverage.missing_usage, 1);
    assert_eq!(report.coverage.missing_price, 1);
    assert_eq!(report.coverage.missing_account, 1);
    assert_eq!(report.coverage.missing_categories.cache_read, 2);
    let wire = serde_json::to_value(&report).unwrap();
    assert!(wire["totals"]["cost"]["complete"].is_null());
    assert_eq!(wire["scope"], "supervised_terminal_attempts");
}

#[test]
fn cost_coverage_distinguishes_unknown_partial_and_measured_zero() {
    let unknown = reduce([fact(1, Some(10)), fact(2, Some(20))], None).unwrap();
    let wire = serde_json::to_value(&unknown).unwrap();
    assert!(wire["totals"]["cost"]["complete"].is_null());
    assert!(wire["totals"]["cost"]["known_lower_bound"].is_null());
    assert_eq!(wire["totals"]["cost"]["priced_attempts"], 0);
    assert_eq!(wire["totals"]["cost"]["missing_attempts"], 2);

    let mut missing = fact(2, Some(20));
    missing.resolved_route = None;
    let partial = reduce([fact(1, Some(1_000_000)), missing], Some(2_000_000)).unwrap();
    assert!(partial.totals.cost.complete.is_none());
    assert_eq!(partial.totals.cost.priced_attempts, 1);
    assert_eq!(partial.totals.cost.missing_attempts, 1);
    assert_eq!(
        partial.totals.cost.known_lower_bound.unwrap().usd,
        "2.000000"
    );
    let zero = reduce([fact(1, Some(0))], None).unwrap();
    assert_eq!(zero.totals.cost.priced_attempts, 1);
    assert_eq!(zero.totals.cost.known_lower_bound.unwrap().usd, "0.000000");
}

#[test]
fn zero_tokens_need_no_price_but_subscription_is_never_assumed_free() {
    assert_eq!(
        cost(&reduce([fact(1, Some(0))], None).unwrap().totals),
        Some(0)
    );
    let unpriced = reduce([fact(1, Some(1))], None).unwrap();
    assert!(unpriced.totals.cost.complete.is_none());
    assert_eq!(unpriced.coverage.missing_price, 1);
    let mut free = configured(Some(0));
    free.models[0].billing = BillingKind::Free;
    let table = validate_prices(&free).unwrap();
    let report = stats([fact(1, Some(u64::MAX))], &table, Window::default()).unwrap();
    assert_eq!(report.totals.cost.complete.unwrap().usd, "0.000000");
    for billing in [BillingKind::Free, BillingKind::Subscription] {
        free.models[0].billing = billing;
        let table = validate_prices(&free).unwrap();
        let mut missing = fact(1, None);
        missing.usage = Usage::default();
        let report = stats([missing], &table, Window::default()).unwrap();
        assert_eq!(cost(&report.totals), Some(0));
        assert_eq!(report.totals.cost.missing_attempts, 0);
        assert_eq!(report.coverage.missing_usage, 1);
        assert_eq!(
            report.coverage.missing_categories,
            Categories {
                uncached_input: 1,
                cache_read: 1,
                cache_write: 1,
                output_excluding_reasoning: 1,
                reasoning: 1
            }
        );
    }
}

#[test]
fn exact_disjoint_usage_is_not_subtracted_or_attributed_from_requested_routes() {
    let mut input = fact(1, Some(10));
    input.usage = Usage {
        uncached_input: Some(10),
        cache_read: Some(20),
        cache_write: Some(30),
        output_excluding_reasoning: Some(40),
        reasoning: Some(50),
    };
    let mut requested = input.clone();
    requested.requested_route = requested.resolved_route.take();
    let report = reduce([requested], Some(1_000_000)).unwrap();
    assert_eq!(report.totals.usage.uncached_input.known, 10);
    assert_eq!(report.totals.usage.reasoning.known, 50);
    assert!(report.routes[0].route.provider.is_none());
    assert!(report.totals.cost.complete.is_none());
    assert_eq!(report.coverage.missing_price, 1);
    let mut config = configured(Some(1_000_000));
    config.models[0].rates.cache_read = Some(2_000_000);
    config.models[0].rates.cache_write = Some(3_000_000);
    config.models[0].rates.output_excluding_reasoning = Some(4_000_000);
    let table = validate_prices(&config).unwrap();
    let report = stats([input], &table, Window::default()).unwrap();
    assert_eq!(cost(&report.totals), Some(500));
}

#[test]
fn retry_totals_and_latest_outcome_use_highest_number_not_arrival_order() {
    let mut latest = fact(9, Some(5));
    latest.terminal_state = AttemptOutcome::Rejected;
    let mut earlier = fact(2, Some(3));
    earlier.finished_at = WallTime::from_micros(2).unwrap();
    earlier.lineage.push(latest.attempt_id);
    let report = reduce([latest, earlier], Some(1_000_000)).unwrap();
    assert_eq!(report.totals.attempts, 2);
    assert_eq!(report.totals.usage.uncached_input.known, 8);
    assert_eq!(report.totals.outcomes[&AttemptOutcome::Succeeded], 1);
    assert_eq!(report.totals.outcomes[&AttemptOutcome::Rejected], 1);
    assert_eq!(report.latest_task_outcomes.len(), 1);
    assert_eq!(report.latest_task_outcomes[&AttemptOutcome::Rejected], 1);
    let duplicate = TelemetryError::InvalidFact("duplicate attempt ID or task/attempt number");
    assert_eq!(
        reduce([fact(1, Some(1)), fact(1, Some(1))], None).unwrap_err(),
        duplicate
    );
    let mut duplicate_number = fact(1, Some(1));
    duplicate_number.attempt_id = AttemptId::from_bytes([5; 16]);
    assert_eq!(
        reduce([fact(1, Some(1)), duplicate_number], None).unwrap_err(),
        duplicate
    );
}

#[test]
fn selection_uses_finished_time_half_open_bounds_and_project() {
    let mut window = Window {
        since: Some(WallTime::from_micros(1).unwrap()),
        until: Some(WallTime::from_micros(2).unwrap()),
        project: Some(ProjectId::from_bytes([2; 16])),
        detail: true,
    };
    let mut at_until = fact(2, Some(10));
    at_until.finished_at = window.until.unwrap();
    let mut outside_project = fact(3, Some(100));
    outside_project.project_id = ProjectId::from_bytes([3; 16]);
    let facts = [fact(1, Some(1)), at_until, outside_project];
    let report = stats(facts, &prices(None), window.clone()).unwrap();
    assert_eq!(report.totals.attempts, 1);
    assert_eq!(report.daily_accounts.unwrap()[0].day, "1970-01-01");
    window.until = window.since;
    let invalid = stats([], &prices(None), window).unwrap_err();
    assert_eq!(invalid, TelemetryError::InvalidWindow);
    let mut backwards = fact(1, None);
    backwards.finished_at = WallTime::from_micros(-1).unwrap();
    let invalid = TelemetryError::InvalidFact("time order or zero attempt number");
    assert_eq!(reduce([backwards], None).unwrap_err(), invalid);
    assert_eq!(reduce([fact(0, None)], None).unwrap_err(), invalid);
}

#[test]
fn daily_groups_are_utc_and_sort_exact_keys_with_missing_last() {
    let mut a = fact(1, Some(0));
    a.finished_at = WallTime::parse("2026-01-02T00:00:00.000000Z").unwrap();
    a.resolved_route.as_mut().unwrap().model = Some("M".into());
    let mut b = fact(2, Some(0));
    b.finished_at = WallTime::parse("2026-01-01T23:59:59.999999Z").unwrap();
    let mut unknown = fact(3, Some(0));
    unknown.resolved_route = None;
    let report = stats([unknown, b, a], &prices(None), detail()).unwrap();
    assert_eq!(report.routes[0].route.model.as_deref(), Some("M"));
    assert_eq!(report.routes[1].route.model.as_deref(), Some("m"));
    assert!(report.routes[2].route.model.is_none());
    let days: Vec<_> = report
        .daily_accounts
        .unwrap()
        .into_iter()
        .map(|g| g.day)
        .collect();
    assert_eq!(days, ["1970-01-01", "2026-01-01", "2026-01-02"]);
}

#[test]
fn wide_products_are_exact_and_all_overflow_paths_refuse() {
    let report = reduce([fact(1, Some(u64::MAX))], Some(1_000_000)).unwrap();
    assert_eq!(cost(&report.totals), Some(u64::MAX));
    let overflow = Err(TelemetryError::Overflow);
    let token_sum = reduce([fact(1, Some(u64::MAX)), fact(2, Some(1))], Some(0));
    assert_eq!(token_sum.map(|_| ()), overflow);
    // Product fits u128 but the rounded microdollar output does not fit u64.
    let output = reduce([fact(1, Some(u64::MAX))], Some(u64::MAX));
    assert_eq!(output.map(|_| ()), overflow);
    // Distinct categories overflow the numerator before token totals overflow.
    let mut input = fact(1, Some(u64::MAX));
    input.usage.cache_read = Some(u64::MAX);
    assert_eq!(reduce([input], Some(u64::MAX)).map(|_| ()), overflow);
    let mut second = fact(2, Some(0));
    second.usage.cache_read = Some(u64::MAX);
    let across = reduce([fact(1, Some(u64::MAX)), second], Some(u64::MAX));
    assert_eq!(across.map(|_| ()), overflow);
}

#[test]
fn attempt_and_group_limits_are_enforced_before_insertion() {
    let table = prices(None);
    let attempts = |n| stats((1..=n).map(|n| fact(n, Some(0))), &table, Window::default());
    assert_eq!(
        attempts(MAX_ATTEMPTS).unwrap().totals.attempts,
        MAX_ATTEMPTS
    );
    assert_eq!(
        attempts(MAX_ATTEMPTS + 1).unwrap_err(),
        TelemetryError::TooLarge("100,000 selected attempts")
    );
    // Each detailed attempt adds one route group and one daily group.
    let groups = |n| {
        stats(
            (1..=n).map(|n| account(n, 0, &n.to_string())),
            &table,
            detail(),
        )
    };
    assert_eq!(groups(MAX_GROUPS / 2).unwrap().routes.len(), 5_000);
    assert_eq!(
        groups(MAX_GROUPS / 2 + 1).unwrap_err(),
        TelemetryError::TooLarge("10,000 groups")
    );
}

#[test]
fn retained_key_byte_limit_counts_route_and_daily_keys() {
    let mut input = fact(1, Some(0));
    input.resolved_route = Some(RouteFact {
        provider: None,
        model: Some("x".repeat(MAX_KEY_BYTES)),
        effort: None,
        account_label: None,
    });
    let too_large = TelemetryError::TooLarge("1 MiB of group keys");
    assert!(reduce([input.clone()], None).is_ok());
    let detailed = stats([input.clone()], &prices(None), detail()).unwrap_err();
    assert_eq!(detailed, too_large);
    input.resolved_route.as_mut().unwrap().provider = Some("p".into());
    assert_eq!(reduce([input], None).unwrap_err(), too_large);
    let mut blank = fact(1, None);
    blank.resolved_route.as_mut().unwrap().effort = Some(" ".into());
    assert_eq!(
        reduce([blank], None).unwrap_err(),
        TelemetryError::InvalidFact("empty route identity")
    );
}

#[test]
fn bundled_toml_uses_exact_rates_and_unknowns_without_aliases() {
    let text = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../config/telemetry-prices.toml"
    ));
    let config: ConfiguredPrices = toml_edit::de::from_str(text).unwrap();
    let table = validate_prices(&config).unwrap();
    let mut input = fact(1, Some(1_000_000));
    input.resolved_route = Some(RouteFact {
        provider: Some("codex".into()),
        model: Some("gpt-6.1-sol".into()),
        effort: None,
        account_label: None,
    });
    let report = stats([input.clone()], &table, Window::default()).unwrap();
    assert_eq!(report.prices.reviewed_at, "2026-10-27");
    assert_eq!(report.totals.cost.complete.unwrap().usd, "2.000000");
    input.resolved_route.as_mut().unwrap().model = Some("gpt-6.1-sol-alias".into());
    let report = stats([input], &table, Window::default()).unwrap();
    assert_eq!(report.coverage.missing_price, 1);
    let minimal = r#"schema_version = 1
id = "unpriced"
reviewed_at = "2026-10-22"
units = "usd_microdollars_per_million_tokens"
[[models]]
provider = "p"
model = "m"
sources = ["unverified"]
billing = "subscription"
[models.rates]
"#;
    let config: ConfiguredPrices = toml_edit::de::from_str(minimal).unwrap();
    let table = validate_prices(&config).unwrap();
    let report = stats([fact(1, Some(1))], &table, Window::default()).unwrap();
    assert!(report.totals.cost.complete.is_none());
    let bad = minimal.replace("[models.rates]", "[models.rates]\nother = 1");
    let error = toml_edit::de::from_str::<ConfiguredPrices>(&bad).unwrap_err();
    assert!(error.message().contains("unknown field `other`"));
}

#[test]
fn price_validation_refuses_unsupported_units_duplicates_and_unverified_zero() {
    let invalid = PriceError::Invalid;
    type Mutation = fn(&mut ConfiguredPrices);
    let cases: [(Mutation, PriceError); 8] = [
        (|c| c.schema_version = 2, PriceError::UnsupportedVersion(2)),
        (|c| c.units = "dollars".into(), invalid("units")),
        (
            |c| c.models.push(c.models[0].clone()),
            invalid("duplicate provider/model"),
        ),
        (
            |c| c.models[0].rates.cache_read = Some(0),
            invalid("zero rate requires explicit verification"),
        ),
        (
            |c| c.models[0].billing = BillingKind::Free,
            invalid("free billing requires four verified zero rates"),
        ),
        (
            |c| c.reviewed_at = "2026-02-30".into(),
            invalid("identity or review date"),
        ),
        (
            |c| c.models[0].sources.clear(),
            invalid("model identity or source references"),
        ),
        (
            |c| c.models[0].model = " ".into(),
            invalid("model identity or source references"),
        ),
    ];
    for (mutate, error) in cases {
        let mut config = configured(Some(1));
        mutate(&mut config);
        assert_eq!(validate_prices(&config).unwrap_err(), error);
    }
}

#[test]
fn trace_has_chrome_event_shape() {
    let mut a = fact(1, Some(5));
    a.started_at = Some(WallTime::from_micros(2_000).unwrap());
    a.finished_at = WallTime::from_micros(9_000).unwrap();
    let mut b = fact(2, None);
    b.started_at = Some(WallTime::from_micros(1_000).unwrap());
    b.finished_at = WallTime::from_micros(3_000).unwrap();
    let trace = crate::trace(&[a, b], &Hosts::default(), None);
    let events = trace["traceEvents"].as_array().unwrap();
    // 1 process + 2 thread names + 2 complete + 2 instants
    assert_eq!(events.len(), 7);
    let phases: Vec<_> = events.iter().map(|e| e["ph"].as_str().unwrap()).collect();
    assert_eq!(phases.iter().filter(|p| **p == "M").count(), 3);
    assert_eq!(phases.iter().filter(|p| **p == "X").count(), 2);
    let spans: Vec<_> = events.iter().filter(|e| e["ph"] == "X").collect();
    assert_eq!(spans[0]["ts"], 1_000);
    assert_eq!(spans[0]["dur"], 2_000);
    assert_eq!(spans[1]["ts"], 2_000);
    assert_eq!(spans[1]["args"]["provider"], "p");
    assert_eq!(spans[1]["args"]["tokens"]["uncached_input"], 5);
    assert!(spans[0]["ts"].as_i64() <= spans[1]["ts"].as_i64());
}

#[test]
fn trace_groups_hosts_and_links_a_child_to_its_parent_across_them() {
    let (local, peer) = (HostId::from_bytes([1; 16]), HostId::from_bytes([2; 16]));
    let mut parent = fact(1, None);
    parent.started_at = Some(WallTime::from_micros(1_000).unwrap());
    parent.finished_at = WallTime::from_micros(9_000).unwrap();
    // Recorded before hosts were stamped: it belongs to the local host.
    let mut child = fact(2, None);
    child.started_at = Some(WallTime::from_micros(2_000).unwrap());
    child.finished_at = WallTime::from_micros(5_000).unwrap();
    child.trace.host = Some(peer);
    child.trace.parent = Some(parent.attempt_id);
    let hosts = Hosts {
        local: Some(local),
        names: BTreeMap::from([(peer, "worker".to_owned())]),
    };
    let trace = crate::trace(&[child.clone(), parent.clone()], &hosts, None);
    let events = trace["traceEvents"].as_array().unwrap();
    let processes: Vec<_> = events
        .iter()
        .filter(|e| e["name"] == "process_name")
        .map(|e| {
            (
                e["pid"].as_u64().unwrap(),
                e["args"]["name"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        processes,
        [
            (1, format!("host {local}").as_str()),
            (2, format!("host worker ({peer})").as_str()),
        ]
    );
    let spans: Vec<_> = events.iter().filter(|e| e["ph"] == "X").collect();
    assert_eq!([&spans[0]["pid"], &spans[1]["pid"]], [1, 2]);
    assert_eq!(spans[1]["args"]["trace"]["host"], peer.to_string());
    let flow: Vec<_> = events.iter().filter(|e| e["cat"] == "parent").collect();
    assert_eq!(flow.len(), 2);
    assert_eq!(
        [
            &flow[0]["ph"],
            &flow[0]["pid"],
            &flow[1]["ph"],
            &flow[1]["pid"]
        ],
        [&json!("s"), &json!(1), &json!("f"), &json!(2)]
    );
    assert_eq!(flow[0]["id"], flow[1]["id"]);
    assert_eq!([&flow[0]["ts"], &flow[1]["ts"]], [2_000, 2_000]);
    // A parent outside the export draws no arrow.
    let orphan = crate::trace(&[child], &hosts, None);
    let events = orphan["traceEvents"].as_array().unwrap();
    assert!(events.iter().all(|e| e["cat"] != "parent"));
    // Without a local host, an unstamped fact sits on an unknown host.
    let unknown = crate::trace(&[parent], &Hosts::default(), None);
    let names: Vec<_> = unknown["traceEvents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["name"] == "process_name")
        .map(|e| e["args"]["name"].clone())
        .collect();
    assert_eq!(names, ["unknown host"]);
}

#[test]
fn claude_attempt_with_cache_usage_reports_all_four_categories() {
    let mut attempt = fact(1, Some(10));
    attempt.resolved_route = Some(RouteFact {
        provider: Some("claude".into()),
        model: Some("claude-sonnet-5".into()),
        effort: None,
        account_label: Some("default".into()),
    });
    attempt.usage = Usage {
        uncached_input: Some(10),
        cache_read: Some(30),
        cache_write: Some(40),
        output_excluding_reasoning: Some(20),
        reasoning: None,
    };
    let mut config = configured(Some(1_000_000));
    config.models[0].provider = "claude".into();
    config.models[0].model = "claude-sonnet-5".into();
    config.models[0].rates.cache_read = Some(500_000);
    config.models[0].rates.cache_write = Some(750_000);
    let prices = validate_prices(&config).unwrap();
    let report = crate::stats([attempt], &prices, Window::default()).unwrap();
    assert_eq!(report.totals.usage.uncached_input.known, 10);
    assert_eq!(report.totals.usage.output_excluding_reasoning.known, 20);
    assert_eq!(report.totals.usage.cache_read.known, 30);
    assert_eq!(report.totals.usage.cache_write.known, 40);
    assert_eq!(report.coverage.missing_categories.uncached_input, 0);
    assert_eq!(
        report
            .coverage
            .missing_categories
            .output_excluding_reasoning,
        0
    );
    assert_eq!(report.coverage.missing_categories.cache_read, 0);
    assert_eq!(report.coverage.missing_categories.cache_write, 0);
}
