//! `handoff` — declare a lane's result to the host-bound journal.
//!
//! The command is a pure edge: it reads the declaration bytes, binds the
//! parent attempt from the consumer environment, and relays one typed call.
//! It never opens the store and never reads the attempt from the payload.
use crate::metadata::CatalogueArg;
use clap::Args;
use provenant_core::{AttemptId, CancelToken, Clock as _};
use provenant_fabric_runtime::relay::{ServiceError, ToolCall};
use provenant_records::{
    CanonicalV1,
    declaration::{DeclarationError, DeclarationInput, DeclarationReceipt},
};
use serde_json::Value;
use std::{io::Read, path::PathBuf, time::Duration};

/// Declare the last accepted handoff for this lane, from a file or stdin.
#[derive(Clone, Debug, Args)]
pub struct Handoff {
    /// Read the schema v2 handoff JSON from a file
    #[arg(long, catalog_required_unless = "stdin", conflicts_with = "stdin")]
    pub file: Option<PathBuf>,
    /// Read the schema v2 handoff JSON from standard input
    #[arg(long, catalog_required_unless = "file", conflicts_with = "file")]
    pub stdin: bool,
}

fn error() -> ServiceError {
    ServiceError::Cli {
        code: "handoff_refused".into(),
        message: "the host-bound declaration owner refused the request".into(),
        data: Value::Null,
    }
}

/// Called on the guardian, never with a lane-selected attempt. The attempt is
/// the one bound to this socket's holder, so a payload cannot name another.
pub fn serve_lane(
    ctx: &crate::fabric::Context,
    holder: &provenant_run::HolderContext,
    input: DeclarationInput,
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
    match store.declare(holder.attempt_id, &input) {
        Ok(receipt) => serde_json::to_value(receipt).map_err(|_| error()),
        Err(DeclarationError::Invalid(invalid)) => Err(ServiceError::Cli {
            code: "handoff_invalid".into(),
            message: invalid.to_string(),
            data: serde_json::to_value(&invalid).unwrap_or(Value::Null),
        }),
        Err(failure) => Err(ServiceError::Cli {
            code: "handoff_refused".into(),
            message: failure.to_string(),
            data: Value::Null,
        }),
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "stdin is an explicit real-time boundary"
)]
fn read_input(command: &Handoff) -> Result<Vec<u8>, String> {
    let limit = provenant_records::handoff::HANDOFF_BYTES;
    if let Some(path) = &command.file {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|error| format!("handoff file: {error}"))?
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("handoff file: {error}"))?;
        if bytes.len() > limit {
            return Err(format!("handoff file exceeds {limit} bytes"));
        }
        Ok(bytes)
    } else if command.stdin {
        provenant_os::sys::hook_io::read_stdin(
            limit,
            std::time::Instant::now() + Duration::from_secs(5),
        )
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::FileTooLarge {
                format!("handoff exceeds {limit} bytes")
            } else {
                format!("handoff stdin: {error}")
            }
        })
    } else {
        Err("handoff requires exactly one of --file or --stdin".into())
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
        eprintln!("handoff field {field}: {detail}");
    }
    if items.is_empty() {
        eprintln!("{message}");
    }
}

/// Process edge: one declaration, one receipt, no prose fallback.
pub fn run(command: Handoff, _globals: &crate::Globals) -> u8 {
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
            eprintln!("handoff field $: invalid handoff JSON: {error}");
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
    let call = ToolCall::Handoff(input);
    let socket = provenant_providers::LaneRuntime::fabric_socket(attempt);
    let cancel = CancelToken::new();
    let mut reply = call.relay_within(&socket, &cancel, Duration::from_secs(10));
    if reply.as_ref().is_err_and(|error| transport(error)) {
        reply = call.relay_within(&socket, &cancel, Duration::from_secs(10));
    }
    match reply {
        Ok(value) => match serde_json::from_value::<DeclarationReceipt>(value) {
            Ok(receipt) => match serde_json::to_string(&receipt) {
                Ok(json) => {
                    println!("{json}");
                    0
                }
                Err(error) => {
                    eprintln!("handoff receipt: {error}");
                    1
                }
            },
            Err(error) => {
                eprintln!("handoff receipt: {error}");
                1
            }
        },
        Err(ServiceError::Cli {
            code,
            message,
            data,
        }) if code == "handoff_invalid" => {
            print_invalid(&message, &data);
            1
        }
        Err(error) => {
            eprintln!("handoff: {error}");
            1
        }
    }
}
