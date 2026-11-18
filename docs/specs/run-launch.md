# Run launch and admission

Owner: `crates/run`, `crates/confinement`, `crates/providers`, `crates/fabric-runtime`
Anchor: `crates/run/src/launch.rs`, `crates/run/src/admission.rs`,
`crates/fabric-runtime/src/graph/next_attempt.rs`

Paths are relative to `tools/front-door/`.

## Admission and custody

Admission commits the reservation and attempt together. A verified worktree has
at most one writer; resource probes do not hold the shared registry lock or run
inside the transaction. Busy capacity is not a queue. Direct-run custody uses
the same captured project and actor identity as the run store. Uncertain custody
keeps its ownership until release is proved.
- Why: admission must not create duplicate writers or hold coordination locks across external work.

Repository coordination is always a single non-blocking try, including shared
configuration snapshots. A busy repository is a typed transient launch outcome,
never a terminal preparation refusal. `RunStore::start_inner` unwinds preparation,
withdraws unlaunched writer custody, releases repository admission and requeues
the same attempt before returning contention to the graph scheduler. The
scheduler persists capped exponential backoff within the original launch deadline;
cancellation wins before any retry, and only deadline exhaustion blocks the node
with a `launch_retry_exhausted` cause. Preparation inherits the launcher's budget
and cancellation token, rather than creating a fresh wait allowance.
An already-referenced queued attempt is relaunched under the same host authority,
key and persisted retry allowance; a late preparation result cannot strand it as
an indefinitely pending observation. The owner reports a custody-free, never-started
attempt as `AttemptObservation::Queued`; the graph retains `AttemptState::Starting`
and its reference, and persists a bounded next wake within the original launch
deadline without charging a launch failure. `Pending` represents active work and transitions to `Running`.
A missing observation reference never erases a previously saved reference. A newly
admitted queued reference is saved before the scheduler completes launch inline.
If the launch edge still leaves it queued, the owner preserves that observation
and its bounded wake. A queued observation alone does not charge a launch failure.
A future wake defers preparation only. `AttemptOwner::attempt_settled` may prove
that an already-referenced run is terminal and has released custody; that proof
permits observation before the wake without authorising a new launch.
`SettlementPurpose` distinguishes ordinary referenced-run observation from the
cancellation proof for an attached peer subtree; detached work must not make
one purpose stand in for the other.
Fixture launch deferral covers both admission and queued relaunch, so sealing
an attempt never implicitly prepares its provider.
- Why: one sibling's short repository mutation must not fail another writer or
  monopolise custody while waiting for it.

A missing repository Git marker is a structural Git-isolation refusal, including
at the early common-directory open. Other filesystem failures retain their I/O
classification.

Writer-binding I/O failures retain bounded operation, path-role and OS error
metadata in the facade diagnostic and graph advance error. Raw paths, error
messages and Git output are not diagnostic detail. Only classified contention
is retried; unrelated I/O failures remain terminal binding failures.

Graph retry holds the graph journal before it acquires run-store launcher
exclusion, then publication and custody locks. It withdraws a queued attempt
under launcher exclusion before saving the graph reset. A started attempt or
unreleased custody refuses the reset. A withdrawal bearing the retry fence is
idempotent if interruption leaves the graph unreset; retry verifies the same
attempt identity and custody again before archiving it. This includes attempts in
dynamic descendant rows moved into retired expansion records; a missing or
inconsistent destination refuses the reset. No run-store path may wait for the
graph journal while holding those locks.

Lock order is launcher exclusion (`launcher.lock`), publication reservation,
then writer/custody exclusion in `RunStore::hand_off`. Git setup under that
custody may only **try** the common-directory repository lock. In the reverse
direction, worktree mutation and removal may hold the repository lock while
**trying** writer exclusion; neither direction waits for the other. Repository
snapshots release their hold before network work. Every acquired guard and
admission hold is released before the scheduler's backoff, so no retry sleeps
under launcher, publication, writer or repository exclusion. A failed custody or
admission withdrawal remains protected for reconciliation; it is not proof that
retry is safe.

All repository-lock acquisitions share `worktrees::lock_repository`:

| Acquisition owner | Contention disposition |
| --- | --- |
| `prepare_writer_with_context` / writer Git configuration | Launch unwinds and retries within its deadline; settlement preserves pending import and uses bounded recovery backoff. |
| Worktree inventory and creation (`list_impl`, `create`) | Graph provisioning retries; a direct CLI command reports busy for caller retry. |
| Worktree removal, ref retirement, `delete_ref_leased_when`, ownership retirement | No mutation on failed acquisition; CLI reports busy and GC retains for a later sweep. |
| `doctor` shared inventory | Report busy; caller may retry. |
| Recursive submodule custody | Try shared repository ownership, then retain busy work if unavailable. |

