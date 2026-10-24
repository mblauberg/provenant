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
