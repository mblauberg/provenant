use crate::launch::{
    Access, PrepareInput, Session, SetupStep, Stdin, base_env, exact, package_grant, subtree,
};
use crate::{LaunchSpec, ProviderError, ProviderEvent as E, StreamState, stream::wire};
use provenant_confinement::CodexPolicy;
use provenant_confinement::permissions::{
    AuthenticationProfile, NetworkEndpoint, ProviderProfile, ProviderProfileSpec,
};
use serde::Deserialize;
use std::ffi::OsString;

pub struct Codex;

pub const REGISTRATION: crate::Registration = crate::Registration {
    adapter: &Codex,
    quota_source: Some(&Codex),
    stdout_decoder: Some(&LiveUsage),
    stderr_decoder: None,
    display_name: "Codex",
    quota: provenant_quota::QuotaPolicy {
        adapter: "codex",
        timer_seconds: 900,
        panel_seconds: 300,
        probe_seconds: 10,
        low_power: true,
        solo: false,
    },
};

crate::adapter!(Codex, "codex", prepare);

/// Completed-turn usage; this cannot stop a turn mid-flight. `codex exec --json`
/// reports each turn's usage in its `turn.completed` event; `input_tokens` holds cached
/// reads and writes, so cache reads (`cached_input_tokens`) are subtracted to
/// count fresh tokens only. Every turn is a distinct call, so each gets its
/// own key and they add.
pub struct LiveUsage;
static TURNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
impl provenant_os::capture::FrameDecoder for LiveUsage {
    fn decode(
        &self,
        frame: &[u8],
    ) -> Result<provenant_os::capture::FrameEvent, provenant_os::capture::DecodeFailure> {
        use provenant_os::capture::FrameEvent;
        Ok(
            live_usage(frame).map_or(FrameEvent::Progress, |tokens| FrameEvent::Usage {
                key: TURNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                tokens,
            }),
        )
    }
    fn reports_usage(&self) -> bool {
        true
    }
}
/// The fresh token total of a `turn.completed` frame, or `None` for any other frame
/// or one that does not parse. Cache reads are subtracted so only fresh tokens count.
/// A bad frame never ends an attempt.
fn live_usage(frame: &[u8]) -> Option<u64> {
    if !frame.windows(14).any(|window| window == b"turn.completed") {
        return None;
    }
    match serde_json::from_slice::<Event>(frame).ok()? {
        Event::Completed { usage } => {
            let fresh_input = usage
                .input_tokens
                .saturating_sub(usage.cached_input_tokens.unwrap_or(0));
            Some(fresh_input.saturating_add(usage.output_tokens))
        }
        _ => None,
    }
}
/// The exec JSON stream carries no answering-model position.
const MODEL_ON_WIRE: bool = false;

