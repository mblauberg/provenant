//! The declaration channel: one journal, host-bound attempts, field errors.
mod support;
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock as _, Digest, OperationId, ProjectId, RootInputs,
};
use provenant_os::clock::SystemClock;
use provenant_records::{
    CanonicalRequest,
    declaration::{DeclarationError, DeclarationInput, DeclarationReceipt, JournalLine},
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
        store.declare(
            self.attempt,
            &DeclarationInput {
                request_id: id.into(),
                payload: body,
            },
        )
    }

    fn journal_lines(&self) -> Result<Vec<JournalLine>> {
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
    let JournalLine::Accepted(sealed) = &lines[0] else {
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
    assert!(error.dropped.iter().any(|field| field.field == "schema_version"));
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
        format!("{}\n", serde_json::to_string(&JournalLine::Closed)?),
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
    let output = rig.command(&["handoff", "--file", file.to_str().ok_or("file path")?])?;
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
