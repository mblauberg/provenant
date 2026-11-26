//! `declare` — declare a lane's result to the host-bound journal.
//!
//! The command is a pure edge: it reads the declaration bytes, binds the
//! parent attempt from the consumer environment, and relays one typed call.
//! It never opens the store and never reads the attempt from the payload.
//! Host and project settings bound the journal, the relay and the near-full
//! warning; an unreadable configuration warns and uses the shipped defaults.
use crate::metadata::CatalogueArg;
use clap::Args;
use provenant_config::{Origin, ResolvedConfig};
use provenant_core::{
    AttemptId, CancelToken, Clock as _, ProjectId, RootInputs, STATE_DIRECTORY_ENV,
};
use provenant_fabric_runtime::relay::{ServiceError, ToolCall};
use provenant_records::{
    CanonicalV1, Layer,
    declaration::{DeclarationError, DeclarationInput, DeclarationLimit, DeclarationReceipt},
    policy::{Effect, Facts, Policy},
    warning::{CONFIG_UNAVAILABLE, Warning, warn},
};
use serde_json::Value;
use std::{io::Read, path::PathBuf, time::Duration};

/// Declare this lane's result, from a file or stdin.
#[derive(Clone, Debug, Args)]
pub struct DeclareArgs {
    /// Read the schema v2 handoff JSON from a file
    #[arg(long, catalog_required_unless = "stdin", conflicts_with = "stdin")]
    pub file: Option<PathBuf>,
    /// Read the schema v2 handoff JSON from standard input
    #[arg(long, catalog_required_unless = "file", conflicts_with = "file")]
    pub stdin: bool,
}

/// The effective declaration-channel settings and the layer that set each.
/// The journal bound is enforced by the host; the relay timeout, retries and
/// near-full policy are applied by the lane CLI. Both sides resolve the same
/// `domains.run` fields from the same host-owned configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct DeclarationSettings {
    pub journal_bytes: u64,
    pub journal_key: String,
    pub journal_source: String,
    pub relay_seconds: u64,
    pub retries: u8,
    pub near_full_policy: Policy,
}

impl DeclarationSettings {
    /// The shipped values: a 1 MiB journal, a 10 second relay, one retry and
    /// the compiled near-full warning rule.
    pub fn shipped() -> Self {
        Self {
            journal_bytes: provenant_records::declaration::DEFAULT_DECLARATION_JOURNAL_BYTES,
            journal_key: "domains.run.declaration_journal_bytes".into(),
            journal_source: "compiled defaults".into(),
            relay_seconds: 10,
            retries: 1,
            near_full_policy: Policy::default(),
        }
    }

    /// The bound the host journal enforces, with its key and source.
    pub fn limit(&self) -> DeclarationLimit {
        DeclarationLimit {
            journal_bytes: self.journal_bytes,
            key: self.journal_key.clone(),
            source: self.journal_source.clone(),
        }
    }

    /// Below one tenth of the configured bound, a successful call warns.
    pub fn near_full(&self, remaining: u64) -> bool {
        self.journal_bytes > 0 && remaining < self.journal_bytes / 10
    }

    /// One initial call plus the configured retries. Zero retries makes one
    /// attempt and never replays a failed relay.
    pub fn relay_attempts(&self) -> u8 {
        self.retries.saturating_add(1)
    }

    /// Resolve host and project layers independently for each field. A project
    /// setting wins over the host setting; an unset field keeps the compiled
    /// value and reports its source as `compiled defaults`.
    pub fn resolve(
        config: &ResolvedConfig<crate::domains::Domains>,
        project: &str,
        state: &std::path::Path,
    ) -> Self {
        let run = &config.domains.run;
        let (journal_bytes, journal_used) = run.declaration_journal_bytes_for(project);
        let journal_field = if journal_used == Layer::Project {
            format!("domains.run.projects.{project}.declaration_journal_bytes")
        } else {
            "domains.run.declaration_journal_bytes".into()
        };
        let journal_source = setting_source(config, &journal_field);
        let (relay_seconds, _) = run.declaration_relay_seconds_for(project);
        let (retries, _) = run.declaration_relay_retries_for(project);
        Self {
            journal_bytes,
            journal_key: journal_field,
            journal_source,
            relay_seconds,
            retries,
            near_full_policy: provenant_config::policy::read(
                state,
                project,
                &std::collections::BTreeMap::new(),
            ),
        }
    }
}

fn setting_source(config: &ResolvedConfig<crate::domains::Domains>, field: &str) -> String {
    match config.origins.get(field) {
        Some(Origin::User { .. }) => format!("host (config.toml#{field})"),
        Some(Origin::Compiled { .. }) | None => "compiled defaults".into(),
    }
}

