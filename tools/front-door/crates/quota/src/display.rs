//! Display vocabulary and projections. Provider policy and host state are explicit inputs.
use crate::{
    time::{duration, iso, number, parse_time, seconds_between, stamp},
    *,
};
use jiff::{Timestamp, tz::TimeZone};
use serde_json::Value;
use std::{cmp::Ordering, collections::BTreeMap};

#[derive(Clone, Debug, Default)]
pub struct Context {
    pub host: String,
    pub enabled: bool,
    pub providers: Vec<String>,
    pub settings: Value,
    pub entries: Value,
    pub banked: BTreeMap<(String, String, String), BankedCredits>,
    pub activity: BTreeMap<String, f64>,
    pub refreshing: Vec<String>,
    pub timezone: Option<TimeZone>,
}
impl Context {
    fn registered(&self, provider: &str) -> bool {
        self.providers.iter().any(|n| n == provider)
    }
    fn timezone(&self) -> TimeZone {
        self.timezone.clone().unwrap_or_else(TimeZone::system)
    }
}
pub fn words(text: &str) -> Vec<&str> {
    text.split(|c: char| python_whitespace(c) || "_.:-".contains(c))
        .filter(|s| !s.is_empty())
        .collect()
}
fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}
pub fn title_case(text: &str) -> String {
    let text = python_strip(text);
    if text.chars().any(char::is_uppercase) {
        text.into()
    } else {
        words(text)
            .iter()
            .map(|s| upper_first(s))
            .collect::<Vec<_>>()
            .join(" ")
    }
}
pub fn sentence_case(text: &str) -> String {
    upper_first(&if text.chars().any(char::is_uppercase) {
        python_strip(text).into()
    } else {
        words(text).join(" ")
    })
}
pub fn provider_name(name: &str) -> String {
    match name {
        "agy" => "Antigravity",
        "claude" => "Claude",
        "codex" => "Codex",
        "copilot" => "Copilot",
        "cursor" => "Cursor",
        "kiro" => "Kiro",
        "opencode" => "OpenCode",
        _ => return title_case(name),
    }
    .into()
}
pub fn plan_label(plan: Option<&str>) -> Option<String> {
    plan.filter(|p| !p.is_empty())
        .map(|p| match python_strip(p).to_lowercase().as_str() {
            "prolite" | "pro_lite" => "Pro Lite".into(),
            "pro_student" => "Pro (Student)".into(),
            _ => title_case(p),
        })
}
pub fn length_label(minutes: Option<Number>) -> Option<String> {
    let minutes = minutes
        .filter(|m| m.get() != 0.0)?
        .rounded_integer()
        .ok()?
        .integer_value()?;
    Some(match minutes {
        60 => "Hourly".into(),
        300 => "5-hour".into(),
        1440 => "Daily".into(),
        10080 => "Weekly".into(),
        40320..=44640 => "Monthly".into(),
        m if m % 1440 == 0 => format!("{}-day", m / 1440),
        m if m % 60 == 0 => format!("{}-hour", m / 60),
        m => format!("{m}-minute"),
    })
}
pub fn window_labels(context: &Context) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from([("iguana_necktie".into(), "Cloud session credits".into())]);
    if let Some(values) = context.settings["window_labels"].as_object() {
        for (k, v) in values {
            if let Some(v) = v.as_str() {
                labels.insert(k.clone(), v.into());
            }
        }
    }
    labels
}
pub fn window_label(row: &Window, labels: &BTreeMap<String, String>) -> String {
    if let Some(label) = labels.get(&row.name).filter(|s| !s.is_empty()) {
        return label.clone();
    }
    let Some(length) = length_label(row.window_minutes) else {
        return sentence_case(&row.name);
    };
    const LENGTH: &[&str] = &[
        "five",
        "seven",
        "thirty",
        "hour",
        "hours",
        "day",
        "days",
        "week",
        "month",
        "hourly",
        "daily",
        "weekly",
        "monthly",
        "primary",
        "secondary",
        "billing",
        "cycle",
        "session",
        "all",
        "scoped",
        "window",
        "limit",
    ];
    let (base, tag) = row.name.split_once(':').unwrap_or((&row.name, ""));
    let mut extra: Vec<&str> = words(base)
        .into_iter()
        .filter(|word| {
            let lower = word.to_lowercase();
            let digits_unit = lower.chars().last().is_some_and(|c| "mhdw".contains(c))
                && lower.len() > 1
                && lower[..lower.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit() || crate::pending::decimal_digit(c).is_some());
            !LENGTH.contains(&lower.as_str()) && Some(*word) != row.pool.as_deref() && !digits_unit
        })
        .collect();
    if !tag.is_empty() && !tag.chars().all(python_digit) {
        extra.extend(words(tag));
    }
    length
        + &if extra.is_empty() {
            String::new()
        } else {
            format!(" ({})", sentence_case(&extra.join(" ")))
        }
}
fn alert_number(context: &Context) -> Number {
    number(&context.settings["display"]["short_window_alert_percent"])
        .filter(|v| *v > 0.0 && *v <= 100.0)
        .map(Number::Float)
        .unwrap_or(Number::Int(90))
}
pub fn alert_percent(context: &Context) -> f64 {
    alert_number(context).get()
}

