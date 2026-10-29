use super::*;
use provenant_quota::{
    AccountScope, ProbeCapture, QuotaEffectKind, QuotaProbeInput, QuotaProbePlan, QuotaStatus,
    QuotaWindow,
};

const NOW: i64 = 1_792_800_000;

fn scope(adapter: &str) -> AccountScope {
    AccountScope {
        adapter: adapter.into(),
        account_key: format!("{adapter}-account"),
        label: format!("{adapter}-label"),
    }
}

fn input() -> QuotaProbeInput {
    QuotaProbeInput {
        now_unix_seconds: NOW,
        deadline_unix_seconds: NOW + 30,
        authority_ref: Some("fixture-authority".into()),
    }
}

fn decode_fixture(adapter: &str, fixture: &str) -> QuotaSnapshot {
    let plan = effect_plan(adapter);
    decode_json(&plan, fixture)
}

fn effect_plan(adapter: &str) -> QuotaProbePlan {
    let kind = match adapter {
        "claude" | "cursor" => QuotaEffectKind::HttpGet,
        "codex" | "agy" => QuotaEffectKind::Command,
        "opencode" => QuotaEffectKind::LocalLedger,
        "command" => unreachable!("command adapter has no quota probe"),
        other => panic!("unlisted adapter {other}"),
    };
    QuotaProbePlan {
        kind,
        account: scope(adapter),
        input: input(),
    }
}

fn decode_json(plan: &QuotaProbePlan, fixture: &str) -> QuotaSnapshot {
    let capture = ProbeCapture {
        stdout: fixture.as_bytes().to_vec(),
        stderr: Vec::new(),
        exit_code: Some(0),
        overflowed: false,
    };
    decode(plan, &capture).expect("redacted provider fixture decodes")
}

fn window<'a>(snapshot: &'a QuotaSnapshot, pool: &str) -> &'a QuotaWindow {
    snapshot
        .windows
        .iter()
        .find(|window| window.pool == pool)
        .unwrap_or_else(|| panic!("missing pool {pool}"))
}

#[test]
fn claude_keeps_named_windows_when_limits_mark_the_weekly_row_inactive() {
    let snapshot = decode_fixture("claude", include_str!("fixtures/claude.json"));

    assert_eq!(snapshot.account.adapter, "claude");
    assert_eq!(snapshot.status, QuotaStatus::Available);
    assert_eq!(snapshot.windows.len(), 2);
    assert_eq!(window(&snapshot, "five_hour").used_basis_points, 6_500);
    assert_eq!(
        window(&snapshot, "five_hour").resets_unix_seconds,
        Some(1_792_829_400)
    );
    assert_eq!(window(&snapshot, "five_hour").window_seconds, Some(18_000));
    assert_eq!(window(&snapshot, "seven_day").used_basis_points, 900);
    assert_eq!(
        window(&snapshot, "seven_day").resets_unix_seconds,
        Some(1_792_958_400)
    );
    assert_eq!(window(&snapshot, "seven_day").window_seconds, Some(604_800));
    let details = snapshot.details.expect("provider details retained");
    assert_eq!(details.plan, None);
    assert_eq!(details.credit_balance, None);
}

#[test]
fn claude_model_scoped_exhaustion_does_not_mark_the_account_exhausted() {
    let mut envelope: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/claude.json")).unwrap();
    let scoped = envelope["payload"]["limits"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|limit| limit["kind"] == "weekly_scoped")
        .unwrap();
    scoped["is_active"] = serde_json::Value::Bool(true);
    scoped["percent"] = serde_json::json!(100);
    let capture = ProbeCapture {
        stdout: serde_json::to_vec(&envelope).unwrap(),
        stderr: Vec::new(),
        exit_code: Some(0),
        overflowed: false,
    };
    let snapshot = decode(&effect_plan("claude"), &capture).unwrap();

    assert_eq!(snapshot.status, QuotaStatus::Available);
    assert_eq!(
        window(&snapshot, "weekly_scoped:Fable").used_basis_points,
        10_000
    );
}

