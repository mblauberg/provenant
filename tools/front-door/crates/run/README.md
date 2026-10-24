# Run store and bounded readers

Step 1 supplies `run.sqlite3` at the selected `StateRoot`, with strict v1
records shared through `provenant_records::run`. It uses WAL, foreign keys,
`FULL` synchronisation, bounded BUSY retries and one thread per connection.
Unknown database and record versions refuse without upgrading them.

`RunStore::open` binds a verified edge `ExecutionContext`. Each call takes a
fresh clock, wait budget and cancellation token through `Request`. Cancellation
only ends that call. The crate has no process-launch or signalling path.

`create` reserves the operation, run and attempt records in one transaction,
then seals canonical `request.json` and exact UTF-8 prompts through supervisor's
held-directory filesystem API before marking the operation accepted. Replay
compares the canonical request digest, including prompt digests, route choice,
authority-plan digest and execution controls. A prepared operation can retry
publication with its original IDs. Admission and scheduling land in step 2;
this storage acceptance does not claim a worker or ticket has been acquired.

`publish_terminal` requires a scoped, durably published supervisor exit record
and completed cleanup. It verifies and syncs that file, seals the result, then
commits the immutable attempt, event, run summary, operation state and released
ticket together. Publication failures remain nonterminal and return typed
errors. Retrying the same publication is idempotent; a different terminal
record is refused. It neither releases supervisor custody nor publishes a
settlement attestation. Those belong to dispatch's holder composition in step 2.

Before sealing `result.md`, publication now seals the intended terminal record
as `terminal-pending.json`. If a restarted publisher computes different text or
metadata, it receives `TerminalRecoveryRequired`. `recover_terminal` uses that
immutable intent, the original sealed result and fresh matching supervisor exit
evidence to retry the transaction. It never certifies replacement result text.
If the result was not yet sealed, the publisher must retry with the original
facts and result. A real SQLite `SQLITE_FULL` test verifies that admission,
events and run closure roll back together, then recover from sealed evidence.
For a result-only orphan created before terminal intent was persisted,
`recover_result_only` accepts revalidated terminal facts from the trusted
provider/confinement edge and uses the original sealed UTF-8 result bytes.
It still requires matching durable supervisor exit/cleanup evidence and all
ordinary success checks. If original terminal facts cannot be revalidated,
the orphan remains unresolved; a result file alone cannot establish success.

`AttemptReader` produces Serialize-only terminal facts, including optional
provider timing and unknown route observations. Its iterator snapshots the
upper sequence and reads at most 64 IDs per page, surfacing read/validation
errors. `record`, `run`, `operation`, `events` and `work_snapshot` provide the
other scoped reads. Workspace selection is refused until the confinement
association exists. Work snapshots currently describe stored nonterminal work;
lease/custody observations and durable cursor acknowledgement land with controls.

`read_output` supports sealed prompt and result bytes with exact offsets,
EOF, clipping, completeness and digest. Reads verify the whole bounded file
before returning a chunk. Requests above 64 KiB are refused. Prompt/result
files are bounded at 1 MiB/4 MiB; JSON records and event pages at 1 MiB.
A byte cut through a UTF-8 scalar is labelled `utf8-fragment`. Capture/event
output selection, tail reads and exports land with the controls API in step 4.
All artefact references are relative to their run root and traverse verified,
private directories without following links.

Tests inject roots and clocks directly and use isolated `/tmp` directories.
The subprocess crash fixture supplies a temporary HOME, state and harness
config paths through supervisor's executor. The CLI already owns the native
isolated `TestRig` in `tests/support`; no second CLI rig is introduced here.

The old run Rust modules, pending/Unicode helpers and run differential harness
were already absent on this baseline. Shared Python leaves remain for their
old dispatch, batch, control, provider, Fabric and finaliser importers, as listed
in the execution-run design's deletion inventory. This crate never calls them.

## Step 2 prerequisite audit

Work route: `codex/gpt-6.1-sol@high`, via Fabric. Milestones 2+3 are incomplete.
The accepted design requires supervisor/confinement acceptance of the holder
and writer-reservation protocol before step 2. The current APIs do not yet
compose that protocol:

- Supervisor's sole same-binary handoff starts `internal supervise`, not a
  durable run scheduler. `OwnedLaunch` requires a writer lease even for readers.
- Production `GuardianHooks` supplies stream observation and filtering, but no
  terminal callback that retains custody through Run's terminal commit.
- Confinement's `VerifiedWorktree` exposes verified identity/path and its held
  directories as `dyn SafeDir`; supervisor's `WriterBinding::from_verified`
  requires concrete `Directory` handles. There is no verified binding API
  joining those owners.

The design/API follow-up is a safe scheduler handoff, a holder finalisation
hook with validated scope and retained custody, and a confinement-owned writer
reservation constructor (including a distinct read-only custody path). These
need owner acceptance and native custody/fault tests. The admission and batch
implementation, command wiring and legacy deletion must follow together;
disabled-provider refusals alone would not satisfy the dispatch milestone.
