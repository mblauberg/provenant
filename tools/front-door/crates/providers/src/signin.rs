//! Non-billable sign-in probes. Only a status bit leaves this module.
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, Termination};
#[cfg(test)]
use provenant_records::Harness;
use provenant_records::{AdapterSpec, SigninFormat};
use std::{ffi::OsString, path::Path, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigninState {
    SignedIn,
    SignedOut,
    Unknown,
}

/// Status commands never submit a model prompt or expose their captures.
pub fn probe(
    executor: &dyn Executor,
    spec: &AdapterSpec,
    executable: &Path,
    home: &Path,
    directory: Option<&Path>,
) -> SigninState {
    if spec.signin_args.is_empty() {
        return SigninState::Unknown;
    }
    let args = spec.signin_args;
    let mut argv = vec![executable.as_os_str().to_owned()];
    argv.extend(args.iter().map(|arg| OsString::from(*arg)));
    let mut env: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), home.into()),
        ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
        ("NO_COLOR".into(), "1".into()),
    ];
    for name in ["XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
        if let Some(value) = std::env::var_os(name) {
            env.push((name.into(), value));
        }
    }
    if let Some(user) = std::env::var_os("USER") {
        env.push(("USER".into(), user));
    }
    if let Some(directory) = directory {
        if let Some(selector) = spec.signin_config_env {
            env.push((selector.into(), directory.into()));
        }
        if spec.signin_account_home {
            env[0].1 = directory.into();
        }
    } else if let Some(selector) = spec.signin_config_env
        && let Some(value) = std::env::var_os(selector)
    {
        env.push((selector.into(), value));
    }
    let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(4));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stderr = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stop_on_output_limit = true;
    let Ok(out) = executor.execute(options) else {
        return SigninState::Unknown;
    };
    if out.timed_out
        || out.drain_truncated
        || out.overflow_stopped
        || out.stdout.truncated
        || out.stdout.read_failed
        || out.stderr.truncated
        || out.stderr.read_failed
    {
        return SigninState::Unknown;
    }
    let mut capture = out.stdout.bytes;
    capture.push(b'\n');
    capture.extend(out.stderr.bytes);
    classify(spec, &capture, out.termination == Termination::Exit(0))
}

fn classify(spec: &AdapterSpec, capture: &[u8], success: bool) -> SigninState {
    let text = String::from_utf8_lossy(capture).to_ascii_lowercase();
    if text.contains("not logged in")
        || spec.auth_markers.iter().any(|marker| text.contains(marker))
    {
        return SigninState::SignedOut;
    }
    match spec.signin_format {
        SigninFormat::LoggedInJson => {
            // stderr is separated by the newline appended by `probe`.
            let status = serde_json::Deserializer::from_slice(capture)
                .into_iter::<serde_json::Value>()
                .next()
                .and_then(Result::ok);
            match status.and_then(|value| value.get("loggedIn").and_then(|v| v.as_bool())) {
                Some(true) if success => SigninState::SignedIn,
                Some(false) => SigninState::SignedOut,
                _ => SigninState::Unknown,
            }
        }
        SigninFormat::StatusJson => {
            let signed_in = serde_json::Deserializer::from_slice(capture)
                .into_iter::<serde_json::Value>()
                .filter_map(Result::ok)
                .any(|value| {
                    value
                        .get("status")
                        .or_else(|| value.pointer("/payload/status"))
                        .and_then(|v| v.as_str())
                        == Some("SUCCESS")
                });
            if success && signed_in {
                SigninState::SignedIn
            } else {
                SigninState::Unknown
            }
        }
        SigninFormat::LoggedInText if success && text.contains("logged in") => {
            SigninState::SignedIn
        }
        SigninFormat::CredentialCount if success => {
            // `auth list` prints a count, never the credentials themselves.
            let mut counts = text
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .filter_map(|words| {
                    words[1]
                        .starts_with("credential")
                        .then(|| words[0].parse::<usize>().ok())
                        .flatten()
                })
                .collect::<Vec<_>>();
            counts.sort_unstable();
            match counts.last() {
                Some(0) => SigninState::SignedOut,
                Some(_) => SigninState::SignedIn,
                None => SigninState::Unknown,
            }
        }
        _ => SigninState::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_signin_fixtures_for_agy_and_opencode() {
        for (adapter, capture, expected) in [
            (
                Harness::Agy,
                "{\"status\":\"SUCCESS\",\"usage\":{}}",
                SigninState::SignedIn,
            ),
            (
                Harness::Agy,
                include_str!("../../../tests/fixtures/providers/agy/auth-required.stderr"),
                SigninState::SignedOut,
            ),
            (
                Harness::Opencode,
                "┌ Credentials\n│ ● OpenCode API key\n└ 1 credentials",
                SigninState::SignedIn,
            ),
            (
                Harness::Opencode,
                "┌ Credentials\n└ 0 credentials",
                SigninState::SignedOut,
            ),
        ] {
            assert_eq!(classify(adapter.spec(), capture.as_bytes(), true), expected);
        }
    }

    #[test]
    fn doctor_agy_signin_uses_recorded_usage_payload_and_jsonl_frames() {
        let fixture: serde_json::Value =
            serde_json::from_slice(include_bytes!("quota/fixtures/agy.json")).unwrap();
        let raw = serde_json::to_vec(&fixture["payload"]).unwrap();
        assert_eq!(
            classify(Harness::Agy.spec(), &raw, true),
            SigninState::SignedIn
        );
        assert_eq!(
            classify(
                Harness::Agy.spec(),
                include_bytes!("quota/fixtures/agy.json"),
                true
            ),
            SigninState::SignedIn
        );
        let mut frames = b"{\"type\":\"start\"}\n".to_vec();
        frames.extend(raw);
        assert_eq!(
            classify(Harness::Agy.spec(), &frames, true),
            SigninState::SignedIn
        );
    }

    #[test]
    fn doctor_signin_failures_and_unknown_output_stay_unknown() {
        assert_eq!(
            classify(Harness::Opencode.spec(), b"1 credentials", false),
            SigninState::Unknown
        );
        assert_eq!(
            classify(Harness::Agy.spec(), b"{\"status\":\"ERROR\"}", true),
            SigninState::Unknown
        );
        assert_eq!(
            classify(Harness::Codex.spec(), b"Logged in using ChatGPT", true),
            SigninState::SignedIn
        );
        assert_eq!(
            classify(Harness::Cursor.spec(), b"Not logged in", true),
            SigninState::SignedOut
        );
    }
}
