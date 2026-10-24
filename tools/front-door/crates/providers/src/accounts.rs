//! Durable non-secret registrations and host identity checks.
use crate::{ProviderError, identity::RegisteredAccount};
use provenant_config::{DomainConfig, DomainSection, Profile, SectionError};
use provenant_supervisor::{
    exec::{ExecOptions, Executor, StreamPolicy, Termination},
    fs::{ReadOptions, read_following},
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Duration,
};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub entries: std::collections::BTreeMap<String, Registration>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub adapter: String,
    pub label: String,
    pub key: String,
    pub directory: PathBuf,
}
impl Registration {
    pub fn registered(&self) -> Result<RegisteredAccount, ProviderError> {
        RegisteredAccount::new(&self.adapter, &self.key, self.directory.clone())
    }
}
impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        let bad =
            self.entries.len() > 128
                || self.entries.values().enumerate().any(|(i, e)| {
                    e.registered().is_err()
                        || !label(&e.label)
                        || self.entries.values().take(i).any(|a| {
                            a.adapter == e.adapter && (a.label == e.label || a.key == e.key)
                        })
                });
        if bad {
            vec![SectionError {
                field: "entries".into(),
                code: "registration",
            }]
        } else {
            Vec::new()
        }
    }
}
pub fn label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn defaults(_: Profile) -> &'static str {
    "[entries]\n"
}
pub const SECTION: DomainSection = DomainSection::new::<Settings>("accounts", defaults);

/// Status output is projected immediately to its fingerprint; email is never retained.
pub fn detect(
    executor: &dyn Executor,
    adapter: &str,
    executable: &Path,
    home: &Path,
    directory: &Path,
    cwd: &Path,
) -> Result<String, ProviderError> {
    if adapter == "codex" {
        let bytes = read_following(
            &directory.join("auth.json"),
            ReadOptions { limit: 1024 * 1024 },
        )
        .map_err(|_| ProviderError::UnknownIdentity)?;
        return crate::identity::codex_account(&bytes).ok_or(ProviderError::UnknownIdentity);
    }
    if adapter != "claude" || directory == home.join(".claude") {
        return Err(ProviderError::UnknownIdentity);
    }
    let argv: Vec<OsString> = [
        executable.as_os_str(),
        OsStr::new("auth"),
        OsStr::new("status"),
        OsStr::new("--json"),
    ]
    .map(Into::into)
    .to_vec();
    let env = [
        ("HOME".into(), home.into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("CLAUDE_CONFIG_DIR".into(), directory.into()),
    ];
    let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(4));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 65536 };
    options.stderr = StreamPolicy::Discard;
    options.stop_on_output_limit = true;
    let out = executor
        .execute(options)
        .map_err(|_| ProviderError::UnknownIdentity)?;
    if out.termination != Termination::Exit(0)
        || out.stdout.truncated
        || out.stdout.read_failed
        || out.timed_out
        || out.drain_truncated
    {
        return Err(ProviderError::UnknownIdentity);
    }
    #[derive(Deserialize)]
    struct Status {
        #[serde(rename = "loggedIn")]
        logged_in: bool,
        email: String,
    }
    let status: Status =
        serde_json::from_slice(&out.stdout.bytes).map_err(|_| ProviderError::UnknownIdentity)?;
    if !status.logged_in {
        return Err(ProviderError::UnknownIdentity);
    }
    crate::identity::fingerprint(&status.email).ok_or(ProviderError::UnknownIdentity)
}