pub fn display_name(
    policy: &impl QuotaPolicy,
    context: &Context,
    provider: &str,
    label: &str,
) -> Option<String> {
    if let Some(named) = context.entries[provider][label]["display_name"]
        .as_str()
        .map(python_strip)
        .filter(|s| !s.is_empty())
    {
        return Some(named.chars().take(64).collect());
    }
    if label == "default" || context.registered(provider) && policy.fingerprinted(provider, label) {
        None
    } else {
        Some(title_case(label))
    }
}
fn reset_key(row: &DisplayWindow) -> &str {
    row.window
        .reset_at
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("9999")
}
fn maximum<'a>(
    items: impl Iterator<Item = &'a DisplayWindow>,
    compare: impl Fn(&DisplayWindow, &DisplayWindow) -> Ordering,
) -> Option<&'a DisplayWindow> {
    items.reduce(|best, item| {
        if compare(item, best).is_gt() {
            item
        } else {
            best
        }
    })
}
pub fn display<'a>(
    policy: &impl QuotaPolicy,
    rows: &[&'a DisplayWindow],
    now: Timestamp,
    alert: f64,
) -> Option<&'a DisplayWindow> {
    let current: Vec<_> = rows
        .iter()
        .copied()
        .filter(|row| policy.sets_state(&row.window) && !policy.reset_passed(&row.window, now))
        .collect();
    let spent = maximum(
        current
            .iter()
            .copied()
            .filter(|r| r.pace.state == "exhausted"),
        |a, b| reset_key(a).cmp(reset_key(b)),
    );
    if spent.is_some() {
        return spent;
    }
    let longest = maximum(current.iter().copied(), |a, b| {
        a.window
            .window_minutes
            .unwrap_or_default()
            .numeric_cmp(b.window.window_minutes.unwrap_or_default())
            .then(a.window.compare_used(&b.window))
    })?;
    maximum(
        current.iter().copied().filter(|r| {
            r.window.used() >= alert
                && !std::ptr::eq(*r, longest)
                && r.window
                    .window_minutes
                    .unwrap_or_default()
                    .numeric_cmp(longest.window.window_minutes.unwrap_or_default())
                    .is_lt()
        }),
        |a, b| {
            a.window.compare_used(&b.window).then(
                b.window
                    .window_minutes
                    .unwrap_or_default()
                    .numeric_cmp(a.window.window_minutes.unwrap_or_default()),
            )
        },
    )
    .or(Some(longest))
}
pub fn repeats(window: &DisplayWindow, windows: &[DisplayWindow]) -> bool {
    window.window.name.contains(':')
        && windows.iter().any(|other| {
            !other.window.name.contains(':')
                && other.window.pool == window.window.pool
                && match (other.window.window_minutes, window.window.window_minutes) {
                    (Some(a), Some(b)) => a.numeric_cmp(b).is_eq(),
                    (None, None) => true,
                    _ => false,
                }
                && other.window.compare_used(&window.window).is_eq()
                && stamp(other.window.reset_at.as_deref())
                    .zip(stamp(window.window.reset_at.as_deref()))
                    .is_some_and(|(a, b)| seconds_between(a, b).abs() <= 60.0)
        })
}
pub fn refusal_reset(policy: &impl QuotaPolicy, value: &Snapshot, rows: &[DisplayWindow]) -> bool {
    let allowance = value.allowance.as_ref().cloned().unwrap_or_default();
    if allowance.spend_control_reached == Some(true) {
        return false;
    }
    let reached = allowance
        .limit_reached_type
        .as_deref()
        .filter(|s| !s.is_empty());
    if reached.is_none() && !value.unreported.is_empty() {
        return false;
    }
    let windows: Vec<_> = rows
        .iter()
        .filter(|r| {
            policy.sets_state(&r.window)
                && reached.is_none_or(|name| {
                    name == r.window.name || Some(name) == r.window.raw_kind.as_deref()
                })
        })
        .collect();
    !windows.is_empty() && windows.iter().all(|r| r.reset_passed == Some(true))
}
fn optional_credits(
    policy: &impl QuotaPolicy,
    value: &Snapshot,
    banked: Option<&BankedCredits>,
) -> (Vec<UsageCredit>, Option<Resets>) {
    let counted = value
        .credits
        .as_ref()
        .filter(|c| c.reset_credits.is_some() || c.source.is_some())
        .or(banked);
    let fallback = counted.map(|c| Resets {
        available: c.reset_credits.unwrap_or(0),
        expires_at: vec![],
        source: c.source.clone().unwrap_or("manual".into()),
    });
    (
        policy.normalise_usage_credits(
            value
                .usage_credits
                .as_ref()
                .map(Vec::as_slice)
                .unwrap_or_default(),
        ),
        policy
            .normalise_resets(value.resets.as_ref())
            .or_else(|| policy.normalise_resets(fallback.as_ref())),
    )
}
fn runway(
    policy: &impl QuotaPolicy,
    context: &Context,
    value: &Snapshot,
    resets: Option<&Resets>,
    drawing: bool,
) -> Result<String, QuotaError> {
    let mut parts = vec![];
    if let Some(credit) = policy.credit_draw(value, true) {
        let rate = if drawing {
            policy.burn_rate(value, &credit).filter(|v| *v != 0.0)
        } else {
            None
        };
        parts.push(if credit.unlimited {
            "then unlimited credits".into()
        } else if let Some(rate) = rate {
            let floor = number(&context.settings["credit_floor"]).unwrap_or(0.0);
            format!(
                "then about {} on credits at the measured credit burn rate",
                duration(
                    (credit.balance.map(Number::get).unwrap_or(0.0) - floor).max(0.0) / rate
                        * 3600.0
                )
            )
        } else {
            format!(
                "then {}; credit runway unknown",
                format_credit(&credit, &context.timezone())?
            )
        });
    }
    if let Some(resets) = resets {
        parts.push(format_resets(resets, &context.timezone())?);
    }
    Ok(parts.join("; "))
}
pub fn describe(
    policy: &impl QuotaPolicy,
    context: &Context,
    value: &Snapshot,
    banked: Option<&BankedCredits>,
    now: Timestamp,
) -> Result<DisplayAccount, QuotaError> {
    let labels = window_labels(context);
    let observed = value
        .observed_at
        .as_ref()
        .and_then(|v| parse_time(v.value()));
    let allowance = value.allowance.as_ref().cloned().unwrap_or_default();
    let reached = allowance
        .limit_reached_type
        .as_deref()
        .filter(|s| !s.is_empty());
    let named_refusal = reached.is_some_and(|n| {
        value
            .windows
            .iter()
            .any(|r| n == r.name || Some(n) == r.raw_kind.as_deref())
    });
    let (usage_credits, resets) = optional_credits(policy, value, banked);
    let mut stale = false;
    let mut rows = vec![];
    for original in &value.windows {
        let mut window = original.clone();
        let pool = window
            .pool
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                if context.registered(&value.provider) {
                    policy.window_pool(&value.provider, &window.name)
                } else {
                    None
                }
            })
            .filter(|s| valid_label(s));
        let passed = policy.reset_passed(&window, now);
        let outdated = observed
            .zip(stamp(window.reset_at.as_deref()))
            .is_some_and(|(o, r)| passed && o < r);
        stale |= outdated && policy.sets_state(&window);
        let refused = if named_refusal {
            reached.is_some_and(|n| n == window.name || Some(n) == window.raw_kind.as_deref())
        } else {
            allowance.ordinary_usage_allowed == Some(false)
        };
        let inactive =
            outdated || observed.is_some() && passed && window.used() < NEAR_EXHAUSTED && !refused;
        let pace = policy.pace(&window, now);
        if let Some(pool) = &pool {
            window.pool = Some(pool.clone());
        }
        rows.push(DisplayWindow {
            label: window_label(&window, &labels),
            window,
            pace,
            pool_label: pool.map(|s| format!("{} models", title_case(&s))),
            reset_passed: inactive.then_some(true),
        });
    }
    rows = rows
        .iter()
        .filter(|r| !repeats(r, &rows))
        .cloned()
        .collect();
    let mut effective = value.clone();
    if refusal_reset(policy, value, &rows) {
        effective.allowance = OptionalRecord::Missing;
    }
    let mut worst = policy.binding(
        &rows
            .iter()
            .filter(|r| r.reset_passed != Some(true))
            .collect::<Vec<_>>(),
    );
    if worst.is_none()
        && effective
            .allowance
            .as_ref()
            .is_some_and(|a| a.ordinary_usage_allowed == Some(false))
    {
        worst = policy.binding(&rows.iter().collect::<Vec<_>>());
    }
    let worst_index =
        worst.and_then(|selected| rows.iter().position(|row| std::ptr::eq(row, selected)));
    let worst = worst.cloned();
    let spent = policy.allowance_spent(&effective, worst.as_ref());
    let drawing = policy.credit_draw(value, spent);
    let rate = drawing
        .as_ref()
        .and_then(|credit| policy.burn_rate(value, credit));
    let reserves = runway(policy, context, value, resets.as_ref(), drawing.is_some())?;
    for row in &mut rows {
        if row.pace.state == "ahead_of_pace" && !reserves.is_empty() {
            row.pace.summary += &format!("; {reserves}");
        }
    }
    let longest = maximum(
        rows.iter().filter(|r| {
            r.window.reset_at.as_ref().is_some_and(|s| !s.is_empty())
                && r.window
                    .window_minutes
                    .map(Number::get)
                    .is_some_and(|v| v != 0.0)
        }),
        |a, b| {
            a.window
                .window_minutes
                .unwrap_or_default()
                .numeric_cmp(b.window.window_minutes.unwrap_or_default())
        },
    );
    let signed_out = value.status.as_deref() == Some("signed_out");
    let lapsed = rows.iter().any(|r| r.reset_passed == Some(true));
    let state = if signed_out {
        "signed_out"
    } else if drawing.is_some() {
        "credit_only"
    } else if spent {
        "exhausted"
    } else if let Some(w) = &worst {
        &w.pace.state
    } else if stale || lapsed {
        "unknown"
    } else {
        "unreported"
    };
    let shown = display(
        policy,
        &rows
            .iter()
            .filter(|r| {
                r.window.pool.is_none()
                    || r.window.pool == worst.as_ref().and_then(|w| w.window.pool.clone())
            })
            .collect::<Vec<_>>(),
        now,
        alert_percent(context),
    );
    let mut history = rows.clone();
    if signed_out {
        for row in &mut history {
            if row.window.used() >= 100.0 {
                row.pace.state = "exhausted".into();
            }
        }
    }
    let back = if signed_out {
        policy.binding(&history.iter().collect::<Vec<_>>())
    } else if spent {
        worst.as_ref()
    } else {
        None
    }
    .filter(|w| w.window.used() >= 100.0);
    // Summaries use the enriched row, including reserve advice.
    let worst = worst_index.and_then(|index| rows.get(index));
    let mut detail = if let Some(worst) = worst {
        if let Some(credit) = &drawing {
            format!(
                "{}: allowance spent; drawing on credits, {}{}",
                worst.window.name,
                format_credit(credit, &context.timezone())?,
                rate.filter(|r| *r != 0.0)
                    .map(|r| format!(", about {}/h", grouped(r)))
                    .unwrap_or_default()
            )
        } else if spent && worst.pace.state != "exhausted" {
            format!(
                "{}: allowance spent; {} refuses ordinary use",
                worst.window.name, value.provider
            )
        } else {
            format!("{}: {}", worst.window.name, worst.pace.summary)
        }
    } else {
        value
            .error
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or("unreported".into())
    };
    if stale {
        detail = format!(
            "stale; window reset since reading; {}",
            if worst.is_some() {
                &detail
            } else {
                "awaiting a fresh reading"
            }
        );
    } else if lapsed && worst.is_none() && !spent && drawing.is_none() {
        detail = "reset passed; awaiting a fresh reading".into();
    }
    Ok(DisplayAccount {
        provider: value.provider.clone(),
        account: value.account.clone(),
        plan: value.plan.clone(),
        host: value.host.clone(),
        active: value.active,
        priority: value.priority,
        observed_at: value.observed_at.clone(),
        attempted_at: value.attempted_at.clone(),
        status: if stale && !signed_out {
            Some("stale".into())
        } else {
            value.status.clone()
        },
        error: value.error.clone(),
        source: value.source.clone(),
        provider_name: provider_name(&value.provider),
        display_name: display_name(policy, context, &value.provider, &value.account),
        plan_label: plan_label(value.plan.as_deref()),
        display_window: shown.map(|w| w.window.name.clone()),
        back_at: back.and_then(|w| w.window.reset_at.clone()),
        state: state.into(),
        summary: if signed_out {
            format!(
                "signed out; last read {}",
                value
                    .observed_at
                    .as_ref()
                    .filter(|value| python_truthy(value.value()))
                    .map(PythonValue::python_str)
                    .unwrap_or_else(|| "never".into())
            )
        } else {
            detail
        },
        unreported: value.unreported.clone(),
        coverage_note: if value.unreported.is_empty() {
            String::new()
        } else {
            format!(
                "{} not reported by {}; missing does not mean 0%",
                value.unreported.join(", "),
                value.provider
            )
        },
        credits: value
            .credits
            .as_ref()
            .filter(|c| c.reset_credits.is_some() || c.source.is_some())
            .or(banked)
            .cloned(),
        credit_advice: policy.credit_advice(
            &rows,
            value
                .credits
                .as_ref()
                .filter(|c| c.reset_credits.is_some() || c.source.is_some())
                .or(banked),
            now,
        ),
        use_it_or_lose_it: longest.map(|w| ExpiringHeadroom {
            window: w.window.name.clone(),
            reset_at: w.window.reset_at.clone(),
            unused_percent: w
                .pace
                .details
                .get("headroom_percent")
                .cloned()
                .unwrap_or(Value::Null),
            expiring_headroom_percent: w
                .pace
                .details
                .get("expiring_headroom_percent")
                .cloned()
                .unwrap_or(Value::Null),
        }),
        usage_credits,
        resets,
        drawing_credits: drawing.as_ref().map(|_| true),
        credit_burn_per_hour: drawing.as_ref().map(|_| rate),
        schedule: value.schedule.clone(),
        windows: rows,
    })
}
fn valid_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
pub fn usable(row: &DisplayAccount) -> bool {
    matches!(row.status.as_deref(), Some("ok" | "stale"))
        && row.display_window.as_ref().is_some_and(|s| !s.is_empty())
        && !["exhausted", "credit_only", "signed_out", "unreported"].contains(&row.state.as_str())
}
pub fn headline(policy: &impl QuotaPolicy, context: &Context, rows: &[DisplayAccount]) -> Headline {
    let mode = context.settings["menubar"]["headline"]
        .as_str()
        .filter(|m| ["recent", "worst", "none"].contains(m) || m.contains('/'))
        .unwrap_or("recent");
    let id = |row: &DisplayAccount| format!("{}/{}", row.provider, row.account);
    if mode == "none" {
        return Headline {
            mode: mode.into(),
            account: None,
            window: None,
        };
    }
    if let Some(row) = rows.iter().find(|r| id(r) == mode) {
        return Headline {
            mode: mode.into(),
            account: Some(mode.into()),
            window: row.display_window.clone(),
        };
    }
    let candidates: Vec<_> = rows.iter().filter(|r| usable(r)).collect();
    let (row, window) = if mode == "worst" {
        let peaks: Vec<_> = candidates
            .iter()
            .filter_map(|r| {
                policy
                    .binding(
                        &r.windows
                            .iter()
                            .filter(|w| w.reset_passed != Some(true))
                            .collect::<Vec<_>>(),
                    )
                    .map(|w| (*r, w))
            })
            .collect();
        let peak = peaks.into_iter().reduce(|a, b| {
            if b.1.window.compare_used(&a.1.window).is_gt() {
                b
            } else {
                a
            }
        });
        (peak.map(|p| p.0), peak.map(|p| p.1.window.name.clone()))
    } else {
        let used = candidates
            .iter()
            .copied()
            .filter(|r| context.activity.get(&id(r)).is_some_and(|n| *n != 0.0))
            .reduce(|a, b| {
                if context.activity[&id(b)] > context.activity[&id(a)] {
                    b
                } else {
                    a
                }
            });
        let row = used.or_else(|| {
            candidates
                .iter()
                .copied()
                .min_by_key(|r| r.priority.unwrap_or(100))
        });
        (row, row.and_then(|r| r.display_window.clone()))
    };
    Headline {
        mode: mode.into(),
        account: row.map(id),
        window,
    }
}
pub fn report(
    policy: &impl QuotaPolicy,
    context: &Context,
    values: &[Snapshot],
    now: Timestamp,
) -> Result<Report, QuotaError> {
    let rows = policy
        .freshest(values)
        .iter()
        .map(|v| {
            describe(
                policy,
                context,
                v,
                context
                    .banked
                    .get(&(v.provider.clone(), v.account.clone(), context.host.clone())),
                now,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut groups: BTreeMap<String, Vec<&DisplayAccount>> = BTreeMap::new();
    for row in &rows {
        if row.state != "signed_out" {
            groups.entry(row.provider.clone()).or_default().push(row);
        }
    }
    let order = |r: &DisplayAccount| {
        let longest = maximum(r.windows.iter(), |a, b| {
            a.window
                .window_minutes
                .unwrap_or_default()
                .numeric_cmp(b.window.window_minutes.unwrap_or_default())
        });
        (
            r.state == "exhausted",
            r.state == "credit_only"
                && context.settings["credits_policy"]
                    .as_str()
                    .filter(|s| ["normal", "never", "last_resort"].contains(s))
                    .unwrap_or("last_resort")
                    == "last_resort",
            r.priority.unwrap_or(100),
            longest.map(reset_key).unwrap_or("9999").to_owned(),
            r.account.clone(),
        )
    };
    let placement_preference = groups
        .into_iter()
        .map(|(p, mut group)| {
            group.sort_by_key(|r| order(r));
            (p, group.iter().map(|r| r.account.clone()).collect())
        })
        .collect();
    let next_due_at = rows
        .iter()
        .filter_map(|r| r.schedule.as_ref())
        .filter_map(|s| stamp(Some(&s.next_due_at)).map(|at| (at, s.next_due_at.clone())))
        .min_by_key(|p| p.0)
        .map(|p| p.1);
    let mut refreshing = context.refreshing.clone();
    refreshing.sort();
    Ok(Report {
        schema: REPORT_SCHEMA.into(),
        generated_at: iso(now),
        host: context.host.clone(),
        enabled: context.enabled,
        next_due_at,
        refreshing,
        providers: context.providers.clone(),
        display: DisplaySettings {
            short_window_alert_percent: alert_number(context),
        },
        headline: headline(policy, context, &rows),
        accounts: rows,
        placement_preference,
    })
}
pub fn day(stamp_value: &str, timezone: &TimeZone) -> Result<String, QuotaError> {
    let moment = stamp(Some(stamp_value))
        .ok_or(QuotaError::InvalidTimestamp)?
        .to_zoned(timezone.clone());
    Ok(format!("{} {}", moment.day(), moment.strftime("%b")))
}
fn grouped(value: f64) -> String {
    let number = format!("{:.0}", value.round_ties_even());
    let (sign, digits) = number
        .strip_prefix('-')
        .map(|s| ("-", s))
        .unwrap_or(("", number.as_str()));
    let mut out = String::from(sign);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}
pub fn amount(value: f64, currency: Option<&str>) -> String {
    if currency == Some("USD") {
        format!("${}", grouped(value))
    } else {
        grouped(value)
            + &currency
                .filter(|s| !s.is_empty())
                .map(|s| format!(" {s}"))
                .unwrap_or_default()
    }
}
pub fn used_text(window: &Window) -> String {
    if let Some(limit) = window.limit_amount.map(Number::get).filter(|n| *n != 0.0) {
        format!(
            "{} of {}",
            amount(
                window.used_amount.map(Number::get).unwrap_or(0.0),
                window.currency.as_ref().and_then(Value::as_str)
            ),
            amount(limit, window.currency.as_ref().and_then(Value::as_str))
        )
    } else {
        format!("{}%", general(window.used()))
    }
}
// Python's default :g uses six significant digits and switches at exponents -4 and 6.
fn general(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.into();
    }
    let exponent = value.abs().log10().floor() as i32;
    let decimals = (5 - exponent).max(0) as usize;
    let rounded = format!("{value:.decimals$}")
        .parse::<f64>()
        .unwrap_or(value);
    let exponent = if rounded != 0.0 {
        rounded.abs().log10().floor() as i32
    } else {
        0
    };
    if !(-4..6).contains(&exponent) {
        let text = format!("{rounded:.5e}");
        let (mantissa, exp) = text.split_once('e').unwrap_or((&text, "0"));
        let exp = exp.parse::<i32>().unwrap_or(0);
        format!(
            "{}e{exp:+03}",
            mantissa.trim_end_matches('0').trim_end_matches('.')
        )
    } else {
        let text = format!(
            "{rounded:.precision$}",
            precision = (5 - exponent).max(0) as usize
        );
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').into()
        } else {
            text
        }
    }
}
fn format_credit(entry: &UsageCredit, timezone: &TimeZone) -> Result<String, QuotaError> {
    let text = if entry.unlimited {
        "unlimited credits".into()
    } else if entry.currency.as_ref().is_some_and(|s| !s.is_empty()) {
        format!(
            "{} credit",
            amount(
                entry.balance.map(Number::get).unwrap_or(0.0),
                entry.currency.as_deref()
            )
        )
    } else {
        format!(
            "{} {}",
            grouped(entry.balance.map(Number::get).unwrap_or(0.0)),
            entry
                .unit
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("credits")
        )
    };
    Ok(text
        + " available"
        + &entry
            .expires_at
            .as_ref()
            .filter(|s| !s.is_empty())
            .map(|s| day(s, timezone).map(|d| format!(" (expires {d})")))
            .transpose()?
            .unwrap_or_default())
}
fn format_resets(resets: &Resets, timezone: &TimeZone) -> Result<String, QuotaError> {
    Ok(format!(
        "{} banked reset{}",
        resets.available,
        if resets.available == 1 { "" } else { "s" }
    ) + &resets
        .expires_at
        .first()
        .map(|s| {
            day(s, timezone).map(|d| {
                format!(
                    " ({} {d})",
                    if resets.expires_at.len() > 1 {
                        "next expires"
                    } else {
                        "expires"
                    }
                )
            })
        })
        .transpose()?
        .unwrap_or_default())
}

pub fn credit_text(entry: &UsageCredit, timezone: &TimeZone) -> Result<String, QuotaError> {
    format_credit(entry, timezone)
}
pub fn reset_text(resets: &Resets, timezone: &TimeZone) -> Result<String, QuotaError> {
    format_resets(resets, timezone)
}
fn window_length(minutes: Option<f64>) -> String {
    minutes
        .filter(|v| *v != 0.0)
        .map(|m| duration(m * 60.0))
        .unwrap_or("-".into())
}
fn reset_cell(value: Option<&str>) -> String {
    value
        .filter(|s| !s.is_empty())
        .unwrap_or("-")
        .chars()
        .take(16)
        .collect::<String>()
        .replace('T', " ")
}
/// Returns the complete stdout bytes, including the final newline. No telemetry loading.
pub fn render(value: &Report, verbose: bool, timezone: &TimeZone) -> Result<String, QuotaError> {
    let mut lines = vec![format!(
        "Provider quota on {}{}",
        value.host,
        if value.enabled {
            ""
        } else {
            " (fetching disabled)"
        }
    )];
    let mut rows = vec![];
    for row in &value.accounts {
        let head = vec![
            row.provider.clone(),
            row.account.clone() + if row.active == Some(true) { "*" } else { "" },
            row.plan_label
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| row.plan.clone().filter(|s| !s.is_empty()))
                .unwrap_or("-".into()),
        ];
        for window in &row.windows {
            let w = &window.window;
            let (mut state, mut detail, mut used) = (
                window.pace.state.clone(),
                window.pace.summary.clone(),
                used_text(w),
            );
            if w.limit_amount.map(Number::get).is_some_and(|v| v != 0.0)
                && w.window_minutes.map(Number::get).is_none_or(|v| v == 0.0)
            {
                state = "-".into();
                detail = format!(
                    "{} left; {}",
                    amount(
                        w.limit_amount.map(Number::get).unwrap_or(0.0)
                            - w.used_amount.map(Number::get).unwrap_or(0.0),
                        w.currency.as_ref().and_then(Value::as_str)
                    ),
                    detail
                        .strip_suffix("; window length unreported")
                        .unwrap_or(&detail)
                );
            }
            if window.reset_passed == Some(true) {
                state = "-".into();
                used = "-".into();
                detail = format!("reset since {}", reset_cell(w.reset_at.as_deref()));
            }
            if row.state == "signed_out" {
                state = "signed out".into();
            }
            let label = if window.label.is_empty() {
                &w.name
            } else {
                &window.label
            };
            let label = window
                .pool_label
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|p| format!("{p} · {label}"))
                .unwrap_or_else(|| label.clone());
            let mut cells = head.clone();
            cells.extend([
                label,
                used,
                window_length(w.window_minutes.map(Number::get)),
                reset_cell(w.reset_at.as_deref()),
                state,
                detail,
            ]);
            rows.push(cells);
        }
        if row.windows.is_empty() {
            let mut cells = head;
            cells.extend([
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                row.status.as_deref().unwrap_or("").replace('_', " "),
                row.error.clone().unwrap_or_default(),
            ]);
            rows.push(cells);
        }
    }
    let headers = [
        "provider", "account", "plan", "window", "used", "length", "resets", "pace", "detail",
    ]
    .map(str::to_owned)
    .to_vec();
    // Python's formatter measures Unicode code points, not display cells or UTF-8 bytes.
    let mut widths = headers
        .iter()
        .map(|h| h.chars().count())
        .collect::<Vec<_>>();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    for row in std::iter::once(headers).chain(rows) {
        lines.push(
            row.iter()
                .zip(&widths)
                .map(|(s, w)| s.clone() + &" ".repeat(w - s.chars().count()))
                .collect::<Vec<_>>()
                .join(" ")
                .trim_end()
                .into(),
        );
    }
    if verbose {
        for row in &value.accounts {
            if !row.unreported.is_empty() {
                lines.push(format!(
                    "{}/{}: {} window(s) not reported (missing does not mean 0%)",
                    row.provider,
                    row.account,
                    row.unreported.len()
                ));
            }
        }
    }
    for row in &value.accounts {
        let mut parts = row
            .usage_credits
            .iter()
            .map(|c| credit_text(c, timezone))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(resets) = &row.resets {
            parts.push(reset_text(resets, timezone)?);
        }
        if row.credit_advice.action != "none" {
            parts.push(format!(
                "{}: {}",
                row.credit_advice.action,
                row.credit_advice
                    .details
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
        }
        if !parts.is_empty() {
            lines.push(format!(
                "{}/{}: {}",
                row.provider,
                row.account,
                parts.join("; ")
            ));
        }
    }
    lines.push(
        "* active account (the CLI default identity); routing reads only cached readings".into(),
    );
    Ok(lines.join("\n") + "\n")
}
/// The CLI owns exit-status rendering; a broken stdout remains a typed I/O failure.
pub fn write_report(
    writer: &mut impl std::io::Write,
    value: &Report,
    verbose: bool,
    timezone: &TimeZone,
) -> Result<(), QuotaError> {
    writer
        .write_all(render(value, verbose, timezone)?.as_bytes())
        .map_err(QuotaError::Write)
}
