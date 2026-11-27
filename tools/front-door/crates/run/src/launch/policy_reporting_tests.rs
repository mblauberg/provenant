use super::*;
use crate::tests::{call, clock, context, input, route};
use provenant_confinement::permissions::{
    AccessMode, MandatoryRoots, NetworkAccess, PathGrant, PathScope, PlanPermissions,
    ProviderProfile, ProviderProfileSpec,
};
use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
use provenant_core::{CancelToken, Clock as _, WorkScope};
use std::path::Path;

// Medium: real accepted requests and continuation through RunStore::start;
// the provider edge inspects admission without spawning a process.
#[test]
fn continuation_keeps_the_sealed_parent_floor_and_policy()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    struct Inspect<'a> {
        strength: &'a str,
        prior: Option<AttemptId>,
    }
    impl Providers for Inspect<'_> {
        fn prepare(
            &self,
            task: &TaskRequest,
            _: &str,
            _: &LaunchPaths,
            prior: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, Box<PrepareRefusal>> {
            assert_eq!(prior.map(|record| record.attempt_id), self.prior);
            assert!(task.profile.parent_sandboxed);
            let strength = task
                .profile
                .knobs
                .get("lanes.sandbox_floor")
                .and_then(serde_json::Value::as_str)
                .unwrap();
            assert_eq!(strength, self.strength);
            let sandbox = task
                .profile
                .knobs
                .get("lanes.sandbox")
                .and_then(serde_json::Value::as_str);
            assert_eq!(
                provenant_records::run::admission_floor(true, sandbox, strength),
                if self.strength == "strict" {
                    (true, false)
                } else {
                    (false, true)
                }
            );
            Err(PrepareRefusal::boxed(
                CauseKind::RouteResolutionRefused,
                "fixture inspected admission",
            ))
        }
        fn decode(
            &self,
            _: &RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &Path,
        ) -> std::result::Result<Decoded, String> {
            unreachable!()
        }
    }
    for strength in ["warn", "strict"] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let parent = input();
        store.create(&parent, &call)?;
        let mut applied = route();
        applied.sandbox = Some(provenant_records::run::SandboxModeReceipt {
            requested: true,
            effective: true,
            source: provenant_records::Layer::Host,
            constraint: None,
            warning: None,
            extra: Default::default(),
        });
        let starting = store.mark_starting(
            &store.record(parent.attempt_id, &call)?,
            &applied,
            &[],
            &Default::default(),
            None,
            &call,
        )?;
        store.withdraw(
            parent.attempt_id,
            starting.state,
            Outcome::Rejected,
            "fixture parent",
            clock.wall_now()?,
            &call,
        )?;
        assert!(store.attempt_floors_children(parent.attempt_id, &call));
        let mut child = input();
        child.attempt_id = AttemptId::from_bytes([7; 16]);
        child.operation_id = provenant_core::OperationId::parse("admission-child")?;
        child.request.parent_attempt = Some(parent.attempt_id);
        child
            .request
            .task
            .profile
            .knobs
            .insert("lanes.sandbox".into(), "off".into());
        child
            .request
            .task
            .profile
            .knobs
            .insert("lanes.sandbox_floor".into(), strength.into());
        crate::tests::rekey(&mut child);
        store.create(&child, &call)?;
        store.start(
            child.attempt_id,
            &Inspect {
                strength,
                prior: None,
            },
            &|_, _| unreachable!(),
            &call,
        )?;
        let mut continuation = input();
        continuation.request = child.request.clone();
        continuation.prompt = child.prompt.clone();
        continuation.attempt_id = AttemptId::from_bytes([8; 16]);
        continuation.operation_id = provenant_core::OperationId::parse("admission-continuation")?;
        continuation.request.parent = Some(child.attempt_id);
        store.create(&continuation, &call)?;
        store.start(
            continuation.attempt_id,
            &Inspect {
                strength,
                prior: Some(child.attempt_id),
            },
            &|_, _| unreachable!(),
            &call,
        )?;
    }
    Ok(())
}

