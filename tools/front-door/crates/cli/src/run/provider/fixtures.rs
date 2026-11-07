use super::*;

/// Apply provider setup with the same path checks used by launch preparation.
#[cfg(feature = "fixture")]
pub fn apply_fixture_setup(steps: &[SetupStep], roots: &[&Path]) -> Result<(), String> {
    apply_setup(steps, roots, None)
}

/// Fixture access to the production plan builder; installed builds omit it.
#[cfg(feature = "fixture")]
pub fn writer_fixture_plan(
    project: ProjectId,
    cwd: &Path,
    home: &Path,
    paths: &run::LaunchPaths,
    writer: &run::WriterLaunch,
    assets: &Path,
) -> Result<ConfinedPlan, String> {
    let scratch = paths.temporary.join("candidate-0");
    apply_setup(
        &[SetupStep::CreateDir(scratch.clone())],
        &[&paths.temporary],
        None,
    )?;
    let request = confinement_request(
        project,
        ConfinementRoots {
            cwd,
            home,
            scratch: &scratch,
            private_state: &paths.provider_state,
            assets,
            prompt_file: None,
        },
        Some(writer),
        &run::LaneAccess::default(),
        &Default::default(),
        "codex",
        true,
    )
    .map_err(|error| error.to_string())?;
    ConfinedPlan::new(request).map_err(|error| error.to_string())
}

/// Exercise production preparation against a fake external provider executable.
/// A supplied Claude token stands in for the host authority and refresh path.
/// Optional publishing uses only a fixture gh beside the supplied executable.
#[cfg(feature = "fixture")]
pub fn prepare_fixture(
    registry: &Registry<'_>,
    task: &run::TaskRequest,
    prompt: &str,
    paths: &run::LaunchPaths,
    executable: &Path,
    token: Option<&provenant_providers::claude::OauthToken>,
) -> Result<run::PreparedLaunch, String> {
    prepare_with(
        registry,
        task,
        prompt,
        paths,
        None,
        |name| {
            if name == "gh" {
                canonical(&executable.with_file_name("gh"))
            } else {
                canonical(executable)
            }
        },
        token,
    )
    .map_err(|error| error.to_string())
}

