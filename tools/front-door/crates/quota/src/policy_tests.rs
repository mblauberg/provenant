use super::*;
use crate::{display::*, time::*};
use jiff::{Timestamp, tz::TimeZone};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::{path::Path, process::Command};

const NOW: &str = "2026-10-23T06:00:00Z";
fn now() -> Timestamp {
    NOW.parse().unwrap()
}
fn at(seconds: f64) -> String {
    iso(add_seconds(now(), seconds).unwrap())
}
fn window(name: &str, used: f64, minutes: Option<f64>, reset: Option<f64>) -> Window {
    Window {
        name: name.into(),
        used_percent: json!(used),
        window_minutes: minutes.map(Number::Float),
        reset_at: reset.map(at),
        ..Default::default()
    }
}
fn snapshot(provider: &str, label: &str, windows: Vec<Window>) -> Snapshot {
    Snapshot {
        schema: SCHEMA.into(),
        provider: provider.into(),
        account: label.into(),
        host: Some("fixture-host".into()),
        active: Some(false),
        observed_at: Some(json!(NOW).into()),
        status: Some("ok".into()),
        windows,
        ..Default::default()
    }
}
fn context() -> Context {
    Context {
        host: "fixture-host".into(),
        enabled: true,
        providers: vec![
            "agy".into(),
            "claude".into(),
            "codex".into(),
            "cursor".into(),
            "opencode".into(),
        ],
        timezone: Some(TimeZone::UTC),
        ..Default::default()
    }
}
fn supported_python_version(output: &str) -> bool {
    crate::oracle::Interpreter::Python.supported_version(output)
}
fn python_available() -> bool {
    crate::oracle::interpreter(crate::oracle::Interpreter::Python).is_some()
}
fn oracle<T: DeserializeOwned>(request: Value) -> T {
    crate::oracle::run(
        crate::oracle::Interpreter::Python,
        &[
            "-B".as_ref(),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/tests/oracle.py")
                .as_os_str(),
        ],
        &request,
    )
    .unwrap()
}
/// Python supplies only dependencies outside this batch. Rust owns all display decisions.
struct OraclePolicy {
    context: Context,
}
impl OraclePolicy {
    fn call<T: DeserializeOwned>(&self, operation: &str, args: Value) -> T {
        oracle(
            json!({"operation":operation,"args":args,"now":NOW,"context":{"host":self.context.host,"settings":self.context.settings,"providers":self.context.providers}}),
        )
    }
}
impl QuotaPolicy for OraclePolicy {
    fn pace(&self, w: &Window, _: Timestamp) -> Pace {
        self.call("pace", json!([w]))
    }
    fn binding<'a>(&self, rows: &[&'a DisplayWindow]) -> Option<&'a DisplayWindow> {
        let index: Option<usize> = self.call("binding", json!([rows]));
        index.map(|i| rows[i])
    }
    fn reset_passed(&self, w: &Window, _: Timestamp) -> bool {
        self.call("reset_passed", json!([w]))
    }
    fn sets_state(&self, w: &Window) -> bool {
        self.call("sets_state", json!([w]))
    }
    fn allowance_spent(&self, s: &Snapshot, w: Option<&DisplayWindow>) -> bool {
        self.call("allowance_spent", json!([s, w]))
    }
    fn credit_draw(&self, s: &Snapshot, spent: bool) -> Option<UsageCredit> {
        self.call("credit_draw", json!([s, spent]))
    }
    fn burn_rate(&self, s: &Snapshot, c: &UsageCredit) -> Option<f64> {
        self.call("burn_rate", json!([s, c]))
    }
    fn credit_advice(
        &self,
        w: &[DisplayWindow],
        c: Option<&BankedCredits>,
        _: Timestamp,
    ) -> CreditAdvice {
        self.call("credit_advice", json!([w, c]))
    }
    fn normalise_usage_credits(&self, c: &[UsageCredit]) -> Vec<UsageCredit> {
        self.call::<Option<Vec<UsageCredit>>>("normalise_usage_credits", json!([c]))
            .unwrap_or_default()
    }
    fn normalise_resets(&self, r: Option<&Resets>) -> Option<Resets> {
        self.call("normalise_resets", json!([r]))
    }
    fn window_pool(&self, p: &str, n: &str) -> Option<String> {
        self.call("window_pool", json!([p, n]))
    }
    fn fingerprinted(&self, p: &str, l: &str) -> bool {
        p == "agy" && l.starts_with("google-")
    }
    fn freshest(&self, v: &[Snapshot]) -> Vec<Snapshot> {
        self.call("freshest", json!([v]))
    }
}
fn compare_report(values: &[Snapshot], context: &Context) -> Report {
    let policy = OraclePolicy {
        context: context.clone(),
    };
    let actual = report(&policy, context, values, now()).unwrap();
    let expected: Value = oracle(
        json!({"operation":"report","now":NOW,"values":values,"context":{"host":context.host,"enabled":context.enabled,"providers":context.providers,"settings":context.settings,"entries":context.entries,"activity":context.activity,"refreshing":context.refreshing,"banked":context.banked.iter().map(|((p,a,h),c)| json!([p,a,h,c])).collect::<Vec<_>>()}}),
    );
    assert_record_eq(&serde_json::to_value(&actual).unwrap(), &expected);
    actual
}
#[test]
fn vocabulary_and_length_policy() {
    assert_eq!(provider_name("agy"), "Antigravity");
    assert_eq!(provider_name("opencode"), "OpenCode");
    assert_eq!(provider_name("big_model"), "Big Model");
    assert_eq!(
        plan_label(Some("pro_student")).as_deref(),
        Some("Pro (Student)")
    );
    assert_eq!(plan_label(Some("Max")).as_deref(), Some("Max"));
    assert_eq!(plan_label(None), None);
    let labels = window_labels(&context());
    for (minutes, name, label) in [
        (Some(300.0), "five_hour", "5-hour"),
        (Some(1440.0), "daily", "Daily"),
        (Some(4320.0), "odd", "3-day (Odd)"),
        (Some(90.0), "burst", "90-minute (Burst)"),
        (Some(44640.0), "billing_cycle", "Monthly"),
        (Some(43200.0), "thirty_day", "Monthly"),
        (Some(10080.0), "seven_day_opus", "Weekly (Opus)"),
        (Some(10080.0), "weekly_scoped:fable", "Weekly (Fable)"),
        (Some(10080.0), "weekly_all:2", "Weekly"),
        (None, "odd_window_name", "Odd window name"),
        (None, "iguana_necktie", "Cloud session credits"),
    ] {
        assert_eq!(
            window_label(&window(name, 0.0, minutes, None), &labels),
            label
        );
    }
    let mut pooled = window("claude.5h", 0.0, Some(300.0), None);
    pooled.pool = Some("claude".into());
    assert_eq!(window_label(&pooled, &labels), "5-hour");
    assert_eq!(
        length_label(Some(Number::Float(90.5))),
        Some("90-minute".into())
    );
    assert_eq!(amount(1234.5, Some("USD")), "$1,234");
    assert_eq!(amount(1235.5, Some("AUD")), "1,236 AUD");
    assert_eq!(duration(172800.0 + 3600.0), "2d 1h");
    assert_eq!(used_text(&window("w", 12.345678, None, None)), "12.3457%");
}
#[test]
fn invalid_alerts_fall_back_and_labels_are_bounded() {
    for value in [json!(null), json!(true), json!(0), json!(101), json!("nan")] {
        let mut c = context();
        c.settings = json!({"display":{"short_window_alert_percent":value}});
        assert_eq!(alert_percent(&c), 90.0);
    }
    let mut c = context();
    c.settings = json!({"display":{"short_window_alert_percent":"80"}});
    assert_eq!(alert_percent(&c), 80.0);
    c.entries =
        json!({"claude":{"work":{"display_name":" x "},"long":{"display_name":"é".repeat(80)}}});
    // These paths need no provider policy calls.
    let p = OraclePolicy { context: c.clone() };
    assert_eq!(display_name(&p, &c, "claude", "work"), Some("x".into()));
    assert_eq!(
        display_name(&p, &c, "claude", "long")
            .unwrap()
            .chars()
            .count(),
        64
    );
}
#[test]
fn display_report_parity_thresholds_staleness_restatement_and_pools() {
    if !python_available() {
        return;
    }
    for (weekly, short, expected) in [
        (30.0, 89.0, "seven_day"),
        (30.0, 90.0, "five_hour"),
        (95.0, 90.0, "five_hour"),
        (95.0, 50.0, "seven_day"),
        (99.0, 92.0, "five_hour"),
        (30.0, 100.0, "five_hour"),
    ] {
        let s = snapshot(
            "claude",
            "work",
            vec![
                window("five_hour", short, Some(300.0), Some(3600.0)),
                window("seven_day", weekly, Some(10080.0), Some(172800.0)),
            ],
        );
        let r = compare_report(&[s], &context());
        assert_eq!(r.accounts[0].display_window.as_deref(), Some(expected));
    }
    let mut s = snapshot(
        "claude",
        "work",
        vec![
            window("five_hour", 100.0, Some(300.0), Some(-3600.0)),
            window("seven_day", 20.0, Some(10080.0), Some(172800.0)),
            window("weekly_all:2", 20.0, Some(10080.0), Some(172850.0)),
        ],
    );
    s.observed_at = Some(json!(at(-7200.0)).into());
    let r = compare_report(&[s], &context());
    assert_eq!(r.accounts[0].status.as_deref(), Some("stale"));
    assert_eq!(r.accounts[0].windows.len(), 2);
    assert_eq!(r.accounts[0].back_at, None);
    for (gemini, claude, expected) in [
        (100.0, 6.0, "claude.weekly"),
        (100.0, 100.0, "gemini.weekly"),
    ] {
        let s = snapshot(
            "agy",
            "google-000000000001",
            vec![
                window("gemini.weekly", gemini, Some(10080.0), Some(172800.0)),
                window("claude.weekly", claude, Some(10080.0), Some(518400.0)),
                window("claude.5h", 12.0, Some(300.0), Some(3600.0)),
            ],
        );
        let r = compare_report(&[s], &context());
        assert_eq!(r.accounts[0].display_window.as_deref(), Some(expected));
    }
}
#[test]
fn display_report_parity_refusals_and_signed_out_history() {
    if !python_available() {
        return;
    }
    for (reached, all_reset, spend_cap, unreported, exhausted) in [
        (Some("primary"), false, false, false, false),
        (Some("primary"), false, false, true, false),
        (Some("secondary"), false, false, false, true),
        (Some("unreported_window"), true, false, true, true),
        (None, false, false, false, true),
        (None, true, false, false, false),
        (None, true, false, true, true),
        (Some("primary"), true, true, false, true),
        (
            Some("workspace_member_credits_depleted"),
            true,
            false,
            false,
            true,
        ),
    ] {
        let mut s = snapshot(
            "codex",
            "default",
            vec![
                window("primary", 60.0, Some(10080.0), Some(-3600.0)),
                window(
                    "secondary",
                    20.0,
                    Some(300.0),
                    Some(if all_reset { -3600.0 } else { 7200.0 }),
                ),
            ],
        );
        s.observed_at = Some(json!(at(-7200.0)).into());
        s.allowance = Some(Allowance {
            ordinary_usage_allowed: Some(false),
            spend_control_reached: Some(spend_cap),
            limit_reached_type: reached.map(Into::into),
        })
        .into();
        if unreported {
            s.unreported.push("missing".into());
        }
        let r = compare_report(&[s], &context());
        assert_eq!(r.accounts[0].state == "exhausted", exhausted);
    }
    let mut s = snapshot(
        "agy",
        "google-000000000001",
        vec![
            window("gemini.weekly", 100.0, Some(10080.0), Some(-172800.0)),
            window("claude.weekly", 100.0, Some(10080.0), Some(-86400.0)),
        ],
    );
    s.status = Some("signed_out".into());
    s.observed_at = Some(json!(at(-604800.0)).into());
    let r = compare_report(&[s], &context());
    assert_eq!(r.accounts[0].state, "signed_out");
    assert!(r.accounts[0].display_window.is_none());
    assert!(r.placement_preference.is_empty());
    assert!(r.accounts[0].back_at.is_some());
}
#[test]
fn display_report_parity_headlines_priority_and_reserves() {
    if !python_available() {
        return;
    }
    let mut a = snapshot(
        "claude",
        "work",
        vec![window("seven_day", 9.0, Some(10080.0), Some(518400.0))],
    );
    a.priority = Some(5);
    let mut b = snapshot(
        "codex",
        "default",
        vec![window("primary", 83.0, Some(10080.0), Some(432000.0))],
    );
    b.priority = Some(1);
    let mut spent = snapshot(
        "claude",
        "away",
        vec![window("seven_day", 100.0, Some(10080.0), Some(259200.0))],
    );
    spent.status = Some("signed_out".into());
    for (mode, expected) in [
        ("recent", Some("claude/work")),
        ("worst", Some("codex/default")),
        ("none", None),
        ("claude/away", Some("claude/away")),
        ("missing/entry", Some("claude/work")),
        ("bogus", Some("claude/work")),
    ] {
        let mut c = context();
        c.activity = BTreeMap::from([
            ("claude/work".into(), 200.0),
            ("codex/default".into(), 100.0),
            ("claude/away".into(), 300.0),
        ]);
        c.settings = json!({"menubar":{"headline":mode}});
        let r = compare_report(&[a.clone(), b.clone(), spent.clone()], &c);
        assert_eq!(r.headline.account.as_deref(), expected);
    }
    for drawing in [false, true] {
        let mut s = snapshot(
            "codex",
            "default",
            vec![window("primary", 33.0, Some(10080.0), Some(159.0 * 3600.0))],
        );
        s.usage_credits = Some(vec![UsageCredit {
            balance: Some(Number::Float(26000.0)),
            unit: Some("credits".into()),
            ..Default::default()
        }])
        .into();
        s.credits = Some(BankedCredits {
            reset_credits: Some(1),
            source: Some("provider".into()),
        });
        if drawing {
            s.allowance = Some(Allowance {
                ordinary_usage_allowed: Some(false),
                ..Default::default()
            })
            .into();
            s.extra.insert(
                "credit_anchor".into(),
                json!({"balance":27000,"observed_at":at(-3600.0)}),
            );
        }
        let r = compare_report(&[s], &context());
        assert!(r.accounts[0].summary.contains("banked reset") || drawing);
        assert_eq!(r.accounts[0].drawing_credits, drawing.then_some(true));
    }
}

