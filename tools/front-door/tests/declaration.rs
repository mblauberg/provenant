//! The declaration channel: one journal, host-bound attempts, field errors.
mod support;
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock as _, Digest, OperationId, ProjectId, RootInputs,
};
use provenant_os::clock::SystemClock;
use provenant_records::{
    CanonicalRequest,
    declaration::{
        DeclarationError, DeclarationInput, DeclarationLimit, DeclarationLine, DeclarationReceipt,
    },
};
use provenant_run::{ExecutionContext, PreparedRun, Request, RunStore};
use serde_json::{Value, json};
use support::{Result, TestRig, code};

fn payload(note: &str) -> Value {
    json!({
        "schema_version":2,
        "status":"done",
        "note":note,
        "remaining":[],
        "decisions":[],
        "verify":[]
    })
}

fn proposal() -> CanonicalRequest {
    serde_json::from_value(json!({
        "schema_version":1,
        "task":{
            "context":{"graph_id":"decl","node_id":"node","attempt":1},
            "prompt_digest":provenant_records::CanonicalV1::file_digest(b"private"),
            "route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":null},
            "authority_digest":Digest::from_bytes([5;32]),
            "provider_timeout_seconds":30,"delivery_id":null
        },
        "parent":null
    }))
    .expect("canonical request")
}

struct Fixture {
    _rig: TestRig,
    attempt: AttemptId,
    context: ExecutionContext,
    clock: SystemClock,
    cancel: CancelToken,
}

impl Fixture {
    fn new() -> Result<Self> {
        let rig = TestRig::new()?;
        let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
        inputs.state_dir = Some(rig.state.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let context = ExecutionContext {
            state_root: roots.state.clone(),
            project_id: project(&rig)?,
            actor_id: ActorId::from_bytes([2; 16]),
            read_prompts: false,
        };
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let request = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = RunStore::open(&context, &request)?;
        let attempt = AttemptId::from_bytes([7; 16]);
        store.create(
            &PreparedRun {
                attempt_id: attempt,
                operation_id: OperationId::parse("declaration-fixture")?,
                created_at: clock.wall_now()?,
                request: proposal(),
                prompt: "private".into(),
            },
            &request,
        )?;
        Ok(Self {
            _rig: rig,
            attempt,
            context,
            clock,
            cancel,
        })
    }

    fn store(&self) -> Result<RunStore> {
        Ok(RunStore::open(
            &self.context,
            &Request {
                clock: &self.clock,
                cancel: &self.cancel,
            },
        )?)
    }

    fn declare(
        &self,
        store: &RunStore,
        id: &str,
        body: Value,
    ) -> std::result::Result<DeclarationReceipt, DeclarationError> {
        self.declare_with(store, id, body, &DeclarationLimit::shipped())
    }

    fn declare_with(
        &self,
        store: &RunStore,
        id: &str,
        body: Value,
        limit: &DeclarationLimit,
    ) -> std::result::Result<DeclarationReceipt, DeclarationError> {
        store.declare(
            self.attempt,
            &DeclarationInput {
                request_id: id.into(),
                payload: body,
            },
            limit,
        )
    }

    fn journal_lines(&self) -> Result<Vec<DeclarationLine>> {
        let path = self
            .context
            .state_root
            .as_path()
            .join("attempts")
            .join(self.attempt.to_string())
            .join("declarations.jsonl");
        let bytes = std::fs::read(path)?;
        bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| Ok(serde_json::from_slice(line)?))
            .collect()
    }
}

fn project(rig: &TestRig) -> Result<ProjectId> {
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    Ok(provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
    )?
    .project_id())
}

#[test]
fn a_valid_declaration_gets_a_one_based_receipt() -> Result {
    let fixture = Fixture::new()?;
    let store = fixture.store()?;
    let receipt = fixture.declare(&store, "first", payload("done"))?;
    assert_eq!(receipt.sequence, 1);
    assert_eq!(receipt.request_id, "first");
    let lines = fixture.journal_lines()?;
    assert_eq!(lines.len(), 1);
    let DeclarationLine::Accepted(sealed) = &lines[0] else {
        panic!("expected an accepted line")
    };
    assert_eq!(sealed.sequence, 1);
    assert_eq!(sealed.digest, receipt.digest);
    assert_eq!(sealed.handoff.note, "done");
    Ok(())
}