#[test]
fn codex_wham_decodes_weekly_window_banked_resets_and_credit_balance() {
    let snapshot = decode_fixture("codex", include_str!("fixtures/codex.json"));

    assert_eq!(snapshot.status, QuotaStatus::Available);
    assert_eq!(snapshot.windows.len(), 1);
    assert_eq!(window(&snapshot, "primary").used_basis_points, 8_600);
    assert_eq!(window(&snapshot, "primary").window_seconds, Some(604_800));
    assert_eq!(
        window(&snapshot, "primary").resets_unix_seconds,
        Some(1_793_382_074)
    );
    let details = snapshot.details.expect("provider details retained");
    assert_eq!(details.plan.as_deref(), Some("prolite"));
    assert_eq!(details.credit_balance.as_deref(), Some("20365.5924652500"));
    assert_eq!(details.reset_credits, Some(1));
    assert!(details.reset_expiries_unix_seconds.is_empty());
}

#[test]
fn codex_app_server_decodes_extra_windows_and_credit_only_status() {
    let snapshot = decode_fixture("codex", include_str!("fixtures/codex-app-server.json"));

    assert_eq!(snapshot.status, QuotaStatus::CreditOnly);
    assert_eq!(snapshot.windows.len(), 3);
    assert_eq!(window(&snapshot, "primary").used_basis_points, 4_320);
    assert_eq!(window(&snapshot, "primary").window_seconds, Some(18_000));
    assert_eq!(window(&snapshot, "secondary").used_basis_points, 10_000);
    assert_eq!(window(&snapshot, "secondary").window_seconds, Some(604_800));
    assert_eq!(
        window(&snapshot, "gpt-5-codex.primary").used_basis_points,
        240
    );
    let details = snapshot.details.expect("provider details retained");
    assert_eq!(details.plan.as_deref(), Some("Plus"));
    assert_eq!(details.credit_balance.as_deref(), Some("9.50"));
    assert_eq!(details.reset_credits, Some(2));
    assert_eq!(details.reset_expiries_unix_seconds, vec![1_792_929_600]);
    let mut envelope: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/codex-app-server.json")).unwrap();
    envelope["payload"]["result"]["rateLimits"]["secondary"]["usedPercent"] = serde_json::json!(20);
    envelope["payload"]["result"]["rateLimitsByLimitId"]["gpt-5-codex"]["rate_limit"]["primary_window"]
        ["used_percent"] = serde_json::json!(100);
    assert_eq!(
        decode_json(&effect_plan("codex"), &envelope.to_string()).status,
        QuotaStatus::Available
    );
}

#[test]
fn agy_keeps_independent_model_pools_and_reports_available_if_any_pool_has_headroom() {
    let snapshot = decode_fixture("agy", include_str!("fixtures/agy.json"));

    assert_eq!(snapshot.status, QuotaStatus::Available);
    assert_eq!(snapshot.windows.len(), 3);
    assert_eq!(window(&snapshot, "gemini.weekly").used_basis_points, 192);
    assert_eq!(
        window(&snapshot, "gemini.weekly").window_seconds,
        Some(604_800)
    );
    assert_eq!(window(&snapshot, "gemini.5h").used_basis_points, 671);
    assert_eq!(window(&snapshot, "gemini.5h").window_seconds, Some(18_000));
    assert_eq!(window(&snapshot, "claude.weekly").used_basis_points, 10_000);
    assert_eq!(
        window(&snapshot, "claude.weekly").window_seconds,
        Some(604_800)
    );
    let details = snapshot.details.expect("provider details retained");
    assert_eq!(details.fingerprint.as_deref(), Some("google-abcdef123456"));
}

#[test]
fn cursor_uses_usage_summary_and_fills_missing_plan_from_membership() {
    let snapshot = decode_fixture("cursor", include_str!("fixtures/cursor.json"));

    assert_eq!(snapshot.status, QuotaStatus::Exhausted);
    assert_eq!(snapshot.windows.len(), 1);
    assert_eq!(snapshot.windows[0].pool, "billing_cycle");
    assert_eq!(snapshot.windows[0].used_basis_points, 10_000);
    assert_eq!(snapshot.windows[0].window_seconds, Some(2_678_400));
    assert_eq!(snapshot.windows[0].resets_unix_seconds, Some(1_794_608_754));
    assert_eq!(
        snapshot.details.unwrap().plan.as_deref(),
        Some("pro_student")
    );
}