// Spec: docs/specs/run-launch.md#policy-and-confinement
#[test]
fn claimed_launch_reports_once_when_a_prepared_attempt_is_replayed()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let run = input();
    store.create(&run, &call)?;
    let expected =
        store.claim_starting_for_preparation(&store.record(run.attempt_id, &call)?, &call)?;
    let grant = |path: &str| PathGrant {
        path: path.into(),
        scope: PathScope::Subtree,
    };
    let mandatory = MandatoryRoots::new(
        Path::new("/fixture/home"),
        Path::new("/fixture/state"),
        Path::new("/fixture/source"),
    )?;
    let policy = provenant_confinement::run_config::RunConfig::default()
        .policy_for_launch(Path::new("/fixture/state"), mandatory);
    let plan = ConfinedPlan::new(PlanRequest {
        scope: WorkScope::Project(store.context.project_id),
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
        permissions: PlanPermissions {
            credential_roots: policy.mandatory().credential_roots().clone(),
            policy: Some(policy),
            read_only: None,
            mode: AccessMode::ReadOnly,
            scratch: grant("/fixture/scratch"),
            private_state: None,
            read_roots: vec![],
            protected_roots: vec![],
            secret_roots: vec![],
            selected_authentication: None,
            include_continuation: false,
            write_roots: vec![],
            network: NetworkAccess::Open,
            nested_confinement: false,
        },
    })?;
    let inherited_sandbox = provenant_confinement::run_config::RunConfig::default().sandbox_for(
        provenant_confinement::run_config::SandboxInputs {
            route: "command/test",
            knob: None,
            parent_sandboxed: false,
            restrictions: &[],
            os_sandboxed: true,
            platform_supported: cfg!(target_os = "macos"),
        },
    );
    assert!(!inherited_sandbox.requires_backend());
    let mut prepared = PreparedLaunch {
        limits: Default::default(),
        continuation_expectation: None,
        argv: vec!["fixture".into()],
        environment: vec![],
        stdin: vec![],
        secret_input: None,
        cwd: "/fixture/source".into(),
        route: route(),
        skipped_routes: vec![],
        observed_head: None,
        repository: None,
        confinement: Some(crate::LaunchConfinement {
            plan,
            profile: ProviderProfile {
                spec: ProviderProfileSpec::default(),
                observations: Default::default(),
            },
            writer: None,
            home: "/fixture/home".into(),
            cargo_home: provenant_confinement::cargo::CargoHome::resolve(
                std::path::Path::new("/fixture/home"),
                |_| None,
            )
            .unwrap(),
            state_root: "/fixture/state".into(),
            sandbox: inherited_sandbox,
            backend: None,
        }),
    };
    prepared.bind_sandbox_backend(&cancel, &Default::default())?;
    let mode = prepared
        .route
        .sandbox
        .as_ref()
        .ok_or("missing inherited sandbox receipt")?;
    assert!(mode.effective);
    assert!(mode.warning.is_none());
    assert!(
        prepared
            .confinement
            .as_ref()
            .ok_or("missing confinement")?
            .backend
            .is_none(),
        "trusted inherited route must not probe a nested backend"
    );
    assert!(
        prepared.environment.iter().any(|(name, value)| {
            name == provenant_core::consumer_env::SANDBOX && value == "on"
        })
    );
    let grants = applied_write_grants(&prepared, WriteGrantLimit::default(), &Default::default())?;
    let mut output = Vec::new();
    let claimed = store.claim_launch(
        &expected,
        &prepared,
        &grants,
        0,
        &Default::default(),
        &call,
        &mut output,
    )?;
    assert!(matches!(claimed.state, AttemptState::Starting { .. }));
    assert_eq!(
        claimed
            .applied_route
            .as_ref()
            .and_then(|route| route.sandbox.as_ref()),
        prepared.route.sandbox.as_ref(),
        "the claim must persist the inherited effective mode",
    );
    // Another launcher can have prepared from the same queued record.
    // The compare-and-swap must refuse its stale claim before reporting.
    assert!(matches!(
        store.claim_launch(
            &expected,
            &prepared,
            &grants,
            0,
            &Default::default(),
            &call,
            &mut output
        ),
        Err(RunError::Conflict)
    ));
    assert_eq!(
        String::from_utf8(output)?
            .lines()
            .filter(|line| line.starts_with("lane policy:"))
            .count(),
        1
    );
    Ok(())
}

// Spec: docs/specs/run-launch.md#lane-scratch-stays-outside-the-attempt-record
#[test]
fn launched_lane_scratch_and_tmpdir_stay_outside_the_attempt_record()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    struct Capture(std::cell::RefCell<Option<LaunchPaths>>);
    impl Providers for Capture {
        fn prepare(
            &self,
            _: &TaskRequest,
            _: &str,
            paths: &LaunchPaths,
            _: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, Box<PrepareRefusal>> {
            *self.0.borrow_mut() = Some(LaunchPaths {
                attempt_id: paths.attempt_id,
                operation_id: paths.operation_id.clone(),
                task_index: paths.task_index,
                lane_depth: paths.lane_depth,
                deadline_unix_ms: paths.deadline_unix_ms,
                scratch: paths.scratch.clone(),
                provider_state: paths.provider_state.clone(),
                temporary: paths.temporary.clone(),
                temporary_alias: paths.temporary_alias.clone(),
            });
            Ok(PreparedLaunch {
                limits: Default::default(),
                continuation_expectation: None,
                argv: vec!["fixture".into()],
                environment: vec![],
                stdin: vec![],
                secret_input: None,
                cwd: "/".into(),
                route: route(),
                skipped_routes: vec![],
                observed_head: None,
                repository: None,
                confinement: None,
            })
        }
        fn decode(
            &self,
            _: &RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &Path,
        ) -> std::result::Result<Decoded, String> {
            unreachable!("no provider runs in this test")
        }
    }
    let temp = tempfile::tempdir()?;
    let short = std::fs::canonicalize(temp.path())?.join("short");
    std::fs::create_dir(&short)?;
    crate::short_temporary::use_parent_in_tests(&short);
    let context = context(temp.path())?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context, &call)?;
    // Launch preparation creates the attempt directory exclusively, as
    // dispatch does before it records the attempt.
    let run = input();
    let provider = Capture(std::cell::RefCell::new(None));
    let _ready = store.prepare_fresh_start(&run, &provider, &call)?;
    let borrowed = provider.0.borrow();
    let paths = borrowed.as_ref().ok_or("paths captured")?;
    let attempt = context
        .state_root
        .as_path()
        .join("attempts")
        .join(run.attempt_id.to_string());
    assert!(!paths.temporary.starts_with(&attempt));
    assert_eq!(paths.scratch, paths.temporary.join("scratch"));
    assert!(paths.scratch.is_dir());
    assert_eq!(
        paths.temporary,
        crate::plan::scratch_root(context.state_root.as_path(), run.attempt_id).join("tmp")
    );
    let alias = paths.temporary_alias.as_ref().ok_or("temporary alias")?;
    assert!(!alias.starts_with(&attempt));
    assert_eq!(
        std::fs::canonicalize(alias)?,
        std::fs::canonicalize(&paths.temporary)?
    );
    Ok(())
}