#[test]
fn field_errors_then_a_valid_correction_keeps_every_rejected_line() -> Result {
    let fixture = Fixture::new()?;
    let store = fixture.store()?;
    let bad = fixture.declare(
        &store,
        "bad-1",
        json!({"schema_version":1,"status":"done","note":"x","remaining":[],"decisions":[],"verify":[]}),
    );
    let DeclarationError::Invalid(error) = bad.expect_err("invalid schema version") else {
        panic!("expected field errors")
    };
    assert!(
        error
            .dropped
            .iter()
            .any(|field| field.field == "schema_version")
    );
    let bad = fixture.declare(
        &store,
        "bad-2",
        json!({"schema_version":2,"status":"elsewhere","note":"x","remaining":[],"decisions":[],"verify":[]}),
    );
    assert!(matches!(bad, Err(DeclarationError::Invalid(_))));
    let receipt = fixture.declare(&store, "good", payload("corrected"))?;
    // Both rejected lines count; the accepted line is the third.
    assert_eq!(receipt.sequence, 3);
    assert_eq!(fixture.journal_lines()?.len(), 3);
    Ok(())
}

#[test]
fn replaying_a_request_id_returns_the_original_receipt_without_a_new_line() -> Result {
    let fixture = Fixture::new()?;
    let store = fixture.store()?;
    let first = fixture.declare(&store, "same", payload("one"))?;
    let replay = fixture.declare(&store, "same", payload("one"))?;
    assert_eq!(replay, first);
    assert_eq!(fixture.journal_lines()?.len(), 1);
    Ok(())
}

#[test]
fn a_request_id_reused_with_different_bytes_conflicts() -> Result {
    let fixture = Fixture::new()?;
    let store = fixture.store()?;
    fixture.declare(&store, "same", payload("one"))?;
    let conflict = fixture.declare(&store, "same", payload("two"));
    assert!(matches!(conflict, Err(DeclarationError::Conflict)));
    assert_eq!(fixture.journal_lines()?.len(), 1);
    Ok(())
}

#[test]
fn a_closed_journal_refuses_every_later_declaration() -> Result {
    let fixture = Fixture::new()?;
    let store = fixture.store()?;
    let directory = fixture
        .context
        .state_root
        .as_path()
        .join("attempts")
        .join(fixture.attempt.to_string());
    std::fs::write(
        directory.join("declarations.jsonl"),
        format!("{}\n", serde_json::to_string(&DeclarationLine::Closed)?),
    )?;
    let refusal = fixture.declare(&store, "late", payload("too late"));
    assert!(matches!(refusal, Err(DeclarationError::Closed)));
    Ok(())
}

#[test]
fn outside_a_lane_the_command_reports_not_in_lane() -> Result {
    let rig = TestRig::new()?;
    let file = rig.root.join("handoff.json");
    std::fs::write(&file, serde_json::to_vec(&payload("outside"))?)?;
    let output = rig.command(&["declare", "--file", file.to_str().ok_or("file path")?])?;
    assert_eq!(code(&output), 2);
    let stderr = String::from_utf8_lossy(&output.stderr.bytes);
    assert!(stderr.contains("not inside a lane"), "{stderr}");
    Ok(())
}

#[test]
fn concurrent_appends_get_distinct_gapless_sequences() -> Result {
    let fixture = Fixture::new()?;
    let threads = (0..8)
        .map(|index| {
            let context = fixture.context.clone();
            let attempt = fixture.attempt;
            std::thread::spawn(move || -> std::result::Result<u64, String> {
                let clock = SystemClock::new();
                let cancel = CancelToken::new();
                let request = Request {
                    clock: &clock,
                    cancel: &cancel,
                };
                let store = RunStore::open(&context, &request).map_err(|e| e.to_string())?;
                store
                    .declare(
                        attempt,
                        &DeclarationInput {
                            request_id: format!("lane-{index}"),
                            payload: payload("parallel"),
                        },
                        &DeclarationLimit::shipped(),
                    )
                    .map(|receipt| receipt.sequence)
                    .map_err(|e| e.to_string())
            })
        })
        .collect::<Vec<_>>();
    let mut sequences = threads
        .into_iter()
        .map(|thread| thread.join().map_err(|_| "thread panicked")?)
        .collect::<std::result::Result<Vec<u64>, String>>()?;
    sequences.sort_unstable();
    assert_eq!(sequences, (1..=8).collect::<Vec<u64>>());
    assert_eq!(fixture.journal_lines()?.len(), 8);
    Ok(())
}

