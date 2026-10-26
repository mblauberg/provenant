# Run store and bounded readers

`Service` is the typed execution API shared by CLI and MCP. It owns dispatch
validation and publication, canonical route requests, target resolution,
status, lanes, output, cancellation, continuation and reconciliation. The
edges supply verified identity, configuration and provider preparation, then
render typed errors through their own output contracts. Public durations use
seconds; the store's private timing and receipt formats remain independent.

The store is `run.sqlite3` at the selected `StateRoot`, with strict v1
records shared through `provenant_records::run`. It uses WAL, foreign keys,
`FULL` synchronisation, bounded BUSY retries and one thread per connection.
Unknown database and record versions refuse without upgrading them.

`RunStore::open` binds a verified edge `ExecutionContext`. Each call takes a
fresh clock, wait budget and cancellation token through `Request`. Cancellation
only ends that call. Explicit run cancellation and launch go through
supervisor custody; supervisor owns the OS mechanics.

`create` reserves the operation, run and attempt records in one transaction,
then seals canonical `request.json` and exact UTF-8 prompts through supervisor's
held-directory filesystem API before marking the operation accepted. Replay
compares the canonical request digest, including prompt digests, route choice,
authority-plan digest and execution controls. A prepared operation retries
publication with its original IDs, even when the replay brings fresh ones. It also queues one admission ticket per
attempt and refuses a submission that would exceed the host queue.

`publish_terminal` requires a scoped, durably published supervisor exit record
and completed cleanup. It verifies and syncs that file, seals the result, then
commits the immutable attempt, event, run summary, operation state and released
ticket together. Publication failures remain nonterminal and return typed
errors. Retrying the same publication is idempotent; a different terminal
record is refused. It neither releases supervisor custody nor publishes a
settlement attestation; the guardian holder below does both.

Publication seals `result.md`, then the intended terminal record as
`terminal-pending.json`. If a restarted publisher computes different text or
metadata, it receives `TerminalRecoveryRequired`. `recover_terminal` uses that
immutable intent, the original sealed result and fresh matching supervisor exit
evidence to retry the transaction. It never certifies replacement result text.
A real SQLite `SQLITE_FULL` test verifies that admission,
events and run closure roll back together, then recover from sealed evidence.
For a result-only orphan left by a crash before the intent was sealed,
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
association exists. Work snapshots describe stored nonterminal work only;
`reconcile` observes custody directly, and durable cursor acknowledgement
arrives with the Fabric facade binding.

`read_output` supports sealed prompt and result bytes, and the attempt's
stderr capture, with exact offsets,
EOF, clipping, completeness and digest. Reads verify the whole bounded file
before returning a chunk. Requests above 64 KiB are refused. Prompt/result
files are bounded at 1 MiB/4 MiB; JSON records and event pages at 1 MiB.
A byte cut through a UTF-8 scalar is labelled `utf8-fragment`; invalid stderr
is labelled `utf8-lossy`. Chunks report the total length so the edge can tail.
`lanes` pages attempts by run, task or attempt target, or all nonterminal
attempts, at most 100 per page.
All artefact references are relative to their run root and traverse verified,
private directories without following links.

Tests inject roots and clocks directly and use isolated `/tmp` directories.
The subprocess crash fixture supplies a temporary HOME, state and harness
config paths through supervisor's executor. The CLI already owns the native
isolated `TestRig` in `tests/support`; no second CLI rig is introduced here.

## Dispatch, admission and the guardian holder

`start` hands each queued attempt to its own same-binary guardian. The edge's
`Providers::prepare` resolves the route and its fallback and builds the exact
invocation; a refusal makes that attempt `rejected` while siblings proceed.
Otherwise the attempt moves to `starting` with generation 1 and its applied
route, takes reader custody, reserves publication and launches. A handoff that
does not confirm marks the operation `launch_unknown` for `reconcile`.

`RunHolder` is the guardian's production `Holder`. `admit` runs before spawn
and waits in `RunStore::admit` until host, project and run concurrency, 20 s
host start spacing and the memory and disk floors allow a start, or until the
admission deadline refuses it. `terminal` runs after the sealed exit is
durable: `finish` decodes the sealed stdout capture, commits the terminal
record idempotently. Writers seal an instruction baseline before launch and
quarantine protected changes under the held writer lease before committing an
outcome. Terminal checks also enforce base ancestry and refuse unfinished Git
operations. Incomplete quarantine stays retryable; review-required changes fail
with retained evidence. Run seals the real quarantine manifest, and supervisor's
`publish_release` binds its digest into the settlement receipt so custody can
clear. The holder context carries a `RootSeed`
because the guardian's environment is cleared. The design's "Decisions taken
in steps 2+3" lists the limits and deferrals.

## Controls

`cancel` withdraws queued work or asks the attempt's guardian to stop.
`resume` accepts a continuation of one terminal attempt as a new run, once
per attempt, and `start` hands the prior attempt to `Providers::prepare` so
the exact session is continued. `reconcile` commits durable exits, withdraws
launches that provably never reached a guardian, and reports anything else as
held or uncertain without touching it. `open_runs` lists runs with
nonterminal work for a recovery scan.

Provider preparation receives canonical per-attempt temporary and provider-state
roots once. Each explicit fallback candidate stages files in `tmp/candidate-N`
and keeps history in `provider-state/candidate-N`. Terminal cleanup removes
`tmp/`, including non-empty candidate directories; provider history is kept.
Supervisor lists writer reservations and reports malformed entries without
hiding valid ones; the CLI includes those diagnostics and returns a non-success
reconcile status.

Tests use fake providers and resources at this boundary. The CLI composes the
production provider registry and the `dispatch`, `lanes`, `output`, `cancel`,
`resume`, `reconcile` and `telemetry stats` leaves.

Start creates a private attempt-owned `tmp/` and passes it to provider
preparation as `LaunchPaths::temporary`; every lane stages scratch files in
`tmp/candidate-N`. Terminal commit and withdrawal remove `tmp/` best-effort
through held directories, without following symlinks. Cleanup failure does not
change the committed attempt. `read_only_plan` loads the bound project's `read-only.json`,
applies the host Git binding and selects that temporary root. Start records
the launching plan's relative write exceptions before custody handoff.
Production `Registry::prepare` verifies the read-only repository against the
persisted dispatch project binding and calls the same `read_only_plan` builder.
Git reads and explicit project writes therefore reach both CLI and Fabric
launches. Non-Git workspaces keep their existing directory read grant and have
no repository exceptions.
