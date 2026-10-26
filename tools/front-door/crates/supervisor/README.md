# Process supervision and custody

provenant-supervisor owns provider supervision and custody (see the
[supervisor spec](../../../../docs/specs/supervisor.md)). It depends on
provenant-os for utility execution, capture and concrete filesystem handles.
Custody-specific unsafe code lives in the private process_os module.

The dev-only `fixture` feature supplies self-re-exec scenarios for the
`harness = false` process target. A self dev-dependency enables it for crate
tests; installation builds do not enable it. No additional executable is built.
Process children use temporary HOME, state and harness configuration paths.

## Native boundaries

process_os::census observes native process identity and proves held custody.
process_os::provider owns guardian-local signals, subreaping and adopted-child
cleanup. process_os::handoff passes writer and attempt locks over a bounded
private same-binary channel. Shared child, filesystem and reaper primitives
live in provenant_os::sys.

`machine` is pure; `provenant_os::capture` decodes before bounded retention and
never treats capture failure or silence as a reason to stop. `custody` owns generation-scoped
records, reservation fencing, recovery and release. `internal supervise` is
private descriptor handoff, not a raw launch API. Run composes provider decoding
and its children journal through `GuardianHooks` in its same-binary entry point.
The hidden invocation carries five validated descriptor numbers in argv, rather
than the design's literal no-argument form. Its private socket carries the bounded
launch message; provider arguments, prompt and environment values never enter the
guardian argv. Fixed descriptor slots would add a collision-prone remapping step.

## Custody and recovery

The state reservation directory and verified worktree are different anchors.
`WriterBinding` can only be constructed from confinement-supplied held worktree
and Git handles, a branch and pinned base. Confinement owns their association to
the selected state slot. A private storage DTO reads those facts during recovery.
A durable ownership journal precedes reservation publication; missing reservation
with unresolved journal evidence refuses a new writer. Retries update the
reservation under the same writer lease; the original ownership journal remains
until settlement, avoiding a two-record generation transaction.

Run's trusted permissions service implements `ReconcileAuthority`. This is an
in-process composition seam, not authentication against arbitrary Rust code.
Supervisor checks the scoped decision and durably records actor, evidence and
disposition before accepting a release. Uncertainty alone does not release a
writer, and reconcile never grants numeric-PID signalling.

`SettlementCommit` requires the original attempt inode, its custody lock and
Run-private `release.json`, `terminal-commit.json` and `quarantine/manifest.json`
attestations. All are version 1 and scoped; the release contains both raw-file
digests. Run must write these only after its canonical terminal transaction and
quarantine commit. This explicit release-attestation contract fills a design gap;
The crate neither reads SQLite nor manufactures canonical Run truth. Run must protect
these files from providers. Scratch admission likewise requires a private generated
directory outside the provider sandbox and Run/writer/attempt exclusion against
all legitimate mutators. Mode 0700 alone does not exclude same-UID peers; unknown
or externally writable trees must not be admitted. Only classified unchanged
leaves are removed; captures, history and unknown data remain.

The guardian requires a native boot identity and fails closed if the host denies
it. Dev fixtures inject a recorded boot key explicitly while retaining real
native process observations; production has no environment override. Darwin's
public proc APIs do not prove a particular open file description holds `flock`;
nested custody therefore remains unknown and prevents descendant delivery there.
A future native proof needs an explicit reviewed mechanism, not a guessed flag.

The `WaitBudget`/`Clock`, streams, executor, filesystem and reaper
APIs are reused. No dependency on Run, providers or confinement is introduced.

After bounded explicit cleanup, a durable unresolved reservation and immutable
exit permit a one-way fenced-leader retirement. The child capability is consumed
before reaping only the known-dead leader; no descendant absence is claimed.
Guardian exit then allows recovery to acquire writer exclusion and reconcile,
while the persistent reservation continues to fence every new writer. Keeping
the guardian alive indefinitely would prevent that recovery path. Publication
failure still retains custody and the bounded outcome for retry.

A census cannot freeze process-group membership. Supervision therefore never sends a group
signal: cancellation targets the pinned unreaped provider, and Linux descendant
cleanup targets only kernel-confirmed adopted children. A successful bounded
children-journal observation is required before descendant delivery. This closes
the shared-host/sibling race instead of treating a fresh census as atomic proof.

An empty session census cannot disprove an unrecorded setsid escape. Same-boot
recovery keeps any durable unresolved-cleanup reservation unknown. Linux clears
that bit only after complete census/tracking checks and a stable bounded scan of
all guardian threads' kernel children lists proves only the dead leader remains.
Darwin has no corresponding subreaper proof. Settlement instead requires two
complete censuses with no provider/session/group/history members and a fresh
lock on the same writer inode after closing the guardian's inherited duplicate.
An unknown census, inherited lease or protected nested owner retains the fence.

## Spawn, polling and publication

Provider spawn uses a bounded pre-exec socket handshake. A temporary native
observer thread captures boot/start/PID/group/session identity while the child
waits, then releases exec and joins before spawn returns. Failure refuses exec;
post-exec zombie visibility cannot erase that recorded identity. The guardian
continues cleanup after a reaped leader and retries unavailable censuses until
the existing cleanup deadline, recording a warning rather than giving up once.

Pipe pumping polls at 5 ms. CPU, cancellation and safety census observations
run at 100 ms; process checkpoints publish on the first loop turn and then
at 1 s intervals. Unchanged
checkpoint/reservation bytes skip durable replacement. Full census still runs
on the pipe thread: moving or slowing safety tracking needs a validated design
and remains deferred. Descendant-journal write
failures warn once per failure episode and mark the current census evidence
incomplete. A subsequent successful census replaces that flag with its own
completeness; the bounded exit warnings retain the journal-failure history.
Cleanup census failures likewise warn once per episode. Routine identity and
tracking updates use atomic replacement; the final reservation commit alone uses
the exit publisher's commit hook. Cleanup and publication feed
actual outcomes into the pure machine, including budget-exhausted unresolved
cleanup; unresolved facts publish an exit but never release the writer fence.
Durable reservation/exit retry loops intentionally retain custody without a time
limit; signals cannot waive publication. `process.json` exposes pending exit
publication when storage permits.

`CapturePlan.terminal_ms` is removed from the private same-binary handoff and
launch summary: terminal grace already uses `root_flush_ms`. There is
no old-state or mixed-version reader contract. Capture and cleanup defaults use
the executor/machine constants. Darwin PID listing over-allocates within its
entry bound and retries a full buffer before claiming truncation. Native boot
identity is cached only after a successful read.

Strict `provenant_os::exec::run_text` remains available. The convenience tee
queue is fixture-only. Further pump
and exit-builder extraction, capture-loop consolidation, exit-record shape
consolidation, scratch partial-write handling and staging-file recovery need
separate focused changes; they are not mechanical changes in this audit lane.