#[test]
fn schedule_plan_policy_matrix_and_differential() {
    use crate::schedule::*;
    let cases = [
        (
            "claude",
            None,
            None,
            "timer",
            "ac",
            None,
            "first_fetch",
            1800.0,
            true,
        ),
        (
            "claude",
            Some(-600.0),
            None,
            "timer",
            "ac",
            None,
            "idle",
            1800.0,
            false,
        ),
        (
            "claude",
            Some(-2400.0),
            None,
            "timer",
            "ac",
            None,
            "idle",
            1800.0,
            true,
        ),
        (
            "claude",
            Some(-360.0),
            None,
            "timer",
            "ac",
            Some(-60.0),
            "active",
            300.0,
            true,
        ),
        (
            "claude",
            Some(-120.0),
            None,
            "timer",
            "ac",
            Some(-60.0),
            "active",
            300.0,
            false,
        ),
        (
            "claude",
            Some(-360.0),
            None,
            "timer",
            "ac",
            Some(-358.0),
            "idle",
            1800.0,
            false,
        ),
        (
            "claude",
            Some(-360.0),
            None,
            "timer",
            "ac",
            Some(-7200.0),
            "idle",
            1800.0,
            false,
        ),
        (
            "claude",
            Some(-1200.0),
            Some((90.0, 10800.0, false)),
            "timer",
            "ac",
            None,
            "near_exhaustion",
            900.0,
            true,
        ),
        (
            "claude",
            Some(-300.0),
            Some((60.0, 16200.0, false)),
            "timer",
            "ac",
            None,
            "near_exhaustion",
            900.0,
            false,
        ),
        (
            "claude",
            Some(-2400.0),
            None,
            "timer",
            "battery",
            None,
            "idle",
            3600.0,
            false,
        ),
        (
            "codex",
            Some(-2400.0),
            None,
            "timer",
            "low",
            None,
            "idle",
            14400.0,
            false,
        ),
        (
            "agy",
            Some(-10800.0),
            None,
            "timer",
            "low",
            None,
            "low_power",
            21600.0,
            false,
        ),
        (
            "agy",
            Some(-60.0),
            None,
            "manual",
            "low",
            None,
            "idle",
            7200.0,
            true,
        ),
        (
            "claude",
            Some(-2400.0),
            Some((100.0, 7200.0, false)),
            "timer",
            "ac",
            None,
            "exhausted_until_reset",
            1800.0,
            false,
        ),
        (
            "claude",
            Some(-600.0),
            Some((100.0, -60.0, false)),
            "timer",
            "ac",
            None,
            "reset_passed",
            1800.0,
            true,
        ),
        (
            "claude",
            Some(-600.0),
            Some((100.0, 7200.0, true)),
            "timer",
            "ac",
            None,
            "idle",
            1800.0,
            false,
        ),
        (
            "claude",
            Some(-180.0),
            None,
            "panel",
            "ac",
            None,
            "idle",
            1800.0,
            true,
        ),
        (
            "claude",
            Some(-60.0),
            None,
            "panel",
            "ac",
            None,
            "idle",
            1800.0,
            false,
        ),
        (
            "claude",
            Some(-20.0),
            None,
            "manual",
            "ac",
            None,
            "idle",
            1800.0,
            false,
        ),
        (
            "codex",
            Some(-90.0),
            None,
            "event",
            "ac",
            None,
            "idle",
            3600.0,
            true,
        ),
        (
            "codex",
            Some(-30.0),
            None,
            "wake",
            "ac",
            None,
            "idle",
            3600.0,
            false,
        ),
        (
            "claude",
            Some(-10800.0),
            None,
            "power",
            "ac",
            None,
            "idle",
            1800.0,
            false,
        ),
    ];
    let differential = python_available();
    for (provider, attempt, win, trigger, power, activity, reason, ttl, due) in cases {
        let mut s = snapshot(
            provider,
            "a",
            win.map(|(used, reset, advisory)| {
                let mut w = window("w", used, Some(300.0), Some(reset));
                w.advisory = advisory.then_some(true);
                vec![w]
            })
            .unwrap_or_default(),
        );
        s.attempted_at = attempt.map(|v| json!(at(v)));
        if activity == Some(-358.0) {
            s.fetch = Some(Fetch {
                last_duration_ms: Some(Number::Float(3000.0)),
                ..Default::default()
            });
        }
        let (c, notes) = resolve(provider, &Value::Null, &Value::Null).unwrap();
        let activity_at = activity.map(|d| now().as_second() as f64 + d);
        let id = format!("{provider}/a");
        let (block, is_due) = plan(PlanInput {
            account_id: &id,
            snapshot: Some(&s),
            now: now(),
            config: &c,
            trigger,
            power,
            activity_at,
            ignore_backoff: false,
            notes: &notes,
        })
        .unwrap();
        assert_eq!(
            (&*block.reason, json!(block.ttl_seconds), is_due),
            (reason, json!(ttl as u64), due),
            "{provider} {trigger} {attempt:?} {activity:?}"
        );
        if differential {
            let expected: Value = oracle(
                json!({"operation":"schedule","provider":provider,"account_id":id,"snapshot":s,"now":NOW,"trigger":trigger,"power":power,"activity_at":activity_at}),
            );
            assert_record_eq(&json!(c), &expected[0]);
            assert_record_eq(&serde_json::to_value(&block).unwrap(), &expected[2]);
            assert_eq!(json!(is_due), expected[3]);
        }
    }
}
#[test]
fn schedule_backoff_and_auth_activity() {
    use crate::schedule::*;
    let differential = python_available();
    let (c, notes) = resolve("claude", &Value::Null, &Value::Null).unwrap();
    for (kind, trigger, ignore, activity, due) in [
        ("network", "timer", false, None, false),
        ("network", "manual", false, None, true),
        ("rate_limited", "manual", false, None, false),
        ("rate_limited", "manual", true, None, true),
        ("rate_limited", "timer", true, None, false),
        ("auth", "timer", false, None, false),
        ("auth", "timer", false, Some(-60.0), true),
        ("missing", "panel", false, None, false),
    ] {
        let mut s = snapshot("claude", "a", vec![]);
        s.attempted_at = Some(json!(at(-120.0)));
        s.fetch = Some(Fetch {
            failures: Some(Number::Int(2)),
            error_kind: Some(kind.into()),
            backoff_until: Some(json!(at(300.0))),
            trigger: Some("timer".into()),
            ..Default::default()
        });
        let activity_at = activity.map(|s| now().as_second() as f64 + s);
        let (b, d) = plan(PlanInput {
            account_id: "claude/a",
            snapshot: Some(&s),
            now: now(),
            config: &c,
            trigger,
            power: "ac",
            activity_at,
            ignore_backoff: ignore,
            notes: &notes,
        })
        .unwrap();
        assert_eq!(d, due, "{kind} {trigger} {ignore}");
        if differential {
            let expected: Value = oracle(
                json!({"operation":"schedule","provider":"claude","account_id":"claude/a","snapshot":s,"now":NOW,"trigger":trigger,"activity_at":activity_at,"ignore_backoff":ignore}),
            );
            assert_record_eq(&json!(b), &expected[2]);
            assert_eq!(json!(d), expected[3]);
        }
    }
    for (kind, failures, retry, low, high) in [
        ("network", 1, None, 108.0, 132.0),
        ("network", 2, None, 216.0, 264.0),
        ("other", 3, None, 432.0, 528.0),
        ("network", 9, None, 3240.0, 3960.0),
        ("rate_limited", 1, Some(900.0), 900.0, 900.0),
        ("rate_limited", 5, Some(30.0), 1728.0, 2112.0),
        ("auth", 4, None, 3240.0, 3960.0),
        ("missing", 1, None, 77760.0, 95040.0),
    ] {
        let wait = backoff_seconds(ErrorKind::from_name(kind), failures, retry, &c, "claude/a");
        assert!((low..=high).contains(&wait));
        if differential {
            let expected: String = oracle(
                json!({"operation":"backoff","provider":"claude","seed":"claude/a","args":[kind,failures,retry]}),
            );
            assert_record_eq(
                &json!(wait),
                &serde_json::from_str::<Value>(&expected).unwrap(),
            );
        }
    }
}
#[test]
fn schedule_configuration_precedence_and_validation() {
    use crate::schedule::*;
    let settings = json!({"schedule":{"min_interval_seconds":90,"jitter_fraction":5,"ttl_idle_seconds":999,"base_seconds":1},"backoff":{"base_seconds":60,"max_seconds":"x","min_interval_seconds":5},"providers":{"claude":{"ttl_idle_seconds":1200,"ttl_active_seconds":-1}}});
    let (config, mut notes) =
        resolve("claude", &json!({"ttl_idle_seconds":600}), &settings).unwrap();
    assert_record_eq(
        &json!([
            config.ttl_idle_seconds,
            config.ttl_active_seconds,
            config.min_interval_seconds,
            config.jitter_fraction,
            config.max_seconds,
            config.base_seconds
        ]),
        &json!([600, 300, 90, 0.1, 3600, 60]),
    );
    notes.sort();
    assert_eq!(
        notes,
        vec![
            "quota.backoff.max_seconds is invalid; using the default",
            "quota.providers.claude.ttl_active_seconds is invalid; using the default",
            "quota.schedule.jitter_fraction is invalid; using the default"
        ]
    );
    let (c, notes) = resolve(
        "claude",
        &Value::Null,
        &json!({"schedule":{"min_interval_seconds":900,"max_interval_seconds":600}}),
    )
    .unwrap();
    assert_record_eq(
        &json!([c.min_interval_seconds, c.max_interval_seconds]),
        &json!([60, 21600]),
    );
    assert_eq!(notes.len(), 1);
    for invalid in [json!(true), json!("3"), json!(0), json!(1.5)] {
        let (c, notes) = resolve("claude", &json!({"max_parallel":invalid}), &Value::Null).unwrap();
        assert_eq!(c.max_parallel, 3);
        assert_eq!(notes.len(), 1);
    }
    let (c, _) = resolve("opencode", &json!({"ttl_active_seconds":10}), &Value::Null).unwrap();
    let mut s = snapshot("opencode", "a", vec![]);
    s.attempted_at = Some(json!(at(-180.0)));
    let (b, d) = plan(PlanInput {
        account_id: "opencode/a",
        snapshot: Some(&s),
        now: now(),
        config: &c,
        trigger: "timer",
        power: "ac",
        activity_at: Some(now().as_second() as f64 - 30.0),
        ignore_backoff: false,
        notes: &[],
    })
    .unwrap();
    assert_eq!(json!(b.ttl_seconds), json!(60));
    assert!(d);
}
#[test]
fn time_boundaries_match_python_and_overflow_is_unset() {
    let inputs = vec![
        json!("1e30"),
        json!(1e30),
        json!(-1e30),
        json!("-1e30"),
        json!("nan"),
        json!("inf"),
        json!(true),
        json!(0),
        json!("2026-10-23T06:00:00+10:00"),
        json!("2026-10-23"),
        json!("2026-10-23 06:00:00.123456"),
        json!(1792735200000_i64),
        json!(1792735200.123456),
    ];
    let actual: Vec<_> = inputs.iter().map(|v| parse_time(v).map(iso)).collect();
    assert!(actual[..8].iter().all(Option::is_none));
    if python_available() {
        let expected: Vec<Option<String>> = oracle(json!({"operation":"time","args":inputs}));
        assert_eq!(actual, expected);
    }
}
#[test]
fn activity_reads_only_metadata_and_respects_identity_and_local_dates() {
    use crate::schedule::*;
    let temp = tempfile::tempdir().unwrap();
    let root = &temp.path().canonicalize().unwrap();
    let modified =
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(now().as_second() as u64 - 120);
    for path in [
        "claude/history.jsonl",
        "claude/projects/p/session.jsonl",
        "codex/sessions/2026/10/23/rollout.jsonl",
        "oc/opencode.db-wal",
        ".gemini/antigravity-cli/log/cli.log",
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&path).unwrap();
        file.set_modified(modified).unwrap();
    }
    assert_eq!(
        activity("claude", &root.join("claude"), now(), root, &TimeZone::UTC),
        Some(now().as_second() as f64 - 120.0)
    );
    assert_eq!(
        activity("codex", &root.join("codex"), now(), root, &TimeZone::UTC),
        Some(now().as_second() as f64 - 120.0)
    );
    assert_eq!(
        activity("opencode", &root.join("oc"), now(), root, &TimeZone::UTC),
        Some(now().as_second() as f64 - 120.0)
    );
    assert!(activity("agy", &root.join("unused"), now(), root, &TimeZone::UTC).is_some());
    assert_eq!(activity("cursor", root, now(), root, &TimeZone::UTC), None);
    assert_eq!(
        activity("claude", &root.join("missing"), now(), root, &TimeZone::UTC),
        None
    );
    // In Brisbane this UTC instant has crossed into the next local day.
    let local = "2026-10-23T15:00:00Z".parse().unwrap();
    let next = root.join("codex/sessions/2026/10/24/next.jsonl");
    std::fs::create_dir_all(next.parent().unwrap()).unwrap();
    std::fs::write(&next, []).unwrap();
    assert!(
        activity_paths(
            "codex",
            &root.join("codex"),
            local,
            root,
            &TimeZone::get("Australia/Brisbane").unwrap()
        )
        .contains(&next)
    );
}