Before entering handoff or spawning a guardian, the launcher durably prepares
the per-attempt `guardian.lock` inode alongside the attempt, writer and custody
identities. The guardian acquires and verifies that original inode before
acknowledging custody or spawning the provider, then records its native process
identity. The prepared record is sufficient even if the guardian dies before
publishing its own identity or updating the pre-handoff reservation: the
original free writer and custody leases exclude a live launcher or guardian.
Only the guardian holds the life lock after preparation;
the provider and its descendants never inherit it. The guardian retains it
through terminal publication and custody release. Kernel process teardown
releases it even when the guardian cannot publish an exit.
Tick's settlement driver consumes that proof under writer exclusion and a
fresh worktree occupancy census. A retained or unknown process observation
cannot substitute for the original free locks or positive non-occupancy.
- Why: a stale process checkpoint cannot prove same-boot guardian death; the
  original kernel lock can supply that evidence without PID-reuse heuristics.

A launch is uncertain only once its reservation may record a handoff. A host
that cannot hold provider custody refuses before that point: the attempt ends
with `unsupported_platform` and its custody is withdrawn, never reported as an
unknown launch awaiting a receipt. A launch cancelled or refused before it
spawns needs no custody, so it records `never_spawned` the same way on every
host.
- Why: a launch that cannot start must not look like a lost lane.

Permanent pre-admission refusals are durable outcomes of their operation. Replay
returns the refusal without preparing again; a failure to store it is not
acknowledged as success.
- Why: a lost response must not turn a refusal into another launch.

Terminal attempts are immutable. A retry is a new linked attempt, and a writer
is never replayed over its edits. A terminal attempt has at most one successor.
- Why: continuation must preserve both history and exclusive custody.

## Policy and confinement

Launch policy defaults to refusal only at credentials, Git isolation, data
integrity and authority boundaries. Other checks allow or warn unless configured
otherwise. A write-scope refusal happens before publication and retains private
work. The lane instructions say the same: a write scope lists the expected files,
so a writer edits another file when the fix needs it and lists it in its
handoff; only a strict `WRITE_SCOPE` policy refuses it.
- Why: advisory checks must not become accidental authority boundaries.

Every non-boundary rule lives in `crates/run/assets/policy.toml`, defaults to
`warn` and is `refusable`, so a project can opt into strict mode. Its knob
`policy.<CODE>` is documented under `policy_rules` in
`config/harness-knobs.json`. A consumer calls `Policy::warn_or`, which warns on
stderr naming the rule, the setting source and the knob, or returns the
caller's refusal under strict mode. Boundary rules keep using `boundary_error`.

Every provider lane has its native tool and shell surface. Read-only mode
constrains filesystem writes; it does not remove shell, editing or web tools
from the provider. The common confinement planner owns worktree writes, reads
of the primary and sibling checkouts, explicit read roots, credential denials
and network reach. Network defaults to open, and explicit network reach is
applied by that same planner for every adapter. Every launch finalises the
host Cargo home and lane target binding through the common environment path.
Private harness instruction files receive the canonical global rules compiled
into the binary, so source checkouts and packaged releases use identical content
without depending on the installed asset layout. Native integration differences
belong to `AdapterSpec`, with a reason and source for an unavailable capability.
- Why: provider selection must not silently narrow a lane's working surface.

The selected sandbox floor applies to each candidate and is sealed for
continuations. Unsupported confinement is reported rather than represented as
sandboxed execution.
- Why: a retry must not silently weaken the requested boundary.

Read-only write exceptions come from user-owned configuration, remain within
verified repository paths and are recorded on the attempt. Git and provider
control paths are excluded.
- Why: only the user may widen a read-only lane's write authority.

Host-native launches retain confinement and provenance and cannot resume a
provider session or select model-specific controls.
- Why: native execution must not claim a provider contract it cannot satisfy.

## Inputs and resources

Unreadable required host inputs reject an attempt before launch. A pinned source
view reports its failure without exposing captured contents or credentials.
- Why: a partial source view must not be mistaken for the requested input.

Lane download stores do not copy the host registry, and fresh provider homes do
not carry provider history.
- Why: shared build inputs must not disclose host or provider state.

Build output is collected only when its Cargo locks are free and recent writes
are protected. Priority adjustment failure does not refuse launch.
- Why: resource management must not corrupt active work or prevent a launch.