/// Build the host-owned roots the lane can see, read `domains.run`, and resolve
/// every channel setting. A missing or unreadable configuration is advisory:
/// it warns with its source and keeps the shipped values.
fn lane_settings(globals: &crate::Globals) -> DeclarationSettings {
    let resolved = (|| -> Result<DeclarationSettings, Box<dyn std::error::Error>> {
        let home = std::env::var_os("HOME").ok_or("HOME is unset")?;
        let cwd = std::env::current_dir()?;
        let mut inputs = RootInputs::new(PathBuf::from(home), cwd.clone());
        inputs.state_dir = globals.state_dir.clone();
        inputs.environment_state_dir = std::env::var_os(STATE_DIRECTORY_ENV).map(PathBuf::from);
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let config = crate::config_reads::read_for(&roots, "declare")?;
        let project = std::env::var(provenant_core::consumer_env::ADMISSION_PROJECT)
            .ok()
            .and_then(|path| provenant_config::project_key_for(std::path::Path::new(&path)))
            .and_then(|key| ProjectId::parse(&key).ok())
            .map(|id| id.to_string())
            .unwrap_or_default();
        Ok(DeclarationSettings::resolve(
            &config,
            &project,
            roots.state.as_path(),
        ))
    })();
    match resolved {
        Ok(settings) => settings,
        Err(error) => {
            warn(
                Warning::new(
                    CONFIG_UNAVAILABLE,
                    format!("declaration settings unavailable ({error}); using compiled defaults"),
                )
                .subject("domains.run.declaration_*")
                .once_per_change(),
            );
            DeclarationSettings::shipped()
        }
    }
}

fn error() -> ServiceError {
    ServiceError::Cli {
        code: "declaration_refused".into(),
        message: "the host-bound declaration owner refused the request".into(),
        data: Value::Null,
    }
}

/// Called on the guardian, never with a lane-selected attempt. The attempt is
/// the one bound to this socket's holder, so a payload cannot name another.
/// The bound is resolved by the host from its own configuration, never by the
/// lane that submitted the declaration.
pub fn serve_lane(
    ctx: &crate::fabric::Context,
    holder: &provenant_run::HolderContext,
    input: DeclarationInput,
    limit: &DeclarationLimit,
) -> Result<Value, ServiceError> {
    let roots = ctx
        .roots
        .with_state_dir(
            holder.roots.state_root.clone(),
            &provenant_os::fs::SystemPathResolver,
        )
        .map_err(|_| error())?;
    let context = provenant_run::ExecutionContext {
        state_root: roots.state,
        project_id: holder.project_id,
        actor_id: holder.actor_id,
        read_prompts: false,
    };
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = provenant_run::RunStore::open_read_only(&context, &call)
        .map_err(|_| error())?
        .ok_or_else(error)?;
    match store.declare(holder.attempt_id, &input, limit) {
        Ok(receipt) => serde_json::to_value(receipt).map_err(|_| error()),
        Err(DeclarationError::Invalid(invalid)) => Err(ServiceError::Cli {
            code: "declaration_invalid".into(),
            message: invalid.to_string(),
            data: serde_json::to_value(&invalid).unwrap_or(Value::Null),
        }),
        Err(failure) => Err(ServiceError::Cli {
            code: "declaration_refused".into(),
            message: failure.to_string(),
            // The Full refusal carries its key and source in the message so
            // the lane is told exactly which setting to raise.
            data: Value::Null,
        }),
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "stdin is an explicit real-time boundary"
)]
fn read_input(command: &DeclareArgs) -> Result<Vec<u8>, String> {
    let limit = provenant_records::handoff::HANDOFF_BYTES;
    if let Some(path) = &command.file {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|error| format!("declaration file: {error}"))?
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("declaration file: {error}"))?;
        if bytes.len() > limit {
            return Err(format!("declaration file exceeds {limit} bytes"));
        }
        Ok(bytes)
    } else if command.stdin {
        provenant_os::sys::hook_io::read_stdin(
            limit,
            std::time::Instant::now() + Duration::from_secs(5),
        )
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::FileTooLarge {
                format!("declaration exceeds {limit} bytes")
            } else {
                format!("declaration stdin: {error}")
            }
        })
    } else {
        Err("declare requires exactly one of --file or --stdin".into())
    }
}

fn transport(error: &ServiceError) -> bool {
    matches!(
        error,
        ServiceError::FabricOwnerUnavailable | ServiceError::Facade(_) | ServiceError::Time(_)
    )
}

fn print_invalid(message: &str, data: &Value) {
    let Some(items) = data.get("dropped").and_then(Value::as_array) else {
        eprintln!("{message}");
        return;
    };
    for item in items {
        let field = item.get("field").and_then(Value::as_str).unwrap_or("$");
        let detail = item.get("error").and_then(Value::as_str).unwrap_or("");
        eprintln!("declaration field {field}: {detail}");
    }
    if items.is_empty() {
        eprintln!("{message}");
    }
}