/// Codex runs with a private `CODEX_HOME` (the attempt's private state) that
/// links only `auth.json` from the selected registered identity (the default
/// login source when no account was selected). Its own seatbelt cannot nest
/// inside ours, so it runs unsandboxed inside the outer profile, which is the
/// real boundary. Its shell tool may run system and install-prefix programs.
fn prepare(
    input: &PrepareInput<'_>,
    access: Access,
    session: Session,
    stream: StreamState,
) -> Result<LaunchSpec, ProviderError> {
    let plan = input.confined.permissions();
    let mut tool_requirements = crate::launch::ToolRequirements::default();
    if input.confined.codex_policy() != CodexPolicy::Clean
        || plan.selected_authentication.as_deref() != Some("login")
    {
        return Err(ProviderError::UnsupportedPolicy);
    }
    let codex_home = &plan
        .private_state
        .as_ref()
        .ok_or(ProviderError::UnsupportedPolicy)?
        .path;
    let credential = input.account.map_or_else(
        || input.home.join(".codex/auth.json"),
        |account| account.directory().join("auth.json"),
    );
    let mut argv: Vec<OsString> = vec![input.executable.into(), "exec".into()];
    if session == Session::Resume {
        let continuation = input.continuation.ok_or(ProviderError::UnknownIdentity)?;
        argv.extend(["resume".into(), continuation.session().into()]);
    }
    argv.extend(
        [
            "--json",
            "--skip-git-repo-check",
            "--ignore-user-config",
            "--ignore-rules",
            "--dangerously-bypass-approvals-and-sandbox",
            "-c",
            "service_tier=\"default\"",
            "-m",
            input.route.model.as_str(),
        ]
        .map(OsString::from),
    );
    if let Some(effort) = &input.route.effort {
        argv.extend([
            "-c".into(),
            format!("model_reasoning_effort=\"{effort}\"").into(),
        ]);
    }
    for image in input.images {
        argv.extend(["--image".into(), image.as_os_str().to_owned()]);
    }
    input.provenant.codex(&mut argv)?;
    argv.push("-".into());

    // Codex runs helpers from its own package (code-mode host, bundled `rg`
    // and `zsh`), and its shell tool runs system programs. System directories
    // are readable through the runtime baseline; exec roots add exec, not
    // reads. No shared install prefix: its var/ stays unreadable.
    let mut package_omissions = Vec::new();
    let package = package_grant(input.executable, &mut package_omissions);
    let tool_exec_roots = vec![
        subtree("/bin"),
        subtree("/usr/bin"),
        subtree("/usr/libexec"),
        package.clone(),
    ];
    let mut runtime_reads = vec![package.clone()];
    let tool_writes = Vec::new();
    let staged_skills = input.stage_user_skills();
    let omitted_discovery = package_omissions;
    let mut setup = vec![SetupStep::Symlink {
        link: codex_home.join("auth.json"),
        target: credential.clone(),
    }];
    if let Some(skills) = &staged_skills.root {
        runtime_reads.push(subtree(skills));
        setup.push(SetupStep::CreateDir(codex_home.join(".agents")));
        setup.push(SetupStep::Symlink {
            link: codex_home.join(".agents/skills"),
            target: skills.clone(),
        });
    }
    let path = crate::launch::tool_search_path(input.home, true);
    let mut env = base_env(input, &plan.scratch.path, path);
    input
        .provenant
        .environment(&plan.scratch.path, &mut env, &mut setup, false)?;
    crate::launch::rust_tools(input, access, &mut tool_requirements, &mut env, &mut setup);
    crate::launch::node_tools(
        input.home,
        access,
        crate::launch::lane_cache(input),
        &mut tool_requirements,
        &mut env,
    );
    // Git may inspect global config even for status. Keep host hooks,
    // includes and credential helpers outside the lane's environment.
    env.extend([
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]);
    if staged_skills.root.is_some() || !input.user_skills() {
        for (key, value) in &mut env {
            if key == "HOME" {
                *value = codex_home.into();
            }
        }
    }
    env.push(("CODEX_HOME".into(), codex_home.into()));
    env.push(("SHELL".into(), "/bin/zsh".into()));
    let mut spec = LaunchSpec {
        argv,
        env,
        stdin: Stdin::Bytes(input.prompt.text().as_bytes().to_vec()),
        secret_input: None,
        cwd: input.cwd.to_owned(),
        setup,
        omitted_discovery,
        user_skills: staged_skills,
        tool_requirements,
        git_wrapper: Default::default(),
        profile: ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![exact(input.executable)],
                allow_process_fork: true,
                runtime_reads,
                authentication: vec![AuthenticationProfile {
                    keychain: None,
                    mechanism: "login".into(),
                    roots: vec![exact(&credential)],
                    // Token refresh rewrites auth.json through the link.
                    in_place_writes: vec![exact(&credential)],
                }],
                tool_exec_roots,
                tool_writes,
                provider_api: vec![NetworkEndpoint {
                    address: "*".into(),
                    port: 443,
                }],
                ..ProviderProfileSpec::default()
            },
            observations: Default::default(),
        },
        stream,
    };
    crate::launch::prepare_tool_requirements(input, &mut spec);
    input
        .provenant
        .grant(&mut spec.profile.spec, &mut spec.setup, false);
    Ok(spec)
}