#[test]
fn renderer_writes_complete_report_and_propagates_io_failure() {
    if !python_available() {
        return;
    }
    let mut s = snapshot(
        "cursor",
        "work",
        vec![
            window("iguana_necktie", 54.4, None, Some(86400.0)),
            window("five_hour", 100.0, Some(300.0), Some(-3600.0)),
        ],
    );
    s.active = Some(true);
    s.plan = Some("pro_student".into());
    s.observed_at = Some(json!(at(-7200.0)).into());
    s.windows[0].limit_amount = Some(Number::Float(250.0));
    s.windows[0].used_amount = Some(Number::Float(136.0));
    s.windows[0].currency = Some(json!("USD"));
    s.unreported = vec!["weekly".into()];
    s.resets = Some(Resets {
        available: 2,
        expires_at: vec![at(86400.0), at(172800.0)],
        source: "provider".into(),
    })
    .into();
    s.usage_credits = Some(vec![UsageCredit {
        balance: Some(Number::Float(26000.0)),
        unit: Some("credits".into()),
        expires_at: Some(at(86400.0)),
        ..Default::default()
    }])
    .into();
    let mut empty = snapshot("claude", "备用", vec![]);
    empty.status = Some("signed_out".into());
    empty.error = Some("provider authentication needs a refresh".into());
    let mut c = context();
    c.enabled = false;
    let report = compare_report(&[s, empty], &c);
    let mut sink = vec![];
    write_report(&mut sink, &report, false, &TimeZone::UTC).unwrap();
    assert!(sink.ends_with(b"\n"));
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(matches!(
        write_report(&mut Broken, &report, false, &TimeZone::UTC),
        Err(QuotaError::Write(_))
    ));
}
#[test]
fn credit_and_reset_rendering_and_invalid_dates() {
    let credit = UsageCredit {
        balance: Some(Number::Float(1234.5)),
        currency: Some("USD".into()),
        expires_at: Some("2026-10-23T18:00:00Z".into()),
        ..Default::default()
    };
    assert_eq!(
        credit_text(&credit, &TimeZone::get("Australia/Brisbane").unwrap()).unwrap(),
        "$1,234 credit available (expires 24 Oct)"
    );
    let unlimited = UsageCredit {
        unlimited: true,
        ..Default::default()
    };
    assert_eq!(
        credit_text(&unlimited, &TimeZone::UTC).unwrap(),
        "unlimited credits available"
    );
    let resets = Resets {
        available: 1,
        expires_at: vec![],
        source: "provider".into(),
    };
    assert_eq!(
        reset_text(&resets, &TimeZone::UTC).unwrap(),
        "1 banked reset"
    );
    assert!(matches!(
        day("invalid", &TimeZone::UTC),
        Err(QuotaError::InvalidTimestamp)
    ));
}