#[test]
fn opencode_decodes_live_partial_local_ledger_and_reports_exhausted_weekly_pool() {
    let snapshot = decode_fixture("opencode", include_str!("fixtures/opencode.json"));
    let details = snapshot.details.as_ref().unwrap();
    assert_eq!(details.source.as_deref(), Some("local-ledger"));
    assert_eq!(details.coverage.as_deref(), Some("partial"));

    assert_eq!(snapshot.status, QuotaStatus::Exhausted);
    assert_eq!(snapshot.windows.len(), 3);
    assert_eq!(snapshot.windows[0].pool, "five_hour");
    assert_eq!(snapshot.windows[0].used_basis_points, 0);
    assert_eq!(snapshot.windows[0].window_seconds, Some(18_000));
    assert_eq!(snapshot.windows[0].resets_unix_seconds, None);
    assert_eq!(snapshot.windows[1].pool, "seven_day");
    assert_eq!(snapshot.windows[1].used_basis_points, 10_000);
    assert_eq!(snapshot.windows[1].window_seconds, Some(604_800));
    assert_eq!(snapshot.windows[1].resets_unix_seconds, Some(1_792_970_400));
    assert_eq!(snapshot.windows[2].pool, "thirty_day");
    assert_eq!(snapshot.windows[2].used_basis_points, 2_404);
    assert_eq!(snapshot.windows[2].window_seconds, Some(2_678_400));
    assert_eq!(snapshot.windows[2].resets_unix_seconds, Some(1_794_957_600));
    let details = snapshot.details.unwrap();
    assert_eq!(details.plan.as_deref(), Some("Go"));
    assert_eq!(
        details.note.as_deref(),
        Some("Partial lower bound: this host only; other devices are not counted")
    );
    let mut envelope: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/opencode.json")).unwrap();
    envelope["payload"]["windows"][2]["used_percent"] = serde_json::json!(125.5);
    assert_eq!(
        window(
            &decode_json(&effect_plan("opencode"), &envelope.to_string()),
            "thirty_day"
        )
        .used_basis_points,
        12_550
    );
}

#[test]
fn no_reported_windows_remain_unknown() {
    let snapshot = decode_fixture(
        "cursor",
        r#"{"payload":{"individualUsage":{},"teamUsage":{}},"details":{}}"#,
    );

    assert!(snapshot.windows.is_empty());
    assert_eq!(snapshot.status, QuotaStatus::Unknown);
}

#[test]
fn quota_effects_follow_specs_and_require_successful_commands() {
    for spec in provenant_records::ADAPTER_SPECS {
        let expected = match spec.name {
            "claude" | "cursor" => QuotaEffectKind::HttpGet,
            "codex" | "agy" => QuotaEffectKind::Command,
            "opencode" => QuotaEffectKind::LocalLedger,
            "command" => {
                assert!(matches!(
                    super::plan(spec.name, &scope(spec.name), &input()),
                    Err(QuotaError::Disabled)
                ));
                continue;
            }
            other => panic!("unlisted adapter {other}"),
        };
        let plan = super::plan(spec.name, &scope(spec.name), &input()).unwrap();
        assert_eq!(plan.kind, expected);
        let capture = provenant_quota::ProbeCapture {
            stdout: br#"{"payload": {}}"#.to_vec(),
            stderr: vec![],
            exit_code: None,
            overflowed: false,
        };
        let result = super::decode(&plan, &capture);
        if expected == QuotaEffectKind::Command {
            assert!(matches!(
                result,
                Err(QuotaError::Invalid("provider quota effect"))
            ));
        } else {
            assert!(result.is_ok(), "{}: {result:?}", spec.name);
        }
    }
    assert!(matches!(
        super::plan("unknown", &scope("unknown"), &input()),
        Err(QuotaError::Disabled)
    ));
}