crate::quota_source!(Codex, "codex");
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "thread.started")]
    Thread { thread_id: String },
    #[serde(rename = "turn.started")]
    Started,
    #[serde(rename = "item.completed")]
    Item { item: Item },
    #[serde(rename = "turn.completed")]
    Completed { usage: Usage },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "turn.failed")]
    Failed { error: Option<serde_json::Value> },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Item {
    #[serde(rename = "agent_message")]
    Answer { text: String },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Usage {
    /// Everything the model read, including cached and newly cached tokens.
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: Option<u64>,
    #[serde(default)]
    cache_write_input_tokens: Option<u64>,
    output_tokens: u64,
}
/// The host's wall time in its own zone, which Codex uses for a named reset.
fn local_now() -> Option<jiff::Zoned> {
    use provenant_core::Clock;
    let now = provenant_os::clock::SystemClock::new().unix_seconds();
    Some(
        jiff::Timestamp::from_second(now)
            .ok()?
            .to_zoned(jiff::tz::TimeZone::system()),
    )
}

/// Codex's spent plan allowance, read only for the limit and its reset.
/// `You've hit your usage limit.` spends the whole account; a limit that
/// names a model (`usage limit for <model>`) holds that model only.
fn usage_limit(message: &str, now: Option<&jiff::Zoned>) -> Option<E> {
    let lower = message.to_ascii_lowercase();
    let at = lower.find("hit your usage limit")?;
    let account_wide = !lower[at..].starts_with("hit your usage limit for ");
    Some(E::QuotaLimited {
        message: message.to_owned(),
        pool: account_wide.then(|| provenant_quota::ACCOUNT_ROUTE.to_owned()),
        resets_in_seconds: now.and_then(|now| usage_limit_reset(&lower, now)),
    })
}

/// Seconds until the reset Codex names: `try again at 3:05 PM` (today),
/// `try again at Nov 14th, 2026 3:05 PM`, both in local time, or
/// `try again in 2 days 3 hours 5 minutes`. Anything else names no reset.
fn usage_limit_reset(lower: &str, now: &jiff::Zoned) -> Option<u64> {
    let clock = |time: &str, meridiem: &str| -> Option<(i8, i8)> {
        let (hour, minute) = time.split_once(':')?;
        let (hour, minute) = (hour.parse::<i8>().ok()?, minute.parse::<i8>().ok()?);
        if !(1..=12).contains(&hour) || !(0..60).contains(&minute) {
            return None;
        }
        match meridiem {
            "am" => Some((hour % 12, minute)),
            "pm" => Some((hour % 12 + 12, minute)),
            _ => None,
        }
    };
    let seconds = if let Some((_, rest)) = lower.split_once("try again at ") {
        let rest = rest
            .split(['\n', '('])
            .next()?
            .trim_end()
            .trim_end_matches('.');
        let words: Vec<&str> = rest.split_whitespace().collect();
        let date = match words.as_slice() {
            [time, meridiem] => {
                let (hour, minute) = clock(time, meridiem)?;
                now.date().at(hour, minute, 0, 0)
            }
            [month, day, year, time, meridiem] => {
                const MONTHS: [&str; 12] = [
                    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov",
                    "dec",
                ];
                let month = MONTHS.iter().position(|name| month.starts_with(name))? + 1;
                let day = day
                    .trim_end_matches(',')
                    .trim_end_matches(|c: char| c.is_ascii_alphabetic())
                    .parse::<i8>()
                    .ok()?;
                let (hour, minute) = clock(time, meridiem)?;
                jiff::civil::Date::new(year.parse().ok()?, i8::try_from(month).ok()?, day)
                    .ok()?
                    .at(hour, minute, 0, 0)
            }
            _ => return None,
        };
        let reset = date.to_zoned(now.time_zone().clone()).ok()?;
        reset.timestamp().as_second() - now.timestamp().as_second()
    } else {
        let (_, rest) = lower.split_once("try again in ")?;
        let rest = rest
            .split(['\n', '('])
            .next()?
            .trim_end()
            .trim_end_matches('.');
        let words: Vec<&str> = rest.split_whitespace().collect();
        let mut total = 0_i64;
        for pair in words.chunks(2) {
            let [count, unit] = pair else { return None };
            let count = count.parse::<i64>().ok()?;
            let unit = match unit.trim_end_matches(',').trim_end_matches('s') {
                "day" => 86_400,
                "hour" => 3_600,
                "minute" => 60,
                "second" => 1,
                _ => return None,
            };
            total = total.checked_add(count.checked_mul(unit)?)?;
        }
        total
    };
    u64::try_from(seconds).ok().filter(|seconds| *seconds > 0)
}

/// The provider's own refusal of the requested model, in its words.
fn model_refusal(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("model")
        && [
            "not supported",
            "unsupported model",
            "does not exist",
            "not found",
            "unknown model",
            "no access",
        ]
        .iter()
        .any(|phrase| lower.contains(phrase))
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        Event::Thread { thread_id } => vec![E::Session(thread_id)],
        Event::Item {
            item: Item::Answer { text },
        } => vec![E::MessageStart, E::Text(text)],
        Event::Completed { usage } => vec![
            E::Usage {
                // Codex counts cache reads and writes inside `input_tokens`;
                // the recorded categories are disjoint.
                input: usage.input_tokens.saturating_sub(
                    usage
                        .cached_input_tokens
                        .unwrap_or(0)
                        .saturating_add(usage.cache_write_input_tokens.unwrap_or(0)),
                ),
                output: usage.output_tokens,
                cache_read: usage.cached_input_tokens,
                cache_write: usage.cache_write_input_tokens,
                cache_write_1h: None,
            },
            E::Terminal,
        ],
        Event::Error { message } => {
            let lower = message.to_ascii_lowercase();
            vec![if lower.contains("reconnecting") {
                E::TransientError
            } else if model_refusal(&message) {
                E::ModelUnavailable { message }
            } else if let Some(limited) = usage_limit(&message, local_now().as_ref()) {
                limited
            } else if lower.contains("too many requests") || lower.contains("status: 429") {
                E::RateLimited { message }
            } else {
                E::Error
            }]
        }
        Event::Failed { error } => {
            let typed = error.as_ref().and_then(|error| {
                let refused = error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .filter(|message| model_refusal(message))
                    .map(|message| E::ModelUnavailable {
                        message: message.to_owned(),
                    });
                let message = || {
                    error
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                };
                if refused.is_some() {
                    return refused;
                }
                if let Some(limited) =
                    message().and_then(|message| usage_limit(&message, local_now().as_ref()))
                {
                    return Some(limited);
                }
                match error.get("codex_error_info")?.as_str()? {
                    // Without the capacity message the failure stays generic.
                    "server_overloaded" => message().map(|message| E::ModelOverloaded { message }),
                    // The account's plan allowance is spent; another account
                    // may still serve the lane.
                    "usage_limit_exceeded" => Some(E::QuotaLimited {
                        message: message().unwrap_or_else(|| "provider usage limit reached".into()),
                        pool: None,
                        resets_in_seconds: None,
                    }),
                    _ => None,
                }
            });
            vec![typed.unwrap_or(E::Error)]
        }
        _ => vec![],
    })
}