// Records compare exact numeric spelling, key sets, array order and field presence.
fn assert_record_eq(actual: &Value, expected: &Value) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => assert_eq!(a.to_string(), b.to_string()),
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len());
            for (a, b) in a.iter().zip(b) {
                assert_record_eq(a, b);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
            for (key, a) in a {
                assert_record_eq(a, &b[key]);
            }
        }
        _ => assert_eq!(actual, expected),
    }
}

#[test]
fn iso_time_formats_are_python_compatible() {
    if !python_available() {
        return;
    }
    let inputs: Vec<Value> = [
        "1960-01-01T00:00:00.5Z",
        "0001-01-01",
        "20261023T060000",
        "2026-W43-5T06:00:00",
        "2026-10-23🐍06:00:00",
        "2026-10-23T06:00:00,1234567",
        "2026-10-23T06:00:00z",
        "2026-10-23T06:00:60Z",
        "2026-10-23T06:00:00Z[UTC]",
        "2026-10-23T06",
        "2026-10-23T0600",
        "2026-10-23T06:00:00+10:30:15.5",
        "2026-10-23T06:00:00.9999999Z",
        "2026-10-23T12.5",
        "2026-10-23T12:30.5",
        "2026-10-23T12:30:00+01.5",
        "2026-10-23T12:30:00+01:30.5",
        "2026-W43",
        "1__0",
        "10_0",
        "١٠٠",
    ]
    .into_iter()
    .map(|s| json!(s))
    .collect();
    let actual: Vec<_> = inputs.iter().map(|v| parse_time(v).map(iso)).collect();
    let expected: Vec<Option<String>> = oracle(json!({"operation":"time","args":inputs}));
    for ((input, actual), expected) in inputs.iter().zip(actual).zip(expected) {
        assert_eq!(actual, expected, "{input}");
    }
}

