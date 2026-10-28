//! Host housekeeping on existing quota events. No background timer or service.
use provenant_core::{CancelToken, Clock, OperationId, Roots, WaitBudget};
use provenant_fabric_runtime::coordination::{
    EventsInput, LocalCoordination, Mutation, ThreadWait,
};
use provenant_os::{clock::SystemClock, exec::SystemExecutor};
use std::path::Path;

pub fn housekeep(roots: &Roots, home: &Path, cwd: &Path) -> Result<(), String> {
    if crate::fabric::in_lane() {
        return Ok(());
    }
    let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|e| e.to_string())?;
    let clock = SystemClock::new();
    let now = clock
        .wall_now()
        .map_err(|e| e.to_string())?
        .as_timestamp()
        .as_second();
    let executor = SystemExecutor::new(1).map_err(|e| e.to_string())?;
    // At most one native update per event keeps the menu's existing tick bounded.
    for (provider, settings) in &config.providers {
        let Ok(executable) = crate::run::provider_executable(provider) else {
            continue;
        };
        let Some(record) = provenant_providers::update::check(
            roots.state.as_path(),
            provider,
            &executable,
            settings,
            &executor,
            now,
        )?
        else {
            continue;
        };
        let ctx =
            crate::fabric::Context::open(roots.clone(), home, cwd).map_err(|e| e.to_string())?;
        let cancel = CancelToken::new();
        let budget = WaitBudget::cli(clock.mono_now(), 2).map_err(|e| e.to_string())?;
        let service = LocalCoordination::open(
            &ctx.roots.state,
            ctx.identity.project,
            ctx.identity.inbox.actor,
            &clock,
            &ThreadWait,
            budget,
            &cancel,
        )
        .map_err(|e| e.to_string())?;
        let operation = OperationId::parse(&format!(
            "provider-update-{provider}-{}",
            record.checked_unix_seconds
        ))
        .map_err(|e| e.to_string())?;
        let note = format!(
            "Provider {provider} update: {}; old version: {}; new version: {}",
            record.outcome,
            record.old_version.as_deref().unwrap_or("unknown"),
            record.new_version.as_deref().unwrap_or("unknown"),
        );
        service
            .mutate(
                Mutation::Events(EventsInput::Note {
                    note,
                    dedup_key: None,
                }),
                &operation,
                budget,
                &cancel,
            )
            .map_err(|e| e.to_string())?;
        if record.outcome != "unknown install method" {
            break;
        }
    }
    Ok(())
}
