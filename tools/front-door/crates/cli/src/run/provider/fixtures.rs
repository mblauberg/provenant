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
) -> Result<provenant_fabric_mcp::lane::HostRelay, String> {
    use provenant_core::CancelToken;
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
    let mut ctx = crate::fabric::Context::open(roots, &context.roots.home, &launch.cwd)
        .map_err(|e| e.to_string())?;
    if ctx.identity.project != context.project_id {
        return Err("Fabric relay project mismatch".into());
    }
    ctx.identity.inbox = Address::new(ctx.identity.host, &format!("lane-{}", context.attempt_id))
        .map_err(|e| e.to_string())?;
    let home = context.roots.home.clone();
    let cwd = launch.cwd.clone();
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
    provenant_fabric_mcp::lane::HostRelay::start(
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
    )
    .map_err(|e| e.to_string())
}