#[test]
fn snapshot_wire_nulls_and_j2_contract() {
    let mut value = json!({"schema":SCHEMA,"provider":"example","account":"work","host":"fixture-host","active":false,"plan":null,"priority":null,"observed_at":NOW,"attempted_at":null,"source":null,"status":"ok","error":null,"windows":[{"name":"budget","raw_kind":null,"used_percent":50,"window_minutes":null,"reset_at":null,"window_start":null,"used_amount":5,"limit_amount":10,"currency":null}],"unreported":[],"credits":null,"usage_credits":null,"resets":null,"allowance":null});
    let record: Snapshot = serde_json::from_value(value.clone()).unwrap();
    assert!(matches!(record.usage_credits, OptionalRecord::Null));
    assert_record_eq(&serde_json::to_value(&record).unwrap(), &value);
    value.as_object_mut().unwrap().remove("usage_credits");
    value.as_object_mut().unwrap().remove("allowance");
    value.as_object_mut().unwrap().remove("resets");
    let absent: Snapshot = serde_json::from_value(value.clone()).unwrap();
    assert!(absent.usage_credits.is_missing());
    assert_record_eq(&json!(absent), &value);
    let mut mutant = value.clone();
    mutant["windows"][0]["used_percent"] = json!("fifty");
    assert!(serde_json::from_value::<Snapshot>(mutant.clone()).is_ok());
    if python_available() {
        let result: Value = oracle(json!({"operation":"schema","value":value,"mutant":mutant}));
        if result["available"] == true {
            assert_eq!(result["valid"], true);
            assert_eq!(result["mutant_rejected"], true);
        } else {
            eprintln!("SKIP J2 validation: Python jsonschema is absent");
        }
    }
}