#[cfg(test)]
mod live_usage_tests {
    use super::*;
    use provenant_os::capture::{FrameDecoder, FrameEvent};

    #[test]
    fn a_completed_turn_reports_its_fresh_input_and_output() {
        let turn = br#"{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}"#;
        let FrameEvent::Usage { tokens, key } = LiveUsage.decode(turn).unwrap() else {
            panic!("a completed turn reports usage");
        };
        // 1000 (total input) - 800 (cached) + 50 (output) = 250 fresh tokens.
        assert_eq!(tokens, 250);
        // Each turn is its own call, so two identical turns both count.
        let FrameEvent::Usage { key: next, .. } = LiveUsage.decode(turn).unwrap() else {
            panic!("a completed turn reports usage");
        };
        assert_ne!(key, next);
        for other in [
            &br#"{"type":"turn.started"}"#[..],
            br#"{"type":"item.started","item":{"id":"item_0","type":"command_execution","command":"pwd","status":"in_progress"}}"#,
            br#"{"type":"item.completed","item":{"id":"item_0","type":"command_execution","command":"pwd","aggregated_output":"/tmp\n","exit_code":0,"status":"completed"}}"#,
            br#"{"type":"item.completed","item":{"type":"agent_message","text":"turn.completed"}}"#,
            b"turn.completed but not json",
        ] {
            assert_eq!(LiveUsage.decode(other).unwrap(), FrameEvent::Progress);
        }
        assert!(LiveUsage.reports_usage());
    }
}