/// Called only by the host guardian with the context accepted under custody.
/// The attempt determines the actor, regardless of request or environment labels.
pub(crate) fn start_fabric_relay(
    context: &run::HolderContext,
    graph_wake: Option<std::sync::Arc<super::super::holder::GraphWake>>,
    graph_read: Option<std::sync::Arc<super::super::holder::GraphRead>>,
) -> Result<provenant_fabric_mcp::lane::HostRelay, String> {
    use provenant_core::{CancelToken, Clock};
    use provenant_fabric_runtime::{coordination::CoordinationService, identity::Address};
    let launch = context
        .launch
        .as_ref()
        .ok_or("host launch context missing")?;
    let mut inputs =
        provenant_core::RootInputs::new(context.roots.home.clone(), context.roots.checkout.clone());
    inputs.state_dir = Some(launch.configuration_root.clone());
    let roots = inputs
        .resolve(&provenant_os::fs::SystemPathResolver)
        .map_err(|e| e.to_string())?;
    let relay_cwd = launch.graph_directory.as_deref().unwrap_or(&launch.cwd);
    let mut ctx = crate::fabric::Context::open(roots, &context.roots.home, relay_cwd)
        .map_err(|e| e.to_string())?;
    if ctx.identity.project != context.project_id {
        return Err("Fabric relay project mismatch".into());
    }
    ctx.identity.inbox = Address::new(ctx.identity.host, &format!("lane-{}", context.attempt_id))
        .map_err(|e| e.to_string())?;
    let home = context.roots.home.clone();
    let cwd = relay_cwd.to_path_buf();
    let clock = provenant_os::clock::SystemClock::new();
    let executor = provenant_os::exec::SystemExecutor::new(provenant_fabric_mcp::lane::WORKERS)
        .map_err(|e| e.to_string())?;
    let cancel = CancelToken::new();
    // Initialise the store before a lane opens it; store writers persist WAL companions.
    drop(
        CoordinationService::for_identity(
            &ctx.roots.state,
            &ctx.identity,
            &ctx.config,
            &home,
            &cwd,
            &executor,
            &clock,
            &provenant_fabric_runtime::coordination::ThreadPause,
            &cancel,
        )
        .map_err(|e| e.to_string())?,
    );
    let ctx = std::sync::Arc::new(ctx);
    let graph_ctx = std::sync::Arc::clone(&ctx);
    let attempt = context.attempt_id.to_string();
    let graph_clock = provenant_os::clock::SystemClock::new();
    let graph_holder = context.clone();
    provenant_fabric_mcp::lane::HostRelay::start_with_graph(
        LaneRuntime::fabric_socket(context.attempt_id),
        provenant_records::Limits {
            max_bytes: ctx.config.limits.input_frame_bytes,
            max_depth: usize::try_from(ctx.config.limits.json_depth).unwrap_or(64),
            max_members: 100_000,
        },
        move |call, cancel, within| {
            let budget = provenant_fabric_mcp::lane::host_budget(&clock, within)?;
            let coordination = CoordinationService::for_identity(
                &ctx.roots.state,
                &ctx.identity,
                &ctx.config,
                &home,
                &cwd,
                &executor,
                &clock,
                &provenant_fabric_runtime::coordination::ThreadPause,
                cancel,
            )?;
            call.execute_lane_coordination(&coordination, &ctx.config.limits, budget, cancel)
        },
        move |arguments, cancel: &CancelToken, within| {
            let budget = provenant_fabric_mcp::lane::host_budget(&graph_clock, within)?;
            let observe = arguments["op"] == "read";
            serve_graph_frame(
                || {
                    crate::fabric::graph_relay::serve_with_read(
                        &graph_ctx,
                        &attempt,
                        arguments,
                        budget,
                        cancel,
                        &graph_clock,
                        &|input| match &graph_read {
                            Some(read) => read(
                                &graph_holder,
                                input,
                                cancel,
                                std::time::Duration::from_micros(
                                    budget.remaining(graph_clock.mono_now()).as_micros(),
                                ),
                            ),
                            None => Err(provenant_fabric_mcp::tools::ServiceError::Cli {
                                code: "graph_read_unavailable".into(),
                                message: "the host has no graph reader; upgrade the host".into(),
                                data: serde_json::Value::Null,
                            }),
                        },
                    )
                },
                |id| {
                    if observe {
                        return Ok(());
                    }
                    if let Some(wake) = &graph_wake {
                        wake(&graph_holder, id)
                    } else {
                        Ok(())
                    }
                },
            )
        },
    )
    .map_err(|e| e.to_string())
}

/// Finish admission before waking; this also covers an idempotent replay.
fn serve_graph_frame(
    serve: impl FnOnce() -> Result<serde_json::Value, provenant_fabric_mcp::tools::ServiceError>,
    wake: impl FnOnce(&str) -> Result<(), String>,
) -> Result<serde_json::Value, provenant_fabric_mcp::tools::ServiceError> {
    let receipt = serve()?;
    if let Some(id) = receipt["graph_id"].as_str() {
        wake(id).map_err(|message| provenant_fabric_mcp::tools::ServiceError::Cli {
            code: "graph_wake_failed".into(),
            message,
            data: receipt.clone(),
        })?;
    }
    Ok(receipt)
}

#[cfg(test)]
mod graph_wake_tests {
    use super::*;

    #[test]
    fn relay_wakes_after_admission_releases_custody_and_on_replay() {
        let custody = std::sync::Mutex::new(0);
        for replayed in [false, true] {
            let receipt = serve_graph_frame(
                || {
                    let mut revision = custody.lock().unwrap();
                    if !replayed {
                        *revision += 1;
                    }
                    Ok(serde_json::json!({"graph_id":"children", "replayed":replayed}))
                },
                |id| {
                    assert_eq!(id, "children");
                    // The owner can take custody and sees the durable revision.
                    assert_eq!(*custody.try_lock().unwrap(), 1);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(receipt["replayed"], replayed);
        }
        let failed = serve_graph_frame(
            || {
                Err(provenant_fabric_mcp::tools::ServiceError::Facade(
                    provenant_fabric_mcp::tools::FacadeError::StateCorrupt,
                ))
            },
            |_| panic!("a refused admission must not wake the owner"),
        );
        assert!(matches!(
            failed,
            Err(provenant_fabric_mcp::tools::ServiceError::Facade(
                provenant_fabric_mcp::tools::FacadeError::StateCorrupt,
            ))
        ));
    }
}