/// Build the project-overridable declaration settings from a host config file.
fn settings_from(
    rig: &TestRig,
    project: &str,
    source: &str,
) -> Result<provenant_cli::declaration::DeclarationSettings> {
    std::fs::write(rig.state.join("config.toml"), source)?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let config = provenant_cli::config_reads::read_for(&roots, "declare")?;
    Ok(provenant_cli::declaration::DeclarationSettings::resolve(
        &config,
        project,
        roots.state.as_path(),
    ))
}

#[test]
fn a_project_limit_overrides_the_host_limit() -> Result {
    let rig = TestRig::new()?;
    let project = project(&rig)?.to_string();
    let settings = settings_from(
        &rig,
        &project,
        &format!(
            "schema_version=1\n[domains.run]\ndeclaration_journal_bytes = 2097152\ndeclaration_relay_seconds = 12\ndeclaration_relay_retries = 2\n[domains.run.projects.\"{project}\"]\ndeclaration_journal_bytes = 65536\ndeclaration_relay_seconds = 3\ndeclaration_relay_retries = 0\n"
        ),
    )?;
    assert_eq!(settings.journal_bytes, 65536);
    assert!(
        settings
            .journal_key
            .contains(&format!("projects.{project}"))
    );
    assert_eq!(settings.relay_seconds, 3);
    assert_eq!(settings.retries, 0);
    assert_eq!(settings.relay_attempts(), 1);
    Ok(())
}

#[test]
fn near_full_policy_off_still_reports_the_bound() -> Result {
    let rig = TestRig::new()?;
    let settings = settings_from(
        &rig,
        "",
        "schema_version=1\n[knobs]\n'policy.DECLARATION_JOURNAL_NEAR_FULL'='off'\n",
    )?;
    assert!(settings.near_full(0));
    let effect = settings.near_full_policy.check(
        "DECLARATION_JOURNAL_NEAR_FULL",
        provenant_records::policy::Facts::violation("three bytes left".into()),
    );
    assert_eq!(effect, provenant_records::policy::Effect::Proceed);
    Ok(())
}

#[test]
fn a_full_journal_names_its_key_and_source() -> Result {
    let fixture = Fixture::new()?;
    let store = fixture.store()?;
    let limit = DeclarationLimit {
        journal_bytes: 1,
        key: "domains.run.projects.p.declaration_journal_bytes".into(),
        source: "host (config.toml#declaration)".into(),
    };
    let failure = fixture.declare_with(&store, "too-big", payload("done"), &limit);
    let DeclarationError::Full {
        key,
        setting_source,
    } = failure.expect_err("journal is full")
    else {
        panic!("expected a Full refusal")
    };
    assert_eq!(key, limit.key);
    assert_eq!(setting_source, limit.source);
    assert!(fixture.journal_lines()?.is_empty());
    Ok(())
}

#[test]
fn the_receipt_reports_bytes_left_for_the_near_full_warning() -> Result {
    let fixture = Fixture::new()?;
    let store = fixture.store()?;
    let large = DeclarationLimit {
        journal_bytes: 16 << 20,
        key: "domains.run.declaration_journal_bytes".into(),
        source: "compiled defaults".into(),
    };
    let first = fixture.declare_with(&store, "same", payload("done"), &large)?;
    let used = large.journal_bytes - first.remaining_bytes;
    assert!(used > 0);
    let tight = DeclarationLimit {
        journal_bytes: used + used / 20,
        key: large.key.clone(),
        source: large.source.clone(),
    };
    // A replay keeps the original disposition but refreshes the remainder
    // from the live journal under the effective bound.
    let replay = fixture.declare_with(&store, "same", payload("done"), &tight)?;
    assert_eq!(replay.sequence, first.sequence);
    assert!(tight.near_full(replay.remaining_bytes));
    assert!(!large.near_full(replay.remaining_bytes));
    Ok(())
}
