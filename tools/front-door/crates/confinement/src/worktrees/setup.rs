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
        Err(error) => return fallback(format!("could not read host state: {error}")),
    };
    let layout = match context.project_layout() {
        Ok(layout) => layout,
        Err(error) => return fallback(format!("could not resolve project layout: {error}")),
    };
    let ports = match provenant_config::PortTable::read_for(layout.project(), &state) {
        Ok(ports) => ports,
        Err(error) => return fallback(format!("could not read port declaration: {error}")),
    };
    let command =
        provenant_config::dispatch_port_backend(&ports.naming.backend, || None, Some, || None);
    let Some(command) = command else {
        return (requested.to_owned(), None);
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
    let publication = match provenant_config::record_port_command(
        &state,
        "naming",
        ports.naming.source,
        "derive",
        &evidence_key,
        &execution,
        provenant_config::PortReceiptLifetime::AgeDays {
            days: provenant_config::DEFAULT_PORT_COMMAND_RECEIPT_AGE_DAYS,
        },
    ) {
        Ok(publication) => publication,
        Err(error) => return fallback(format!("could not record command evidence: {error}")),
    };
    let durability_warning = match publication {
        provenant_config::PortCommandPublication::Durable => None,
        provenant_config::PortCommandPublication::VisibleDurabilityUnconfirmed { cause } => Some(
            format!("command receipt is visible but durability is unconfirmed: {cause}"),
        ),
    };
    let fallback_after_publication = |reason: String| {
        let (name, warning) = fallback(reason);
        let warning = match (warning, durability_warning.as_ref()) {
            (Some(warning), Some(receipt)) => Some(format!("{warning}; {receipt}")),
            (Some(warning), None) => Some(warning),
            (None, Some(receipt)) => Some(receipt.clone()),
            (None, None) => None,
        };
        (name, warning)
    };
    if execution.disposition != provenant_config::PortCommandDisposition::Succeeded {
        return fallback_after_publication(format!("returned {:?}", execution.disposition));
    }
    let Some(outcome) = execution.outcome.as_ref() else {
        return fallback_after_publication("returned no execution evidence".into());
    };
    if !execution.capture_complete() {
        return fallback_after_publication("returned incomplete output".into());
    }
    let output = match String::from_utf8(outcome.stdout.bytes.clone()) {
        Ok(output) => output,
        Err(_) => return fallback_after_publication("returned non-UTF-8 output".into()),
    };
    let output = output
        .strip_suffix("\r\n")
        .or_else(|| output.strip_suffix('\n'))
        .unwrap_or(&output);
    if output.is_empty() && outcome.stdout.bytes.is_empty() {
        return (requested.to_owned(), durability_warning);
    }
    if output.is_empty()
        || output.trim() != output
        || output.contains(['\n', '\r'])
        || crate::repo::GitRefName::branch(output).is_err()
    {
        return fallback_after_publication("returned an invalid derived branch".into());
    }
    (output.to_owned(), durability_warning)
}

pub(super) fn run(
    context: &WorktreeContext<'_>,
    path: &Path,
    branch: &str,
    cancel: &CancelToken,
) -> Vec<String> {
    let mut durability_warning = None;
    let result = (|| -> Result<(), String> {
        let state = context.host_state().map_err(|error| error.to_string())?;
        let settings = provenant_config::WorktreeSettings::read(&state)
            .map_err(|error| format!("could not read post-create settings: {error}"))?
            .for_project(context.binding.project_id());
        let layout = context
            .project_layout()
            .map_err(|error| error.to_string())?;
        let ports = provenant_config::PortTable::read_for(layout.project(), &state)
            .map_err(|error| format!("could not read ports: {error}"))?;
        let command = provenant_config::dispatch_port_backend(
            &ports.post_create.backend,
            || None,
            Some,
            || None,
        );
        let Some(command) = command else {
            return Ok(());
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
        let publication = provenant_config::record_port_command(
            &state,
            "post_create",
            ports.post_create.source,
            "create",
            &evidence_key,
            &execution,
            provenant_config::PortReceiptLifetime::AgeDays {
                days: provenant_config::DEFAULT_PORT_COMMAND_RECEIPT_AGE_DAYS,
            },
        )
        .map_err(|error| format!("could not record command evidence: {error}"))?;
        if let provenant_config::PortCommandPublication::VisibleDurabilityUnconfirmed { cause } =
            publication
        {
            durability_warning = Some(format!(
                "post-create receipt is visible but durability is unconfirmed: {cause}"
            ));
        }
        match execution.disposition {
            provenant_config::PortCommandDisposition::Succeeded => Ok(()),
            disposition => Err(format!("post-create command returned {disposition:?}")),
        }
    })();
    match result {
        Ok(()) => durability_warning.into_iter().collect(),
        Err(error) => {
            let warning = format!("{error}; worktree remains usable at {}", path.display());
            let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
            let mut warnings: Vec<_> = durability_warning.into_iter().collect();
            warnings.push(warning);
            warnings
        }
    }
}