#[test]
fn review_regression_signed_date_components_are_rejected() {
    for text in ["2026+105", "2026-+1-05", "2026-01-+5"] {
        assert_eq!(parse_time(&json!(text)), None, "{text}");
    }
}

#[test]
fn review_regression_activity_uses_fromtimestamp_fraction_rounding() {
    use crate::schedule::*;
    for (seconds, later) in [
        (
            1700000000.0 + 6.0 * 2.0_f64.powi(-22),
            "2023-11-14T22:13:20.000002Z",
        ),
        (
            1735689600791625390_u64 as f64 / 1e9,
            "2025-01-01T00:00:00.791626Z",
        ),
    ] {
        let mut c = Config::for_provider("claude");
        c.active_window_seconds = Number::Int(0);
        let s = snapshot("claude", "a", vec![]);
        let (block, _) = plan(PlanInput {
            account_id: "claude/a",
            snapshot: Some(&s),
            now: later.parse().unwrap(),
            config: &c,
            trigger: "timer",
            power: "ac",
            activity_at: Some(seconds),
            ignore_backoff: false,
            notes: &[],
        })
        .unwrap();
        assert_eq!(json!(block.ttl_seconds), json!(1800));
        assert_eq!(
            parse_time(&json!(seconds)).map(iso).as_deref(),
            Some(if seconds < 1730000000.0 {
                "2023-11-14T22:13:20.000001Z"
            } else {
                "2025-01-01T00:00:00.791625Z"
            })
        );
    }
}

#[cfg(unix)]
#[test]
fn review_regression_dangling_project_symlink_keeps_real_activity() {
    use crate::schedule::*;
    let temp = tempfile::tempdir().unwrap();
    let root = &temp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("projects/real")).unwrap();
    let session = root.join("projects/real/s.jsonl");
    std::fs::write(&session, "").unwrap();
    std::os::unix::fs::symlink(root.join("nonexistent"), root.join("projects/dead")).unwrap();
    let paths = activity_paths("claude", root, now(), root, &TimeZone::UTC);
    assert!(paths.contains(&session), "{paths:?}");
    assert!(activity("claude", root, now(), root, &TimeZone::UTC).is_some());
}

#[test]
fn review_regression_integer_wire_spelling_is_preserved() {
    let wire = json!({"name":"weekly","used_percent":50,"window_minutes":10080,
        "raw_kind":null,"reset_at":null,"window_start":null,"used_amount":5,"limit_amount":10});
    let record: Window = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(
        serde_json::to_string(&json!(record)).unwrap(),
        serde_json::to_string(&wire).unwrap()
    );
}