#[cfg(test)]
mod usage_limit_tests {
    use super::*;

    fn at(text: &str) -> jiff::Zoned {
        text.parse().expect("zoned time")
    }

    #[test]
    fn a_plan_usage_limit_holds_the_account_until_its_named_reset() {
        let now = at("2026-11-10T12:00:00+00:00[UTC]");
        let plan = "You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at Nov 14th, 2026 3:05 PM.";
        let Some(E::QuotaLimited {
            pool,
            resets_in_seconds,
            ..
        }) = usage_limit(plan, Some(&now))
        else {
            panic!("a usage limit is typed");
        };
        assert_eq!(pool.as_deref(), Some(provenant_quota::ACCOUNT_ROUTE));
        assert_eq!(resets_in_seconds, Some(4 * 86_400 + 3 * 3_600 + 300));
        for (text, reset) in [
            (
                "You've hit your usage limit. Try again at 3:05 PM.",
                Some(11_100),
            ),
            ("You've hit your usage limit. Try again at 12:30 AM.", None),
            (
                "You've hit your usage limit. Try again in 2 days 3 hours.",
                Some(183_600),
            ),
            ("You've hit your usage limit. Try again later.", None),
            ("You've hit your usage limit. Try again at teatime.", None),
        ] {
            assert!(
                matches!(
                    usage_limit(text, Some(&now)),
                    Some(E::QuotaLimited { resets_in_seconds, .. }) if resets_in_seconds == reset
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn a_model_usage_limit_holds_only_that_model() {
        let now = at("2026-11-10T12:00:00+00:00[UTC]");
        let text = "You've hit your usage limit for GPT-5.3-Codex-Spark. Switch to another model now, or try again at 3:05 PM.";
        assert!(matches!(
            usage_limit(text, Some(&now)),
            Some(E::QuotaLimited {
                pool: None,
                resets_in_seconds: Some(11_100),
                ..
            })
        ));
        assert!(usage_limit("stream disconnected before completion", Some(&now)).is_none());
    }
}

#[cfg(test)]
mod model_refusal_tests {
    use super::*;

    #[test]
    fn a_provider_model_refusal_is_a_typed_cause() {
        let message = "The 'gpt-9' model is not supported when using Codex with a ChatGPT account.";
        for line in [
            serde_json::json!({"type": "error", "message": message}),
            serde_json::json!({"type": "turn.failed", "error": {"message": message}}),
        ] {
            let events = decode(line.to_string().as_bytes()).expect("decoded");
            assert!(
                matches!(events.as_slice(), [E::ModelUnavailable { message: m }] if m == message),
                "{events:?}"
            );
        }
        let other = serde_json::json!({"type": "error", "message": "stream disconnected"});
        let events = decode(other.to_string().as_bytes()).expect("decoded");
        assert!(matches!(events.as_slice(), [E::Error]));
    }
}
