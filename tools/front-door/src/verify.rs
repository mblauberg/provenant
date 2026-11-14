//! Run the effective repository verification port.
use crate::{emit, fail, roots};
use provenant_cli::Globals;
use provenant_config::{
    CommandExitCodes, ExitDisposition, PortBackend, PortCommand, PortCommandDisposition,
    PortCommandExecution, PortSource, PortTable,
};
use provenant_core::CancelToken;
use provenant_os::exec::{SystemExecutor, Termination};
use provenant_records::{CanonicalV1, CommandError, CommandResult, Outcome};
use serde::Serialize;
use std::{ffi::OsString, io::Write, path::Path, time::Duration};

const COMMAND: &str = "verify";
const CONFIG_JSON_ENV: &str = "PROVENANT_VERIFY_EFFECTIVE_CONFIG_JSON";
const PROJECT_ENV: &str = "PROVENANT_VERIFY_PROJECT_ROOT";
const TIER_ENV: &str = "PROVENANT_VERIFY_TIER";

#[derive(Serialize)]
struct Report {
    tier: String,
    backend: &'static str,
    source: PortSource,
    disposition: Option<PortCommandDisposition>,
    exit_code: Option<i64>,
    elapsed_micros: Option<u64>,
    capture_complete: bool,
}

pub fn run(tier: Option<&str>, globals: &Globals) -> u8 {
    let resolved = match roots::resolve(
        globals,
        roots::Workspace::Checkout,
        roots::Product::Optional,
    ) {
        Ok(resolved) => resolved,
        Err(error) => {
            return fail(
                COMMAND,
                Outcome::Invalid,
                "ROOT",
                &error.to_string(),
                globals,
            );
        }
    };
    let project = resolved.project;
    let ports = match PortTable::read_for(Some(&project), resolved.roots.state.as_path()) {
        Ok(ports) => ports,
        Err(error) => {
            return fail(
                COMMAND,
                Outcome::Invalid,
                "PORT_CONFIG",
                &format!("cannot read verify port: {error}"),
                globals,
            );
        }
    };
    let declaration = ports.verify;
    let tier = tier.unwrap_or(&declaration.default_tier).to_owned();
    if !declaration.tiers.contains_key(&tier) {
        return fail(
            COMMAND,
            Outcome::Invalid,
            "VERIFY_TIER",
            &format!("verify tier {tier:?} is not declared by the effective verify port"),
            globals,
        );
    }
    let effective_config = match serde_json::to_string(&declaration) {
        Ok(config) => OsString::from(config),
        Err(error) => {
            return fail(
                COMMAND,
                Outcome::Error,
                "PORT_CONFIG",
                &format!("cannot encode effective verify config for the verify engine: {error}"),
                globals,
            );
        }
    };
    let stored_command_digest = match &declaration.backend {
        PortBackend::Command { command } => Some(command.digest()),
        PortBackend::Builtin | PortBackend::None => None,
    };
    let environment = [
        (OsString::from(CONFIG_JSON_ENV), effective_config),
        (OsString::from(PROJECT_ENV), project.as_os_str().to_owned()),
        (OsString::from(TIER_ENV), OsString::from(&tier)),
    ];
    let timeout = Duration::from_secs(provenant_core::MAX_LANE_RUNTIME_SECONDS);
    let cancel = CancelToken::new();
    let (backend, source, mut execution, should_record) = match declaration.backend {
        PortBackend::Builtin => {
            let script = resolved.assets.join("scripts/verify-tier");
            if !script.is_file() {
                return fail(
                    COMMAND,
                    Outcome::Unavailable,
                    "VERIFY_ENGINE_UNAVAILABLE",
                    "the builtin verify tier engine is not present in the selected product assets",
                    globals,
                );
            }
            let arguments = [
                script.as_os_str().to_owned(),
                "--project".into(),
                project.as_os_str().to_owned(),
                "--tier".into(),
                OsString::from(&tier),
            ];
            let command = PortCommand {
                argv: vec!["python3".into()],
                exit_codes: CommandExitCodes {
                    success: vec![0],
                    unknown: Vec::new(),
                    other: ExitDisposition::Refused,
                },
            };
            match execute(
                &command,
                &project,
                &arguments,
                &environment,
                timeout,
                &cancel,
            ) {
                Ok(mut execution) => {
                    if execution.disposition == PortCommandDisposition::Refused {
                        execution.disposition = PortCommandDisposition::Failed;
                    }
                    ("builtin", declaration.source, execution, false)
                }
                Err(error) => {
                    return fail(
                        COMMAND,
                        Outcome::Unavailable,
                        "VERIFY_ENGINE_UNAVAILABLE",
                        &format!("cannot start the builtin verify tier engine: {error}"),
                        globals,
                    );
                }
            }
        }
        PortBackend::Command { command } => {
            let command = substitute_tier(&command, &tier);
            match execute(&command, &project, &[], &environment, timeout, &cancel) {
                Ok(mut execution) => {
                    // The request key carries the selected tier; the digest binds
                    // the exact stored command template and survives substitution.
                    if let Some(digest) = stored_command_digest {
                        execution.command_digest = digest;
                    }
                    ("command", declaration.source, execution, true)
                }
                Err(_error) => {
                    let mut execution = PortCommandExecution::failed(&command);
                    if let Some(digest) = stored_command_digest {
                        execution.command_digest = digest;
                    }
                    ("command", declaration.source, execution, true)
                }
            }
        }
        PortBackend::None => {
            return render(
                Report {
                    tier,
                    backend: "none",
                    source: declaration.source,
                    disposition: None,
                    exit_code: None,
                    elapsed_micros: None,
                    capture_complete: false,
                },
                Outcome::Unavailable,
                Some((
                    "VERIFY_UNAVAILABLE",
                    "the effective verify port is disabled".into(),
                )),
                None,
                globals,
            );
        }
    };
    if execution.disposition == PortCommandDisposition::Succeeded && !execution.capture_complete() {
        execution.disposition = PortCommandDisposition::Unknown;
    }
    if should_record {
        let key = format!(
            "{}:{tier}",
            CanonicalV1::file_digest(project.as_os_str().as_encoded_bytes())
        );
        if let Err(error) = provenant_config::record_port_command(
            resolved.roots.state.as_path(),
            "verify",
            source,
            "tier",
            &key,
            &execution,
        ) {
            return render(
                report(&tier, backend, source, &execution),
                Outcome::Error,
                Some((
                    "VERIFY_EVIDENCE",
                    format!("cannot record verify command evidence: {error}"),
                )),
                Some(&execution),
                globals,
            );
        }
    }
    let status = outcome(execution.disposition);
    let message = failure_message(execution.disposition);
    render(
        report(&tier, backend, source, &execution),
        status,
        message,
        Some(&execution),
        globals,
    )
}