#[test]
fn checked_numbers_preserve_large_integers_and_reject_out_of_range_tokens() {
    for text in [
        "-9223372036854775808",
        "9007199254740993",
        "18446744073709551615",
        "50.0",
        "50",
    ] {
        let n: Number = decode_record(text.as_bytes()).unwrap();
        assert_eq!(serde_json::to_string(&n).unwrap(), text);
        let wire = format!(
            "{{\"name\":\"w\",\"used_percent\":{text},\"window_minutes\":{text},\"used_amount\":{text},\"limit_amount\":{text}}}"
        );
        let row: Window = decode_record(wire.as_bytes()).unwrap();
        let value = json!(row);
        for key in [
            "used_percent",
            "window_minutes",
            "used_amount",
            "limit_amount",
        ] {
            assert_eq!(value[key].to_string(), text);
        }
        let state: Fetch = decode_record(
            format!("{{\"last_duration_ms\":{text},\"retry_after_s\":{text}}}").as_bytes(),
        )
        .unwrap();
        assert_eq!(json!(state)["last_duration_ms"].to_string(), text);
        let credit: UsageCredit =
            decode_record(format!("{{\"balance\":{text}}}").as_bytes()).unwrap();
        assert_eq!(json!(credit)["balance"].to_string(), text);
    }
    for text in [
        "18446744073709551616",
        "-9223372036854775809",
        "999999999999999999999999999999999999999",
    ] {
        for wire in [
            text.to_owned(),
            format!("{{\"extra\":[{text}]}}"),
            format!("{{\"name\":\"w\",\"used_percent\":{text}}}"),
        ] {
            assert!(matches!(
                decode_record::<Value>(wire.as_bytes()),
                Err(QuotaError::NumberOutOfRange)
            ));
        }
    }
    assert!(matches!(
        Number::checked_integer(18446744073709551616.0),
        Err(QuotaError::NumberOutOfRange)
    ));
}

#[test]
fn schedule_tolerates_unreadable_percent_null_failures_and_empty_credits() {
    use crate::schedule::*;
    let differential = python_available();
    for used in [json!("unknown"), Value::Null, json!({}), json!(true)] {
        let s: Snapshot = decode_record(serde_json::to_vec(&json!({"provider":"claude","account":"a",
            "windows":[{"name":"w","used_percent":used}], "fetch":{"failures":null,"last_duration_ms":3000},
            "credits":{}})).unwrap().as_slice()).unwrap();
        let (config, notes) = resolve("claude", &Value::Null, &Value::Null).unwrap();
        let (block, due) = plan(PlanInput {
            account_id: "claude/a",
            snapshot: Some(&s),
            now: now(),
            config: &config,
            trigger: "timer",
            power: "ac",
            activity_at: None,
            ignore_backoff: false,
            notes: &notes,
        })
        .unwrap();
        assert_eq!(json!(block.ttl_seconds), json!(1800));
        assert_eq!(json!(block.last_duration_ms), json!(3000));
        assert_eq!(json!(block.failures), json!(0));
        if differential {
            let expected: Value = oracle(
                json!({"operation":"schedule","provider":"claude","account_id":"claude/a","snapshot":s,"now":NOW}),
            );
            assert_record_eq(&json!(config), &expected[0]);
            assert_record_eq(&json!(block), &expected[2]);
            assert_eq!(json!(due), expected[3]);
        }
    }
    if differential {
        let mut s = snapshot("claude", "a", vec![]);
        s.credits = Some(BankedCredits::default());
        let r = compare_report(&[s], &context());
        assert!(r.accounts[0].credits.is_none());
    }
}

#[test]
fn interpreter_version_and_required_differential_contract() {
    for (version, supported) in [
        ("Python 3.9.6", false),
        ("Python 3.10.16", false),
        ("Python 3.11.0", false),
        ("Python 3.12.0", true),
        ("Python 3.14.3\n", true),
        ("not Python", false),
    ] {
        assert_eq!(supported_python_version(version), supported, "{version}");
    }
    for required in ["0", "1"] {
        let out = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "policy_tests::differential_interpreter_probe",
                "--nocapture",
            ])
            .env(
                "PROVENANT_PORT_PYTHON",
                "/nonexistent/provenant-quota-python",
            )
            .env("PROVENANT_PORT_DIFF_REQUIRED", required)
            .output()
            .unwrap();
        assert_eq!(
            out.status.success(),
            required == "0",
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(diagnostic.contains("requires Python 3.12+"), "{diagnostic}");
        if required == "0" {
            assert!(diagnostic.contains("skip:"));
        }
    }
}

#[test]
fn differential_interpreter_probe() {
    let _ = python_available();
}

#[test]
fn provider_pool_oracle_uses_the_real_hook() {
    if !python_available() {
        return;
    }
    let policy = OraclePolicy { context: context() };
    assert_eq!(policy.window_pool("agy", ".x"), None);
    assert_eq!(
        policy.window_pool("agy", "claude.weekly").as_deref(),
        Some("claude")
    );
}

#[test]
#[should_panic]
fn exact_record_comparison_rejects_integer_float_mutants() {
    assert_record_eq(&json!({"n":50}), &json!({"n":50.0}));
}

#[test]
fn display_review_regression_duplicate_names_and_numeric_signed_out_history() {
    if !python_available() {
        return;
    }
    let s = snapshot(
        "claude",
        "a",
        vec![
            window("same", 10.0, Some(10080.0), Some(518400.0)),
            window("same", 80.0, Some(300.0), Some(7200.0)),
        ],
    );
    compare_report(&[s], &context());
    for observed in [
        json!(1792735200),
        json!(1792735200.5),
        json!(1e-7),
        json!(true),
        json!(false),
        json!(["a\"b", true, 1e-5]),
        json!({"seen":true}),
        json!([]),
        json!(0),
        json!(""),
    ] {
        let mut s = snapshot("claude", "a", vec![]);
        s.status = Some("signed_out".into());
        s.observed_at = Some(observed.into());
        compare_report(&[s], &context());
    }
}

