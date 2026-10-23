use super::*;
use crate::{BillingKind, ConfiguredModel, ConfiguredPrices, Rates, validate_prices};
use provenant_core::OperationId;
use provenant_records::RouteFact;

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
        started_at: WallTime::from_micros(0).unwrap(),
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
        output_paths: vec![],
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
    }
}
fn reduce(facts: impl IntoIterator<Item = AttemptFact>, rate: Option<u64>) -> Stats {
    stats(facts, &prices(rate), Window::default())
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
    let mut second = fact(2, Some(250_000));
    second.resolved_route.as_mut().unwrap().account_label = Some("other".into());
    let report = reduce([first, second], Some(1)).unwrap();
    assert_eq!(report.totals.cost.complete.unwrap().microdollars, 1);
    // The first group rounds 0.5 up; the second rounds 0.25 down.
    assert_eq!(
        report.routes[0]
            .totals
            .cost
            .complete
            .as_ref()
            .unwrap()
            .microdollars,
        1
    );
    assert_eq!(
        report.routes[1]
            .totals
            .cost
            .complete
            .as_ref()
            .unwrap()
            .microdollars,
        0
    );
    let report = reduce([fact(1, Some(499_999))], Some(1)).unwrap();
    assert_eq!(report.totals.cost.complete.unwrap().microdollars, 0);
    let report = reduce([fact(1, Some(500_000))], Some(1)).unwrap();
    assert_eq!(report.totals.cost.complete.unwrap().usd, "0.000001");
    // Summing rounded group totals would return 0 instead of 1.
    let mut a = fact(1, Some(300_000));
    a.resolved_route.as_mut().unwrap().account_label = Some("a".into());
    let mut b = fact(2, Some(300_000));
    b.resolved_route.as_mut().unwrap().account_label = Some("b".into());
    assert_eq!(
        reduce([a, b], Some(1))
            .unwrap()
            .totals
            .cost
            .complete
            .unwrap()
            .microdollars,
        1
    );
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
    assert_eq!(report.totals.cost.known_lower_bound.usd, "2.000000");
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
fn zero_tokens_need_no_price_but_subscription_is_never_assumed_free() {
    assert_eq!(
        reduce([fact(1, Some(0))], None)
            .unwrap()
            .totals
            .cost
            .complete
            .unwrap()
            .microdollars,
        0
    );
    let unpriced = reduce([fact(1, Some(1))], None).unwrap();
    assert!(unpriced.totals.cost.complete.is_none());
    assert_eq!(unpriced.coverage.missing_price, 1);
    let mut free = configured(Some(0));
    free.models[0].billing = BillingKind::Free;
    let table = validate_prices(&free).unwrap();
    assert_eq!(
        stats([fact(1, Some(u64::MAX))], &table, Window::default())
            .unwrap()
            .totals
            .cost
            .complete
            .unwrap()
            .usd,
        "0.000000"
    );
    for billing in [BillingKind::Free, BillingKind::Subscription] {
        free.models[0].billing = billing;
        let table = validate_prices(&free).unwrap();
        let mut missing = fact(1, None);
        missing.usage = Usage::default();
        let report = stats([missing], &table, Window::default()).unwrap();
        assert_eq!(report.totals.cost.complete.unwrap().microdollars, 0);
        assert_eq!(report.totals.cost.missing_attempts, 0);
        assert_eq!(report.coverage.missing_usage, 1);
        let missing = report.coverage.missing_categories;
        assert_eq!(
            [
                missing.uncached_input,
                missing.cache_read,
                missing.cache_write,
                missing.output_excluding_reasoning,
                missing.reasoning,
            ],
            [1; 5]
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
    input.requested_route = input.resolved_route.take();
    let report = reduce([input], Some(1_000_000)).unwrap();
    assert_eq!(report.totals.usage.uncached_input.known, 10);
    assert_eq!(report.totals.usage.reasoning.known, 50);
    assert!(report.routes[0].route.provider.is_none());
    assert!(report.totals.cost.complete.is_none());
    assert_eq!(report.coverage.missing_price, 1);
    let mut config = configured(Some(1_000_000));
    config.models[0].rates.cache_read = Some(2_000_000);
    config.models[0].rates.cache_write = Some(3_000_000);
    config.models[0].rates.output_excluding_reasoning = Some(4_000_000);
    let mut input = fact(1, Some(10));
    input.usage.cache_read = Some(20);
    input.usage.cache_write = Some(30);
    input.usage.output_excluding_reasoning = Some(40);
    input.usage.reasoning = Some(50);
    assert_eq!(
        stats(
            [input],
            &validate_prices(&config).unwrap(),
            Window::default()
        )
        .unwrap()
        .totals
        .cost
        .complete
        .unwrap()
        .microdollars,
        500
    );
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
    assert_eq!(
        reduce([fact(1, Some(1)), fact(1, Some(1))], None).unwrap_err(),
        TelemetryError::InvalidFact("duplicate attempt ID or task/attempt number")
    );
    let mut duplicate_number = fact(1, Some(1));
    duplicate_number.attempt_id = AttemptId::from_bytes([5; 16]);
    assert_eq!(
        reduce([fact(1, Some(1)), duplicate_number], None).unwrap_err(),
        TelemetryError::InvalidFact("duplicate attempt ID or task/attempt number")
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
    let report = stats(
        [fact(1, Some(1)), at_until, outside_project],
        &prices(None),
        window.clone(),
    )
    .unwrap();
    assert_eq!(report.totals.attempts, 1);
    assert_eq!(report.daily_accounts.unwrap()[0].day, "1970-01-01");
    window.until = window.since;
    assert_eq!(
        stats([], &prices(None), window).unwrap_err(),
        TelemetryError::InvalidWindow
    );
    let mut backwards = fact(1, None);
    backwards.finished_at = WallTime::from_micros(-1).unwrap();
    assert_eq!(
        reduce([backwards], None).unwrap_err(),
        TelemetryError::InvalidFact("time order or zero attempt number")
    );
    assert_eq!(
        reduce([fact(0, None)], None).unwrap_err(),
        TelemetryError::InvalidFact("time order or zero attempt number")
    );
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
    let report = stats(
        [unknown, b, a],
        &prices(None),
        Window {
            detail: true,
            ..Window::default()
        },
    )
    .unwrap();
    assert_eq!(report.routes[0].route.model.as_deref(), Some("M"));
    assert_eq!(report.routes[1].route.model.as_deref(), Some("m"));
    assert!(report.routes[2].route.model.is_none());
    let groups = report.daily_accounts.unwrap();
    assert_eq!(groups[0].day, "1970-01-01");
    assert_eq!(groups[1].day, "2026-01-01");
    assert_eq!(groups[2].day, "2026-01-02");
}

#[test]
fn wide_products_are_exact_and_all_overflow_paths_refuse() {
    assert_eq!(
        reduce([fact(1, Some(u64::MAX))], Some(1_000_000))
            .unwrap()
            .totals
            .cost
            .complete
            .unwrap()
            .microdollars,
        u64::MAX
    );
    assert_eq!(
        reduce([fact(1, Some(u64::MAX)), fact(2, Some(1))], Some(0)).unwrap_err(),
        TelemetryError::Overflow
    );
    // Product fits u128 but the rounded microdollar output does not fit u64.
    assert_eq!(
        reduce([fact(1, Some(u64::MAX))], Some(u64::MAX)).unwrap_err(),
        TelemetryError::Overflow
    );
    // Distinct categories overflow the numerator before token totals overflow.
    let mut input = fact(1, Some(u64::MAX));
    input.usage.cache_read = Some(u64::MAX);
    assert_eq!(
        reduce([input], Some(u64::MAX)).unwrap_err(),
        TelemetryError::Overflow
    );
    let first = fact(1, Some(u64::MAX));
    let mut second = fact(2, Some(0));
    second.usage.cache_read = Some(u64::MAX);
    assert_eq!(
        reduce([first, second], Some(u64::MAX)).unwrap_err(),
        TelemetryError::Overflow
    );
}

#[test]
fn attempt_and_group_limits_are_enforced_before_insertion() {
    let table = prices(None);
    let report = stats(
        (1..=MAX_ATTEMPTS).map(|n| fact(n, Some(0))),
        &table,
        Window::default(),
    )
    .unwrap();
    assert_eq!(report.totals.attempts, MAX_ATTEMPTS);
    assert_eq!(
        stats(
            (1..=MAX_ATTEMPTS + 1).map(|n| fact(n, Some(0))),
            &table,
            Window::default()
        )
        .unwrap_err(),
        TelemetryError::TooLarge("100,000 selected attempts")
    );
    let distinct = |n| {
        let mut input = fact(n, Some(0));
        input.resolved_route.as_mut().unwrap().account_label = Some(n.to_string());
        input
    };
    let window = Window {
        detail: true,
        ..Window::default()
    };
    assert_eq!(
        stats((1..=MAX_GROUPS / 2).map(distinct), &table, window.clone())
            .unwrap()
            .routes
            .len(),
        5_000
    );
    assert_eq!(
        stats((1..=MAX_GROUPS / 2 + 1).map(distinct), &table, window).unwrap_err(),
        TelemetryError::TooLarge("10,000 groups")
    );
}

#[test]
fn retained_key_byte_limit_counts_route_and_daily_keys() {
    let mut input = fact(1, Some(0));
    input.resolved_route = Some(RouteFact {
        provider: None,
        model: Some("x".repeat(MAX_KEY_BYTES as usize)),
        effort: None,
        account_label: None,
    });
    assert!(reduce([input.clone()], None).is_ok());
    assert_eq!(
        stats(
            [input.clone()],
            &prices(None),
            Window {
                detail: true,
                ..Window::default()
            }
        )
        .unwrap_err(),
        TelemetryError::TooLarge("1 MiB of group keys")
    );
    input.resolved_route.as_mut().unwrap().provider = Some("p".into());
    assert_eq!(
        reduce([input], None).unwrap_err(),
        TelemetryError::TooLarge("1 MiB of group keys")
    );
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
    assert_eq!(report.prices.reviewed_at, "2026-10-22");
    assert_eq!(report.totals.cost.complete.unwrap().usd, "2.000000");
    input.resolved_route.as_mut().unwrap().model = Some("gpt-6.1-sol-alias".into());
    assert_eq!(
        stats([input], &table, Window::default())
            .unwrap()
            .coverage
            .missing_price,
        1
    );
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
    assert!(
        stats([fact(1, Some(1))], &table, Window::default())
            .unwrap()
            .totals
            .cost
            .complete
            .is_none()
    );
    let bad = minimal.replace("[models.rates]", "[models.rates]\nother = 1");
    let error = toml_edit::de::from_str::<ConfiguredPrices>(&bad).unwrap_err();
    assert!(error.message().contains("unknown field `other`"));
}

#[test]
fn price_validation_refuses_unsupported_units_duplicates_and_unverified_zero() {
    let mut config = configured(Some(1));
    config.schema_version = 2;
    assert_eq!(
        validate_prices(&config).unwrap_err(),
        PriceError::UnsupportedVersion(2)
    );
    config = configured(Some(1));
    config.units = "dollars".into();
    assert_eq!(
        validate_prices(&config).unwrap_err(),
        PriceError::Invalid("units")
    );
    config = configured(Some(1));
    config.models.push(config.models[0].clone());
    assert_eq!(
        validate_prices(&config).unwrap_err(),
        PriceError::Invalid("duplicate provider/model")
    );
    config = configured(Some(0));
    config.models[0].zero_rate_verified = false;
    assert_eq!(
        validate_prices(&config).unwrap_err(),
        PriceError::Invalid("zero rate requires explicit verification")
    );
    config = configured(None);
    config.models[0].billing = BillingKind::Free;
    assert_eq!(
        validate_prices(&config).unwrap_err(),
        PriceError::Invalid("free billing requires four verified zero rates")
    );
    config = configured(Some(1));
    config.reviewed_at = "2026-02-30".into();
    assert_eq!(
        validate_prices(&config).unwrap_err(),
        PriceError::Invalid("identity or review date")
    );
    config = configured(Some(1));
    config.models[0].sources.clear();
    assert_eq!(
        validate_prices(&config).unwrap_err(),
        PriceError::Invalid("model identity or source references")
    );
    config = configured(Some(1));
    config.models[0].model = " ".into();
    assert_eq!(
        validate_prices(&config).unwrap_err(),
        PriceError::Invalid("model identity or source references")
    );
}
