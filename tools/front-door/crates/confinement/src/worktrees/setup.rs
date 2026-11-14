//! Repository post-create hook under the new worktree's custody.
use super::WorktreeContext;
use provenant_core::{CancelToken, WaitBudget};
use std::{ffi::OsString, io::Write, path::Path, time::Duration};

pub(super) fn branch_name(
    context: &WorktreeContext<'_>,
    requested: &str,
    cancel: &CancelToken,
) -> (String, Option<String>) {
    let fallback = |reason: String| {
        (
            requested.to_owned(),
            Some(format!(
                "naming hook {reason}; keeping requested branch name"
            )),
        )
    };
    let state = match context.host_state() {
        Ok(state) => state,
        Err(error) => return fallback(error),
    };
    let ports = match provenant_config::PortTable::read_for(Some(context.repo.root_path()), &state)
    {
        Ok(ports) => ports,
        Err(error) => return fallback(format!("could not read port declaration: {error}")),
    };
    let command = match &ports.naming.backend {
        provenant_config::PortBackend::Builtin | provenant_config::PortBackend::None => {
            return (requested.to_owned(), None);
        }
        provenant_config::PortBackend::Command { command } => command,
    };
    let settings = match provenant_config::WorktreeSettings::read(&state) {
        Ok(settings) => settings,
        Err(_) => return fallback("timeout configuration is unavailable".into()),
    };
    let timeout_seconds = settings
        .for_project(context.binding.project_id())
        .timeout_seconds;
    let arguments = [
        OsString::from(requested),
        context.repo.root_path().as_os_str().to_owned(),
    ];
    let execution = provenant_config::execute_port_command(
        context.executor,
        command,
        context.repo.root_path(),
        &arguments,
        &[],
        Duration::from_secs(timeout_seconds),
        cancel,
    );
    let evidence_key = format!("{}:{requested}", context.binding.project_id());
    if let Err(error) = provenant_config::record_port_command(
        &state,
        "naming",
        ports.naming.source,
        "derive",
        &evidence_key,
        &execution,
    ) {
        return fallback(format!("could not record command evidence: {error}"));
    }
    if execution.disposition != provenant_config::PortCommandDisposition::Succeeded {
        return fallback(format!("returned {:?}", execution.disposition));
    }
    let Some(outcome) = execution.outcome else {
        return fallback("returned no execution evidence".into());
    };
    if !execution.capture_complete() {
        return fallback("returned incomplete output".into());
    }
    let output = match String::from_utf8(outcome.stdout.bytes.clone()) {
        Ok(output) => output,
        Err(_) => return fallback("returned non-UTF-8 output".into()),
    };
    let output = output
        .strip_suffix("\r\n")
        .or_else(|| output.strip_suffix('\n'))
        .unwrap_or(&output);
    if output.is_empty() && outcome.stdout.bytes.is_empty() {
        return (requested.to_owned(), None);
    }
    if output.is_empty()
        || output.trim() != output
        || output.contains(['\n', '\r'])
        || crate::repo::GitRefName::branch(output).is_err()
    {
        return fallback("returned an invalid derived branch".into());
    }
    (output.to_owned(), None)
}

pub(super) fn run(
    context: &WorktreeContext<'_>,
    path: &Path,
    branch: &str,
    cancel: &CancelToken,
) -> Vec<String> {
    let result = (|| -> Result<(), String> {
        let state = context.host_state().map_err(|error| error.to_string())?;
        let settings = provenant_config::WorktreeSettings::read(&state)
            .map_err(|error| format!("could not read post-create settings: {error}"))?
            .for_project(context.binding.project_id());
        let ports = provenant_config::PortTable::read_for(Some(context.repo.root_path()), &state)
            .map_err(|error| format!("could not read ports: {error}"))?;
        let command = match &ports.post_create.backend {
            provenant_config::PortBackend::Builtin | provenant_config::PortBackend::None => {
                return Ok(());
            }
            provenant_config::PortBackend::Command { command } => command,
        };
        let budget = WaitBudget::cli(context.clock.mono_now(), settings.timeout_seconds)
            .map_err(|error| error.to_string())?;
        let remaining =
            Duration::from_micros(budget.remaining(context.clock.mono_now()).as_micros());
        if remaining.is_zero() {
            return Err("post-create command timed out".into());
        }
        let arguments = [OsString::from(branch), path.as_os_str().to_owned()];
        let execution = provenant_config::execute_port_command(
            context.executor,
            command,
            path,
            &arguments,
            &[],
            remaining,
            cancel,
        );
        let evidence_key = format!(
            "{}:{}:{}",
            context.binding.project_id(),
            branch,
            path.display()
        );
        provenant_config::record_port_command(
            &state,
            "post_create",
            ports.post_create.source,
            "create",
            &evidence_key,
            &execution,
        )
        .map_err(|error| format!("could not record command evidence: {error}"))?;
        match execution.disposition {
            provenant_config::PortCommandDisposition::Succeeded => Ok(()),
            disposition => Err(format!("post-create command returned {disposition:?}")),
        }
    })();
    match result {
        Ok(()) => Vec::new(),
        Err(error) => {
            let warning = format!("{error}; worktree remains usable at {}", path.display());
            let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
            vec![warning]
        }
    }
}