fn execute(
    command: &PortCommand,
    project: &Path,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    timeout: Duration,
    cancel: &CancelToken,
) -> Result<PortCommandExecution, provenant_os::exec::ExecError> {
    let executor = SystemExecutor::new(1)?;
    Ok(provenant_config::execute_port_command(
        &executor,
        command,
        project,
        arguments,
        environment,
        timeout,
        cancel,
    ))
}

fn substitute_tier(command: &PortCommand, tier: &str) -> PortCommand {
    let mut expanded = command.clone();
    expanded.argv = command
        .argv
        .iter()
        .map(|part| part.replace("{tier}", tier))
        .collect();
    expanded
}

fn report(
    tier: &str,
    backend: &'static str,
    source: PortSource,
    execution: &PortCommandExecution,
) -> Report {
    Report {
        tier: tier.to_owned(),
        backend,
        source,
        disposition: Some(execution.disposition),
        exit_code: execution
            .outcome
            .as_ref()
            .and_then(|outcome| match outcome.termination {
                Termination::Exit(code) => Some(code),
                Termination::Signal(_) => None,
            }),
        elapsed_micros: execution
            .outcome
            .as_ref()
            .map(|outcome| outcome.elapsed_micros),
        capture_complete: execution.capture_complete(),
    }
}

fn outcome(disposition: PortCommandDisposition) -> Outcome {
    match disposition {
        PortCommandDisposition::Succeeded => Outcome::Ok,
        PortCommandDisposition::Refused => Outcome::Refused,
        PortCommandDisposition::TimedOut => Outcome::Timeout,
        PortCommandDisposition::Cancelled => Outcome::Cancelled,
        PortCommandDisposition::Unknown | PortCommandDisposition::Failed => Outcome::Error,
    }
}

fn failure_message(disposition: PortCommandDisposition) -> Option<(&'static str, String)> {
    let (code, message) = match disposition {
        PortCommandDisposition::Succeeded => return None,
        PortCommandDisposition::Unknown => ("VERIFY_UNKNOWN", "verify command result is unknown"),
        PortCommandDisposition::Refused => {
            ("VERIFY_REFUSED", "verify command refused the selected tier")
        }
        PortCommandDisposition::TimedOut => (
            "VERIFY_TIMEOUT",
            "verify command exceeded its execution time limit",
        ),
        PortCommandDisposition::Cancelled => ("VERIFY_CANCELLED", "verify command was cancelled"),
        PortCommandDisposition::Failed => ("VERIFY_FAILED", "verification failed"),
    };
    Some((code, message.into()))
}

fn render(
    report: Report,
    status: Outcome,
    error: Option<(&'static str, String)>,
    execution: Option<&PortCommandExecution>,
    globals: &Globals,
) -> u8 {
    let output_error = if !globals.json {
        execution
            .and_then(|execution| execution.outcome.as_ref())
            .and_then(|outcome| {
                std::io::stdout()
                    .lock()
                    .write_all(&outcome.stdout.bytes)
                    .and_then(|()| std::io::stderr().lock().write_all(&outcome.stderr.bytes))
                    .err()
            })
    } else {
        None
    };
    let (status, error) = match output_error {
        Some(error) => (
            Outcome::Error,
            Some((
                "VERIFY_OUTPUT",
                format!("cannot render verify output: {error}"),
            )),
        ),
        None => (status, error),
    };
    let message = match &error {
        Some((_, message)) => format!("{message}: tier {}", report.tier),
        None => format!(
            "verify tier {} completed via {} ({:?})",
            report.tier, report.backend, report.source
        ),
    };
    let mut result = CommandResult::new(COMMAND, status, report);
    if let Some((code, message)) = error {
        result.error = Some(CommandError {
            diagnostic: None,
            code: code.into(),
            message,
            cause: None,
            repair: None,
        });
    }
    emit(&result, globals, &message, false)
}