#[test]
fn signed_out_history_preserves_input_dictionary_order() {
    if !python_available() {
        return;
    }
    for observed in [
        r#"{"z":1,"a":2}"#,
        r#"{"z":{"y":true,"a":["x",1e-7]},"a":2}"#,
        r#"{"z":1,"a":2,"z":3}"#,
    ] {
        let wire = format!(
            r#"{{"provider":"claude","account":"a","status":"signed_out","observed_at":{observed}}}"#
        );
        let value: Snapshot = decode_record(wire.as_bytes()).unwrap();
        let c = context();
        let actual = describe(
            &OraclePolicy { context: c.clone() },
            &c,
            &value,
            None,
            now(),
        )
        .unwrap();
        let expected: Value =
            oracle(json!({"operation":"report", "values_json":format!("[{wire}]"),"now":NOW}));
        assert_eq!(
            actual.summary,
            expected["accounts"][0]["summary"].as_str().unwrap()
        );
    }
}

#[test]
fn display_review_regression_window_label_uses_python_digit_classes() {
    let labels = BTreeMap::new();
    for (name, expected) in [
        ("½h", "5-hour (½h)"),
        ("²h", "5-hour (²h)"),
        ("١h", "5-hour"),
        ("123h", "5-hour"),
        ("weekly:Ⅻ", "5-hour (Ⅻ)"),
        ("weekly:²", "5-hour"),
        ("weekly:١", "5-hour"),
        ("weekly:123", "5-hour"),
    ] {
        assert_eq!(
            window_label(&window(name, 10.0, Some(300.0), None), &labels),
            expected,
            "{name}"
        );
    }
}

#[test]
fn worst_headline_without_binding_is_an_explicit_python_bug_divergence() {
    if !python_available() {
        return;
    }
    let mut s = snapshot(
        "claude",
        "a",
        vec![window("advisory", 10.0, Some(300.0), Some(3600.0))],
    );
    s.windows[0].advisory = Some(true);
    let mut c = context();
    c.settings = json!({"menubar":{"headline":"worst"}});
    let policy = OraclePolicy { context: c.clone() };
    let mut row = describe(&policy, &c, &s, None, now()).unwrap();
    // A usable-looking account whose policy has no binding row.
    row.state = "unknown".into();
    row.display_window = Some("advisory".into());
    let rows = [row];
    assert_eq!(headline(&policy, &c, &rows).account, None);
    let expected: Value = oracle(json!({"operation":"headline_outcome", "args":[rows],
        "context":{"settings":c.settings}}));
    assert_eq!(expected, json!({"error":"TypeError"}));
}

#[test]
fn schedule_keeps_large_integral_ttls_and_exact_interval_order() {
    use crate::schedule::*;
    let large = 9007199254740993_u64;
    let (config, notes) = resolve(
        "claude",
        &json!({"ttl_idle_seconds":large, "max_interval_seconds":large}),
        &Value::Null,
    )
    .unwrap();
    let (block, _) = plan(PlanInput {
        account_id: "claude/a",
        snapshot: None,
        now: now(),
        config: &config,
        trigger: "timer",
        power: "ac",
        activity_at: None,
        ignore_backoff: false,
        notes: &notes,
    })
    .unwrap();
    assert_eq!(json!(block.ttl_seconds).to_string(), large.to_string());
    let (config, notes) = resolve(
        "claude",
        &json!({"min_interval_seconds":large, "max_interval_seconds":large-1}),
        &Value::Null,
    )
    .unwrap();
    assert_eq!(json!(config.min_interval_seconds), json!(60));
    assert_eq!(json!(config.max_interval_seconds), json!(21600));
    assert_eq!(
        notes,
        ["max_interval_seconds is below min_interval_seconds; using the defaults"]
    );
    if python_available() {
        let account = json!({"ttl_idle_seconds":large, "max_interval_seconds":large});
        let (config, _) = resolve("claude", &account, &Value::Null).unwrap();
        let expected: Value = oracle(
            json!({"operation":"schedule","provider":"claude","account_id":"claude/a","account_quota":account,"now":NOW}),
        );
        assert_record_eq(&json!(config), &expected[0]);
        assert_record_eq(&json!(block), &expected[2]);
    }
}

#[test]
fn display_compares_and_labels_large_integer_windows_exactly() {
    if !python_available() {
        return;
    }
    let mut rows = Vec::new();
    for (name, minutes) in [
        ("first", 9007199254740992_u64),
        ("second", 9007199254740993),
    ] {
        let window: Window = decode_record(
            serde_json::to_vec(&json!({"name":name,"used_percent":10,"window_minutes":minutes}))
                .unwrap()
                .as_slice(),
        )
        .unwrap();
        assert_eq!(
            window_label(&window, &BTreeMap::new()),
            format!("{minutes}-minute ({})", sentence_case(name))
        );
        rows.push(DisplayWindow {
            window,
            pace: Pace::default(),
            label: String::new(),
            pool_label: None,
            reset_passed: None,
        });
    }
    let policy = OraclePolicy { context: context() };
    assert_eq!(
        display(&policy, &rows.iter().collect::<Vec<_>>(), now(), 90.0)
            .unwrap()
            .window
            .name,
        "second"
    );
    assert!(matches!(
        decode_record::<Value>(b"{\"ttl_idle_seconds\":18446744073709551616}"),
        Err(QuotaError::NumberOutOfRange)
    ));
}

#[test]
fn schedule_caps_products_before_integer_or_float_overflow() {
    use crate::schedule::*;
    for factor in [json!(u64::MAX), json!(1e308)] {
        let account = json!({"battery_factor":factor});
        let (config, notes) = resolve("claude", &account, &Value::Null).unwrap();
        let result = plan(PlanInput {
            account_id: "claude/a",
            snapshot: None,
            now: now(),
            config: &config,
            trigger: "timer",
            power: "battery",
            activity_at: None,
            ignore_backoff: false,
            notes: &notes,
        });
        let (block, _) = result.unwrap();
        assert_eq!(json!(block.ttl_seconds), json!(21600));
        if python_available() {
            let expected: Value = oracle(
                json!({"operation":"schedule","provider":"claude","account_id":"claude/a","account_quota":account,"power":"battery","now":NOW}),
            );
            assert_record_eq(&json!(block), &expected[2]);
        }
    }
}
