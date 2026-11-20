//! Command strategy adapter, preserving the former build admission wrapper.

use super::{ExecPlace, Job, JobOutcome, JobResult, RunContext};
use provenant_config::{ExitDisposition, PortCommand};
use provenant_core::consumer_env::{ADMISSION_SLOT, PORT_PHASE, PORT_SOURCE};
use std::ffi::OsString;

/// Run the configured build command around the original executable and argv.
pub(super) fn execute(
    job: &mut Job,
    ctx: &RunContext,
    mut outcome: JobOutcome,
) -> Result<JobResult, super::DispatchFailure> {
    let Some(step) = job.steps.first() else {
        let error = super::BuildError::Config("build job has no command step".into());
        outcome.finished_unix_ms = super::ledger::now_unix_ms();
        return Err((error, Box::new(outcome)));
    };
    let command = &ctx.strategy.port.command;
    if !command.is_valid() {
        let error = super::BuildError::Config("repository build wrapper has no executable".into());
        outcome
            .warnings
            .push("repository build wrapper has no executable".into());
        outcome.finished_unix_ms = super::ledger::now_unix_ms();
        return Err((error, Box::new(outcome)));
    }
    if step.argv.is_empty() {
        let error = super::BuildError::Config("build step has empty argv".into());
        outcome.finished_unix_ms = super::ledger::now_unix_ms();
        return Err((error, Box::new(outcome)));
    }
    let mut argv: Vec<OsString> = command.argv.iter().map(OsString::from).collect();
    argv.extend(step.argv.iter().cloned());

    let mut explicit = Vec::new();
    for name in [
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_JOBS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "RUSTDOCFLAGS",
        "RUSTC",
        "RUSTDOC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        ADMISSION_SLOT,
    ] {
        if let Some((_, value)) = job.env.iter().find(|(key, _)| key == name) {
            explicit.push((OsString::from(name), value.clone()));
        }
    }
    explicit.push((OsString::from(PORT_PHASE), OsString::from("build")));
    explicit.push((
        OsString::from(PORT_SOURCE),
        OsString::from(source_name(ctx.strategy.source)),
    ));
    let env = provenant_config::port_command_environment(job.env.iter().cloned(), &explicit);
    let original = std::mem::replace(&mut job.steps[0].argv, argv);
    let result =
        super::local::execute_with_env(job, ctx, outcome, ExecPlace::Local, None, 0, Some(env));
    job.steps[0].argv = original;
    let result = result?;
    let Some(exit) = result.outcome.exit else {
        return Ok(result);
    };
    let code = exit.process_code();
    {
        let disposition = disposition(command, code);
        match disposition {
            Disposition::Success => Ok(with_exit(result, 0)),
            Disposition::Unknown => {
                let msg = format!(
                    "provenant: repository build admission returned an unknown outcome (source: {:?})",
                    ctx.strategy.source
                );
                Ok(with_warning(
                    with_exit(result, if code == 0 { 1 } else { code }),
                    msg,
                ))
            }
            Disposition::Refused => {
                let msg = format!(
                    "provenant: repository build admission refused cargo (source: {:?})",
                    ctx.strategy.source
                );
                Ok(with_warning(
                    with_exit(result, if code == 0 { 1 } else { code }),
                    msg,
                ))
            }
        }
    }
}

fn disposition(command: &PortCommand, code: i32) -> Disposition {
    if command.exit_codes.success.contains(&code) {
        Disposition::Success
    } else if command.exit_codes.unknown.contains(&code)
        || command.exit_codes.other == ExitDisposition::Unknown
    {
        Disposition::Unknown
    } else {
        Disposition::Refused
    }
}

fn with_warning(mut result: JobResult, warning: String) -> JobResult {
    result.outcome.warnings.push(warning);
    result
}

fn with_exit(mut result: JobResult, code: i32) -> JobResult {
    let exit = super::ExitStatus::code(code);
    result.outcome.exit = Some(exit);
    if let Some(step) = result.step_exits.first_mut() {
        *step = Some(exit);
    }
    result
}

#[derive(Clone, Copy)]
enum Disposition {
    Success,
    Unknown,
    Refused,
}

fn source_name(source: provenant_config::PortSource) -> &'static str {
    match source {
        provenant_config::PortSource::Builtin => "default",
        provenant_config::PortSource::Detected => "detected",
        provenant_config::PortSource::Host => "host",
        provenant_config::PortSource::HostProject => "host_project",
        provenant_config::PortSource::Project => "project",
    }
}