/// Process edge: one declaration, one receipt, no prose fallback.
pub fn run(command: DeclareArgs, globals: &crate::Globals) -> u8 {
    let Some(parent) = std::env::var(provenant_core::consumer_env::PARENT_ATTEMPT_ID).ok() else {
        eprintln!("{}", DeclarationError::NotInLane);
        return 2;
    };
    let attempt = match AttemptId::parse(&parent) {
        Ok(attempt) => attempt,
        Err(error) => {
            eprintln!("invalid parent attempt id: {error}");
            return 2;
        }
    };
    let bytes = match read_input(&command) {
        Ok(bytes) => bytes,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let payload = match serde_json::from_slice::<Value>(&bytes) {
        Ok(payload) => payload,
        Err(error) => {
            eprintln!("declaration field $: invalid declaration JSON: {error}");
            return 1;
        }
    };
    let digest = CanonicalV1::file_digest(&bytes);
    let clock = provenant_os::clock::SystemClock::new();
    let started = clock
        .wall_now()
        .map(|time| time.as_micros())
        .unwrap_or_default();
    let input = DeclarationInput {
        request_id: format!("{}-{started}", provenant_core::hex(digest.bytes())),
        payload,
    };
    let settings = lane_settings(globals);
    let call = ToolCall::Declare(input);
    let socket = provenant_providers::LaneRuntime::fabric_socket(attempt);
    let cancel = CancelToken::new();
    let timeout = Duration::from_secs(settings.relay_seconds);
    let mut reply = call.relay_within(&socket, &cancel, timeout);
    // Every retry reuses the same request id, so a replay is idempotent.
    for _ in 1..settings.relay_attempts() {
        if !reply.as_ref().is_err_and(|error| transport(error)) {
            break;
        }
        reply = call.relay_within(&socket, &cancel, timeout);
    }
    match reply {
        Ok(value) => match serde_json::from_value::<DeclarationReceipt>(value) {
            Ok(receipt) => {
                if settings.near_full(receipt.remaining_bytes) {
                    let detail = format!(
                        "declaration journal has {} bytes left of {} from {}",
                        receipt.remaining_bytes, settings.journal_bytes, settings.journal_source
                    );
                    if let Effect::Warn(warning) = settings
                        .near_full_policy
                        .check("DECLARATION_JOURNAL_NEAR_FULL", Facts::violation(detail))
                    {
                        warn(warning);
                    }
                }
                match serde_json::to_string(&receipt) {
                    Ok(json) => {
                        println!("{json}");
                        0
                    }
                    Err(error) => {
                        eprintln!("declaration receipt: {error}");
                        1
                    }
                }
            }
            Err(error) => {
                eprintln!("declaration receipt: {error}");
                1
            }
        },
        Err(ServiceError::Cli {
            code,
            message,
            data,
        }) if code == "declaration_invalid" => {
            print_invalid(&message, &data);
            1
        }
        Err(error) => {
            eprintln!("declaration: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::declaration::DeclarationLimit;

    #[test]
    fn shipped_settings_match_the_channel_defaults() {
        let settings = DeclarationSettings::shipped();
        assert_eq!(settings.journal_bytes, 1 << 20);
        assert_eq!(settings.relay_seconds, 10);
        assert_eq!(settings.retries, 1);
        assert_eq!(
            settings.limit().key,
            "domains.run.declaration_journal_bytes"
        );
        assert!(!settings.near_full(1 << 17));
        assert!(settings.near_full((1 << 20) / 10 - 1));
    }

    #[test]
    fn zero_retries_makes_exactly_one_attempt() {
        let mut settings = DeclarationSettings::shipped();
        assert_eq!(settings.relay_attempts(), 2);
        settings.retries = 0;
        assert_eq!(settings.relay_attempts(), 1);
        settings.retries = 5;
        assert_eq!(settings.relay_attempts(), 6);
    }

    #[test]
    fn a_zero_journal_bound_is_never_near_full() {
        let mut settings = DeclarationSettings::shipped();
        settings.journal_bytes = 0;
        assert!(!settings.near_full(0));
    }

    #[test]
    fn a_project_bound_is_named_in_the_limit() {
        let mut settings = DeclarationSettings::shipped();
        settings.journal_bytes = 65_536;
        settings.journal_key = "domains.run.projects.p.declaration_journal_bytes".into();
        settings.journal_source = "host (config.toml)".into();
        let limit: DeclarationLimit = settings.limit();
        assert_eq!(limit.journal_bytes, 65_536);
        assert!(limit.key.contains("projects.p"));
        assert!(limit.source.contains("config.toml"));
        assert!(settings.near_full(6_000));
    }
}
