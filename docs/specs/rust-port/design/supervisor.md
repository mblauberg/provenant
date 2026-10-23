# Supervisor and confinement

Proposal for chair acceptance under [PORTING.md v7](https://github.com/mblauberg/provenant/blob/1b3d48e4c549811ce3636edc30b047f36fd24ada/docs/specs/rust-port/PORTING.md).
Author: `codex/gpt-6.1-sol@high` via Fabric. Source baseline:
`1b3d48e4c549811ce3636edc30b047f36fd24ada`; also reuse the bounded OS APIs in
[PR #1348](https://github.com/mblauberg/provenant/pull/1348), with the corrections
below. Command spellings are proposals for the freeze in `design/cli.md`.

## Purpose

`provenant-supervisor` holds processes, output and kernel locks from launch
through cleanup and durable exit publication. Its pure state machine orders
exit, cancellation and deadlines; its syscall boundary grants authority only
over processes it owns. `provenant-confinement` turns an authorised execution
plan into filesystem, Git, network and provider permissions, checks evidence,
and quarantines instruction changes. Together they prevent a failed write,
a lost client or a mistaken process identity from killing a healthy lane,
freeing a busy worktree or publishing an incomplete result. Worktree management
and retention use these same capabilities. Run owns the run registry, admission,
attempts, status, scheduling, retries and result semantics. `fabric-runtime`
owns identity, coordination and federation routing (R2 as amended by the chair).

## Keep and remove

Keep the custody invariants in the [supervisor spec](../../harness/rust-supervisor.md#structural-guarantees),
subject to v7's abort profile and single Rust implementation:

- Register child observation before spawn, reset inherited signal dispositions
  and masks, observe exit with `WNOWAIT`, clean up while the leader reserves its
  PID/PGID, then consume its handle to reap. Never signal a reused identity.
- Only `ProviderGroup::stop(StopReason)` can signal providers. Its closed reasons
  are explicit cancellation, startup deadline, wall deadline, provider protocol
  rejection, supervisor signal and post-terminal cleanup. Silence, CPU inactivity,
  capture failure, ENOSPC and failed publication cannot construct a reason.
- Natural exit observed in a tick wins over late cancellation/deadlines. Startup
  progress is a provider-decoder event; stderr chatter alone need not count.
  Silence produces an observation, including incomplete/unknown progress, never
  a stalled verdict or an automatic stop. Keep bounded CPU/tree progress probes.
- One writer per registered linked worktree; custody and writer leases outlive
  the requesting MCP call and any provider still writing. Lock files are stable
  inodes and are never unlinked or explicitly unlocked while a duplicate lives.
- Spare registered nested owners only with matching native identity, observed
  ancestry and proof of the held custody description. An unlocked descriptor,
  an environment token or a process name is insufficient. Preserve proven
  sparing through incomplete census; never signal shared code-mode hosts or
  sibling groups. Parents cancel registered children by request, not by PID.
- Publish one terminal record with no-replace semantics after its referenced
  files are durable. Publication returns custody on failure. An I/O warning
  cannot change lifecycle state. A visible link followed by failed directory
  sync means `durability_unconfirmed`, not an absent record to overwrite.

Keep bounded utility execution, native process facts/CWD, descriptor-relative
SafeFs, writer retry evidence, provider permission plans, instruction quarantine,
protected-tip release checks, output installation/digest verification, safe Git
evidence and plan/apply scratch cleanup. Their policy is specified below.

The removal evidence distinguishes an executed command from a read, edit,
search, quoted example or test of that command. Repository callers are design
inputs, not reasons to preserve old wire shapes.

Repository use is concrete: process facts feed provider execution, admission,
lineage, cleanup and Fabric; writer leases feed dispatch/preflight/batch and
worktree removal; custody feeds finalisation, controls and Fabric Git.
Confinement is called by provider execution/preflight, utility output hooks and
agy staging. Git evidence is called by delivery/merged-delivery validation.
Worktree management has CLI, Fabric host/Git and skill callers; cleanup runs
from post-merge/session-close hooks; lane-safety is installed as a pre-tool hook.
Keep scoped merged-worktree pruning: `scripts/hooks/post-merge` calls
`clean --prune-merged --merged-since ORIG_HEAD`, and `clean.py` proves squash
merges with fixed gh reads. Consolidate it under worktree policy below.

Command-field inspection of Claude's Provenant session
`~/.claude/projects/*/1c21e420-fb1c-4a3f-be95-eb42e8489edb.jsonl`
(2026-10-23), paired with tool results, found 44 `scripts/worktree create`,
8 `scripts/worktree remove`, 84 `provenant lanes` and 2 direct lane-guard
invocations. Reads, edits, searches, quoted examples and heredoc bodies were
excluded. The Codex sample
`~/.codex/sessions/2026/10/22/rollout-2026-10-22T19-08-47-01a162de-be27-7163-bdd3-e3f13f105b0f.jsonl`
contains an executed process-table query, not a direct `codex exec` launch.
Neither cited sample establishes cleanup execution; its post-merge hook is
repository caller evidence. Keep native process diagnostics, used by that
query and required by custody, without preserving ps formatting. No absence
claim from this sample deletes a safety control.

| Remove or consolidate | Evidence and replacement |
| --- | --- |
| Python observer process, opaque staged bundles, duplicate canonical/legacy attempt files and digest sidecars | These are internal seams in `provider_exec.py`, `dispatch_run.py`, `run_lineage.py` and `lane/observer.rs`. All owners become Rust; Run's decoder sends typed events and records supplies one canonical terminal record. Retain the publication invariant, delete the transport. |
| `Drop`-driven provider stopping and unbounded cleanup loops | `lane/group.rs` stops and polls in `Drop`; abort/SIGKILL bypass it. Explicit guardian ownership and a detached reaper replace it. A bookkeeping error must not trigger a destructor kill. |
| Python/Node Unicode, locale-start strings, ps layout and subprocess error emulation | Existing supervisor `pending`, `ps`, `fabric_ps`, and confinement `python`/`pending` implement runtime parity. Native integer identity and typed errors serve the actual safety decisions; v7 R0 removes parity as a requirement. |
| Whole-run retention deletion | `clean.py` currently deletes attempt bulk and old runs. That can erase provider captures, receipts and transcript references. Retain only generated scratch cleanup; transcripts and telemetry have no deletion operation here. |
| `worktree remove --force`, automatic dependency-tree cloning/linking | `worktree.py` can bypass live-writer checks and provision Node modules. A Rust product needs neither Node bootstrap nor a live-writer override. Keep clean linked-worktree creation/removal and merged-branch pruning only within supplied authority. |
| Standalone writer lease, launch/exit wrappers and Git-evidence packet wrapper | `writer_lease.py`, `lane_launch.py`, `lane_exit.py`, `git_evidence_packet.py` duplicate decisions now shared by Run, confinement and workflow. Preserve their safety checks in library calls; workflow owns review receipt contents and Fabric owns the landing lease. |
| Cross-session transcript scanning and caller-supplied terminal-report CLI | `worker_liveness.py` correlates newest transcript by cwd and accepts an exit status from its caller; multiple workers make that correlation ambiguous. Keep native repo-scoped PID/start/CWD/CPU diagnostics, also consumed by custody and the documented direct-CLI fallback. Drop old-root transcript correlation under R1 and require owned wait evidence for managed terminal reports. |
| System-ps replacement CLI | `bin/ps` and `ps.mjs` exist to work around process inspection in the old Python/Node paths. Internal callers use native ProcessObserver/CwdObserver; no general ps formatting/option emulation is needed. |
| Arbitrary-PID/tree signalling from `reap` | `reap.py` rechecks a start string before `kill`, which does not reserve identity between check and signal on macOS. Replace it with recovery/cancellation by managed run ID; external workers must enter through Run to obtain custody. This is a deliberate restriction even where the old command was used. |

## Process and permission design

`Run owner -> attempt guardian -> provider` preserves S2's custody. The guardian
is a short-lived invocation of the same binary in its own session, with Run's
provider decoder composed beside supervisor; supervisor imports neither Run nor
providers. A private, bounded launch channel transfers the plan and duplicated
writer lease, with an ownership acknowledgement before the parent disconnects.
Secrets use memory/pipes and the child environment, never argv or disk. Hidden
`internal supervise` accepts only this handshake, not a public raw launch spec.
MCP cancellation stops a wait; explicit Run cancellation writes a scoped request.

The guardian keeps the provider, attempt lock and writer-lease duplicate after
a Run owner or MCP host dies, and publishes `process-exit.json`. The Run owner
holds its lease through retries and settlement. If it dies, the reservation
remains until recovery acquires the same writer exclusion and commits quarantine
and the canonical terminal bundle; a process-exit file alone never frees a
writer. If the guardian also dies, a persistent writer
reservation fences acquisition until provider/group absence is proved. Free
flock plus missing/unknown identity is insufficient, including death between
spawn and PID publication. An unresolved reservation can be cleared after a
changed boot identity proves the old processes gone; there is no force bypass.
Provider exec is gated on the guardian holding both leases and the durable
pre-spawn reservation; a missing handoff acknowledgement is an unknown launch,
never permission to launch again. Only guardian handoff may inherit duplicate
lock descriptions. Provider exec closes every custody descriptor; no provider
receives an unlockable writer lease. Run records reservation ownership across
attempt generations and clears it only after the final settlement, not between
retries. Unresolved descendant custody keeps the worktree fenced even after
the leader has exited.

The pure machine has Prepared, Running, TerminalGrace, Stopping, Cleaning,
Exited and Published phases. Each OS-loop turn observes `WNOWAIT` before stop
requests. Defaults are 500 ms descendant grace and 2 s root flush; Run supplies
bounded startup/wall/terminal budgets. Signal errors remain failed deliveries.
Linux subreaping and process-wide signal setup run only in the guardian;
macOS uses kqueue plus SIGCHLD. Explicit cleanup precedes consuming reap; abort
or `Drop` is never a provider cleanup mechanism.

Reuse S2's machine/census/capture, SafeFs and #1348's `Executor`, raw outcome,
strict text decoding, `OwnedPid`, CWD, append/lock and marker APIs. Correct
#1348 before its consumers land:

- Each stream is `Capture { limit }`, `Discard`, or `Tee { limit, sink }`.
  Discard has no pipe; Tee uses a bounded nonblocking queue. Full/failed sinks
  record loss without stalling capture. Remove discard booleans plus optional
  sinks. Defaults: 8 MiB retained per stream, maximum 64 MiB, frame cap 64 KiB.
  Decode before retention truncation; a terminal frame beyond the cap matters.
  An oversized protocol frame is explicit decoder loss, never a fabricated
  terminal event or an I/O-derived stop request.
- `timed_out` means a deadline caused TERM to be delivered to a still-owned
  child. Natural exit followed by pipe-drain deadline sets `drain_truncated`,
  not `timed_out`; overflow has a separate cause. Preserve Exit versus Signal,
  byte totals, truncated frames and decoder-queue loss.
- Darwin CWD `proc_pidinfo == 0` means Gone without consulting stale errno.
  Retry negative EINTR; distinguish permission, unsupported, short-record and
  malformed-path failures. CWD never grants signal authority.
- Utility final-wait exhaustion transfers its owned Child to a detached reaper
  thread and returns `CleanupPending`. One bounded per-process queue reserves
  capacity before spawn and polls children fairly; admission refuses when full.
  No unbounded `Drop` wait or discarded Child. Providers remain in the guardian;
  utility handles have direct-child authority only.
- Move all syscalls/unsafe from `src/lane/{mod,group,platform,files}.rs` into
  `supervisor::sys`, including `Command::new`. Safe provider/utility handles
  expose neither raw owning descriptors nor numeric-PID grants.

Lock acquisition takes an explicit monotonic deadline and cancellation token.
Poll `flock(LOCK_NB)` through the safe lock API, waiting only up to the lesser
of the next poll and remaining deadline; retry EINTR within that budget. An
expired deadline returns `LockTimeout`, cancellation returns `Cancelled`, and
observation failure stays unknown. Never enter a blocking flock syscall. A
zero budget makes one nonblocking attempt. A deadline bounds acquisition, not
lease lifetime: expiry never revokes custody, steals a lease or unlinks its
stable inode. Lock ordering is run custody, repository metadata, writer
reservation, attempt publication, then children journal; recovery never holds
a later lock while waiting for an earlier one. Metadata mutators probe the
writer nonblockingly and release the metadata lock before any retry wait.
No SQLite transaction spans a lock wait.

Confinement compiles `ReadOnly` or `WorktreeWrite(VerifiedWorktree)`, explicit
read roots, network policy and named capabilities. Hold verified path anchors;
reject traversal, symlinked state/lock leaves, credential roots and broad home
or tmp grants. Writers may change their linked tree, private Git index/HEAD,
objects and exactly their branch/ref lock/reflog. Keep `.git`, `commondir`,
`gitdir`, `config.worktree`, `objects/info`, other refs, provider configuration
and protected instructions immutable. Read-only execution uses declared reads
and private scratch. Browser/Postgres IPC is explicitly granted.

Provider profiles contain fixture-proven runtime-state/authentication exceptions;
only the selected account mechanism and own continuation transcript are allowed.
No unrelated credential homes or other sessions. Compose provider-native
controls with macOS sandbox-exec when needed; unsupported/nested confinement
fails prelaunch as `ConfinementUnavailable`, never silently broadens access.
Linux fake-provider tests do not establish real-provider confinement: a retained
profile needs an enforcing backend and fixtures or its mode is refused.

Confinement also owns bounded disclosure scanning (10,000 files/64 MiB) and one
streaming redactor for known secret values, credential URLs and token patterns.
Prompt-training routes refuse protected prompt/cwd/read-root overlap.
Budget exhaustion refuses disclosure; an exception requires Run's explicit
scope-bound authority, never text in a prompt. Chunk overlap prevents boundary
leaks. Redact before captures/diagnostics/JSON reach disk; raw protocol bytes
stay only in bounded memory. Never persist the inherited environment. Redactor
failure discards affected output and records a content-free error.

After confirmed quiescence, quarantine holds the writer lease and archives
protected disk/index/HEAD patches, blobs and digests before restoration.
Automatically restore only attributable ordinary `.agents/` edits. Policy or
control-file changes, concurrent edits or unaccountable history require review;
retain evidence and publish a terminal failure, without resetting unrelated
work or rewriting history. Protected-tip verification returns a pinned SHA;
workflow/chair authority separately releases that exact tip.

Safe Git uses a trusted executable, typed operations, literal pathspecs after
`--`, bounded execution and a sanitised environment: remove Git redirection and
config injection; disable hooks, fsmonitor, textconv, external diff, pager and
lazy fetch for evidence. Verify common/private directories and registration;
copied gitfiles and alternate object stores cannot vouch for protected history.
Pin base/head OIDs once, retain NUL status, selected paths, diff digest and
before/after tracked fingerprints. Selected untracked content, changed
HEAD/index/content, timeout or truncation refuses complete evidence. Output
installation uses held-parent paths and verifies digest/device/inode.

Create/checkout/remove/prune share a repository-scoped worktree/ref lock;
writer admission also holds it while verifying registration and publishing its
reservation. Hold it through removal and ref deletion, including a fresh Git
worktree census and checked-out-branch refusal. This serialises managed
cross-worktree checkouts; it does not grant authority over unrelated Git actors.
Worktree removal/pruning holds writer exclusion, rechecks reservations and
repo-scoped native worker observations, and refuses live or unknown candidates.
Pruning requires explicit merged-ref authority and a pinned integration tip.
Ancestry proves ordinary merges; squash proof needs a recorded merged PR in
this repository whose exact head OID equals the current branch tip and whose
merge OID is an ancestor of the integration tip. `--since OID` selects only
merges reachable from the pinned integration tip and not reachable from that
pinned bound; require the bound to be an ancestor of the tip. This graph delta
also scopes ancestry-proven branch tips. gh failure/malformed/incomplete output keeps
the branch unless ancestry alone proves it. Never prune the integration branch
or a branch at its tip. Recheck the branch OID and remove the ref with that
expected old OID, so a new commit invalidates pruning. CLI's post-merge hook
passes only its just-merged scope; it cannot trigger an unscoped repository sweep.

## Commands and MCP

This is a proposal for `design/cli.md`; `--json` is available on public commands
except the external hook protocol. CLI owns parsing/rendering. No raw launch-plan, secret or PID-targeting
command is public. This area adds no MCP tools: Fabric's status, cancellation,
output and execution tools call the library API through Run. Fabric's landing
lease is separate from the worktree writer lease.

| Proposed command | Flags and result |
| --- | --- |
| `worktree create NAME` | Exactly one of `--branch BRANCH` or `--existing-branch BRANCH`; `--base REV` for a new branch, `--repo PATH` defaults to cwd. Creates under the repository's verified `.worktrees/`; returns path, branch, base OID and warnings. No Node provisioning. |
| `worktree list` | `--repo PATH`; rows contain path, branch/head, dirty state and writer state `free/live/unknown`. |
| `worktree check [NAME]` | `--repo PATH`; registration, protected Git metadata and primary/linked-worktree checks, with refusals and repair guidance. |
| `worktree verify NAME --base OID` | `--repo PATH`; requires clean descended HEAD, new work and valid protection/release evidence; returns the pinned head SHA and evidence digest. Workflow attaches its claim/receipt requirements. |
| `worktree remove NAME` | `--repo PATH`; refuses primary, dirty, live or unknown writers. Removes a managed linked worktree; optional `--prune-merged-into REF` requests authorised branch pruning with ancestry to that pinned integration tip. Otherwise keeps the ref. No force flag. |
| `worktree prune --merged-into REF` | `--repo PATH`; requires exactly one scope: `--branch BRANCH` or `--since OID`. Applies the merge proof above only to clean managed trees in that scope. Rows report removed tree/ref, retained reason and evidence OIDs. Authority must cover each deletion. |
| `clean plan` | `--repo PATH`, `--older-than DAYS` defaults to 7; saves a scratch-only plan and prints ID, expiry, candidates, retained categories and allocated-byte estimate. |
| `clean apply PLAN_ID` | `--repo PATH`; checks saved digest, scope and 24-hour expiry, then rechecks each candidate under custody. Returns deleted/skipped/refused rows and reclaimed bytes. It never removes a complete run directory. |
| `run workers` | `--repo PATH` defaults to cwd; native facts for managed and observed direct Codex workers, with wrapper deduplication and launch `--cd` resolution. Rows show PID/start/CWD, CPU time and `managed/unmanaged/unknown`; never expose full argv or read old transcripts. No mutation, kill or caller-supplied terminal status. |
| `run reap RUN_ID` | `--wait-ms N` defaults to 5000, maximum 60000; recover a dead managed owner, or report live/unknown without signalling. Uses Run's settlement callback; no wildcard, PID or process-tree selector. |
| `run cancel RUN_ID` | Run-owned proposal: `--wait-ms N` with the same bounds; durable scoped request, optionally wait for terminal publication. Returns `requested/terminal/unconfirmed`; a timeout is never confirmation of death. |
| `git evidence --base REV --path PATH...` | `--repo PATH`, required `--output RELPATH` below the project evidence directory; returns packet path, pinned head/base and digest. Paths are literal; base is mandatory. |
| `hook lane-safety` | At most 1 MiB stdin JSON in the harness's Bash pre-tool protocol; no `--json` override. Allow/non-Bash is empty stdout/0; deny is the harness decision JSON/0. Deliberate policy change from the old guard's fail-open parser: malformed/oversized input emits the harness's blocking exit 2 and content-free stderr. Record harness handling before enabling this policy; CLI/hooks owns installation and other mappings. |

Human output is a compact table or one result sentence, followed by content-free
warnings/repair guidance on stderr. JSON stdout is one versioned object:
`{schema_version:1, command, outcome, data, warnings}`; errors use
`{schema_version:1, command, error:{code, message, fix}}`. No diagnostics share
JSON or MCP stdout. Common exits proposed for the freeze: 0 completed (including
an empty plan or confirmed already-gone run); 2 invalid arguments/record; 3
policy refusal; 4 contention/live/unknown custody; 5 OS, durability or evidence
failure; 6 bounded wait ended unconfirmed. Per-row skips are data; partial apply
with a refusal/failure returns 3/5. Provider exit status remains record data,
not the supervisor command's exit code. Private guardian errors distinguish
pre-spawn refusal from custody-retained publication failure.

Keep lane-safety's prohibition on name/pattern-based kills (`pkill`, `killall`,
pattern-fed kill/xargs) and worktree cleanup racing an owned writer. Its bounded
shell-token inspection is advisory defence around direct harness commands;
OS confinement and owned handles enforce provider isolation. Unknown/too-deep
shell constructs cannot be certified safe for destructive operations. There
is no claim to parse all shell languages or infer authority from a prompt.

## Records and state

State/config supplies `StateRoot`, proposed as
`${XDG_STATE_HOME:-$HOME/.local/state}/provenant/v2`. This area receives a
verified capability for that fresh root; the final spelling belongs to
`design/state.md`. It never opens, imports, migrates or deletes old `.agent-run/`,
`~/.agents`, transcript or telemetry roots. Worktrees are repository resources,
verified through Git; new metadata lives under StateRoot. There is no adoption
of a pre-rewrite run or writer reservation.

```text
$STATE/projects/<project-id>/
  worktrees.lock                  # repository worktree/ref mutations
  worktrees/<worktree-id>/{identity.json,writer.lock,reservation.json}
  runs/<run-id>/
    custody.lock
    children/{journal.lock,<child-id>.json}
    attempts/<attempt-id>/
      custody.lock
      launch.json                 # redacted plan summary
      process.json                # replaceable observation
      process-exit.json           # immutable process outcome
      cancel.json                 # durable, attempt-bound request
      stdout.log,stderr.log       # bounded redacted captures, retained
      provider-state/             # retained runtime history, never cleanup bulk
      reserve                     # 64 KiB publication headroom
      quarantine/                 # retained patches/blobs and manifest
      scratch/                    # only disposable attempt subtree
  cleanup/{<plan-id>.json,<plan-id>.result.json}
  evidence/<packet-id>/            # retained Git evidence
```

Run owns its canonical attempt/result bundle and run terminal record adjacent
to these files, with paths agreed in `design/execution-run.md`; Fabric indexes
IDs without copying lifecycle truth into another file. Telemetry and provider
transcripts are distinct retained categories even when located beside a run.
No recursive retention delete is permitted above `scratch/`. Only generated
sandbox profiles and disposable launch staging go there. Provider homes that
can contain history/transcripts live in retained `provider-state/`; scratch
never hosts transcripts or telemetry, including provider-generated files.
If a scratch entry contains such data or cannot be classified, retain it.
Credentials must never be copied into either subtree. Managed worktree deletion
is a separate explicit operation.

All records use records' serde envelopes with `schema_version: 1`; refuse
unknown versions and unknown fields for launch, cancellation, reservations,
release evidence and cleanup plans. Read-only observations permit added fields
within v1; writers emit only their declared version. JSON is bounded to 1 MiB
except the ephemeral launch message (2 MiB); process exit is at most 16 KiB with
omission counts. IDs and relative paths are validated newtypes; OS paths remain
`PathBuf`/`OsString` until the records boundary. Non-text OS bytes use records'
explicit byte-path encoding, never Python surrogate emulation. Durations,
byte counts, native start stamps and clock values are integers.

| Record | Required contents and write rule |
| --- | --- |
| `ProcessIdentity` | Host/boot identity, PID and native start stamp (Linux ticks; Darwin seconds/useconds), plus group leader identity. Wall-clock display time grants no authority. Absence, permission denial and unsupported observation remain distinct. |
| `LaunchSummary` | Run/task/attempt IDs, exact resolved route, executable identity, permission-plan digest, cwd/worktree identity, environment names, stream policies and budgets. Redact sensitive argv values; omit prompt and environment values. Atomic replace before spawn where possible; it is never replayable launch authority. |
| `ProcessObservation` | Guardian/provider identity, phase, last output/CPU/tree observations, completeness, capture totals, custody state and bounded errors. Atomic replace; stale observations cannot establish death. |
| `ProcessExit` | Exit/signal or never-spawned cause, stop request and delivered signals, terminal observation, reaped/spared identities, capture/decoder loss and prior I/O warnings. No-replace, file+parent fsync; retain custody for retry on failure. Its own publication durability is an API/observation outcome, never a self-certifying field in immutable bytes. |
| `WriterReservation` | Worktree device/inode, verified Git location, branch/base OID, run/attempt IDs and guardian identity, launch generation and unresolved-cleanup bit. Atomic publication under writer lock before spawn; clear only after quiescence, quarantine and canonical Run terminal commit under the writer lease. A missing/malformed record with conflicting run evidence refuses acquisition. |
| `CancelRequest` / `ChildRegistration` | Request ID, target run/attempt generation, reason and requester; child run ID/token and parent attempt. Atomic no-replace request/registration; journal lock orders child registration against parent's terminal commit. Stale generations cannot cancel a later attempt. |
| `QuarantineManifest` / `GitEvidence` | Pinned OIDs, selected byte-paths, before/after fingerprints, file digests, view/restore status. Evidence files durable before no-replace manifest publication; no claim of success on partial capture. |
| `CleanupPlan` | Project/root identity, policy version, creation/expiry, candidate relative paths, device/inode/type/mtime/size, eligibility proof and canonical digest. Atomic no-replace; apply verifies digest and reacquires/rechecks custody immediately before deletion. Results atomically replaced, idempotent absent candidates reported. |

All authoritative writes use held-directory SafeFs capabilities, exclusive
0600 temporaries in 0700 owner directories, full writes, file sync, atomic
rename/no-replace publication and parent sync. Lock/custody leaves must be
regular, single-link, owner-controlled files. Following reads are explicit
#1348 `ReadOptions` operations for trusted inputs, never custody opens. A
candidate directory writable by another live actor cannot be made safe with a
check-then-unlink; cleanup refuses it. Apply holds run/worktree exclusion through
its no-follow walk, refuses mounts/links/root replacement and skips drift.
Default 7-day retention applies only to disposable scratch after terminal
publication and confirmed quiescence. Keep unknown, active, malformed and
unclassified entries; age alone is never eligibility.

Before spawn Run checks its disk-admission floor; supervisor allocates a 64 KiB
reserve. On publication ENOSPC, unlink the reserve and retry once, with no
provider signal. If storage still fails, retain custody and the bounded outcome
in the guardian, return publication-pending to the caller and retry on bounded
backoff. Recovery never fabricates a provider exit from silence or a missing
file. Complete evidence cannot be guaranteed after simultaneous process loss
and permanent storage failure; report unconfirmed and fence the writer rather
than publish invented terminal success.

## Library boundary

Proposed signatures use core/records newtypes and typed `thiserror` failures
(Refused, Busy, UnknownIdentity, InvalidRecord, Cancelled, CleanupPending, I/O).
CLI/MCP owns rendering. Supervisor depends on core/records only; confinement
adds config and supervisor's safe OS API. No library imports cli.

| API | Contract and consumer |
| --- | --- |
| `machine::step(State, Event, Tick) -> (State, Vec<Effect>)` | Pure OS-driver decisions; explicit clocks and delivery outcomes. No I/O/silence conversion into a stop. |
| `launch(LaunchSpec, AttemptCustody, WriterLeaseHandoff, Observer, Deadline) -> Result<LaunchHandle, LaunchFailure>` | Run starts a guardian with acknowledged handoff of attempt custody and its writer-lease duplicate; Run retains run custody and its writer lease. Failure returns untransferred handles or an unresolved-handoff token; never silently drops them. Handle is a control/observation channel, never a PID grant. Fabric launches through Run. |
| `observe(AttemptRef, Deadline) -> Observation`; `wait(LaunchHandle, Deadline)` | Run/Fabric read Live/Exited/Unknown and publication state; losing a channel never releases custody. |
| `request_stop(AttemptRef, Cancel, Generation)` | Durable attempt-bound request through Run; only the guardian calls `ProviderGroup::stop(StopReason)`. Acknowledgement does not prove death. |
| `acquire_custody(RunRef, Deadline, Cancel)`; `acquire_writer(VerifiedWorktree, ReservationPermit, Deadline, Cancel)` | Opaque ownership, controlled duplicate handoff, lock plus reservation checks. Run owns run/writer custody; fabric-store uses the same generic lock mechanism for its own locks. No writer TTL takeover. Graph permits come from Fabric; no supervisor SQLite access. |
| `recover(AttemptRef, Settlement, Deadline)` | Run composes observation and publication exclusion; detached recovery may outlive its caller. Two recoverers cannot commit twice or signal foreign PIDs. |
| `exec::Executor::execute(ExecOptions)`; `run_text`; `pid::{OwnedPid,CwdObserver,ProcessObserver}` | #1348 raw bounded utility execution and optional strict UTF-8; separate stream enums and direct-owned-child authority. Native process/CWD observations grant no signal authority. |
| `fs::{SafeFs,SafeDir,SafeFile,PathIo}`; `lock(Mode, Deadline, Cancel) -> LockGuard` | Config, confinement, fabric-store and record-persistence callers use held-directory bounded/following reads and append/lock capabilities. Locks use nonblocking polls; custody guards expose controlled duplication/close, never public unlock of a shared description. Callers own path policy. |
| `publish(SafeDir, Name, Bytes, PublishMode::{Replace,NoReplace}) -> Publication` | File sync, atomic publication, then directory sync. Replace is for observations/managed files; NoReplace for terminal markers/evidence. Return `Durable`, `VisibleDurabilityUnconfirmed`, or a typed pre-publication error; conflict never replaces the existing file. Reconcile matching generation/digest and retry sync after a visible publication; retain custody until durable. |
| `confinement::run_utility(UtilityRead, VerifiedRepo, UtilityBudget, Cancel) -> Result<ExecOutcome, UtilityError>` | Workflow's fixed Git and gh reads, through the supervisor Executor. Closed operations build argv: Git status/rev-parse/diff/log/worktree and merge-base ancestry reads; gh PR/issue/check reads with validated repo/IDs and fixed JSON fields. No shell, arbitrary argv, arbitrary API path or write verb. Budget supplies wall/grace/reap deadlines and per-stream caps; truncation cannot establish evidence. Policy sanitises the environment and pins executables. Secret auth stays out of argv/logs. |
| `confinement::compile(PlanPermissions, ProviderProfile, Anchors) -> ConfinedLaunch` | providers/Run receive enforced wrapper/environment, availability result and plan digest. No execution or stop authority. |
| `verify_worktree`; `Git::evidence`; `verify_tip`; `quarantine`; `OutputCustody::{install,verify}`; `scan_disclosure`; `Redactor` | Run/workflow consume verified Git/path capabilities, pinned evidence and safe outputs. Workflow supplies explicit protected-tip release authority. |
| `confinement::worktrees::{create,list,check,verify,remove,prune}`; `retention::{plan,apply}` | CLI/workflow/Fabric services; supervisor supplies OS/lock mechanisms, confinement supplies policy. No new crate. |

Required from state/config: fresh verified StateRoot, IDs, integer clocks,
byte-path encoding, canonical digests, bounded versioned serde records and
resolved policy/profile/retention settings. Help/root resolution starts neither
OS driver nor database. Records owns content; supervisor owns atomic effects.

Confinement's utility facade preserves R2: workflow depends on confinement,
which depends on supervisor; workflow does not gain a direct supervisor edge.
Mutating Git worktree/ref operations use separate typed confinement services
with explicit authority. A utility handle cannot stop a provider or a sibling.
Run composes confinement's output filter with supervisor's injected capture
filter trait, so supervisor never imports confinement. Decode bounded protocol
frames in memory before redacting retained bytes.

Required from providers: argv/environment plus selected authentication inputs,
a fixture-proven permission profile and bounded decoding into Progress,
Terminal or closed ProtocolRejected events; no Python observer or signalling
callback. Run (`design/execution-run.md`) owns the run registry, admission,
attempts, status, retry generations, child registration, cancel scope, settlement
and the single terminal bundle.
It commits referenced evidence before its terminal marker and returns custody
on error. Dirty/changed/unreadable writer evidence prevents automatic retry;
silence never proves an untouched tree.

`fabric-runtime` (`design/fabric.md`) owns identity and federation routing,
supplies graph reservation permits and consumes ID-scoped registry/status/output/
cancel through Run. Its store may index Run IDs but owns no competing run
registry or lifecycle status. Transport cancellation
only ends its wait. Workflow supplies integration ref, authorised branch pruning
and protected-tip releases. No area independently probes then kills a PID or
infers authority from a file containing a name. The generational run-driver
lease (`lease.py`) and Fabric landing lease remain their owners' protocols;
neither substitutes for kernel writer custody.

Replace direct-PID reap guidance in `docs/runbooks/shell-pitfalls.md` and
`skills/orchestrate/references/{lane-safety,direct-cli-fallback,worker-liveness}.md`
with Run-managed launch/cancel. An observed unmanaged worker is diagnostic only;
its owning harness controls cancellation. Provenant neither adopts it by PID
nor pretends a diagnostic row gives it custody.

## External contracts and fixtures

Provider wire forms, harness hooks, SKILL.md, MCP and Git remain fixed under R0.
Our launch/receipt/CLI formats change freely; no Python/Node oracle or reader.
The external hook/Git/OS recording directories below do not yet exist at this
baseline: they are required build inputs, not evidence already collected.

| External contract | Fixture and assertion |
| --- | --- |
| Provider argv/env, streams, results and continuation | Sanitised real streams in `tests/fixtures/fabric-context/{claude,codex,cursor,opencode,agy,kiro}.jsonl` seed decoder tests. Providers owns version/provenance manifests and missing launch/permission/continuation recordings, including Copilot if retained. Assert typed events beyond capture cap and no credential persistence. `fabric-v1/provider-*.json` are owned receipts, not provider-wire recordings. |
| Harness Bash pre-tool hook | CLI records real tool/session/cwd stdin and allow/deny stdout/exit under `tests/fixtures/external/hooks/`. `test_lane_safety.py` supplies scenarios, not recordings. Assert broad-kill denial, malformed-input handling and nesting bounds. Other harness mappings/installation stay with CLI. |
| Git porcelain/OIDs/worktrees/ref/index/patch forms | Capture system-Git version plus raw NUL worktree/status/diff/index output under `tests/fixtures/external/git/`, including SHA-1/SHA-256 and linked trees. Also run temporary-repository tests for evidence races and redirects. |
| SKILL.md and MCP | records owns R6's tracked frontmatter subset fixtures; confinement protects instruction bytes without another YAML reader. fabric-mcp owns recorded JSON-RPC 2.0 stdio/cancellation fixtures; integration proves clean stdout and retained custody after cancelled waiters. |
| OS and sandbox interfaces | Linux/macOS handshake fixtures record platform/backend and operation allowed/denied, not generated SBPL text equality. Fake providers cannot certify a real profile. Missing external recordings block that profile's support; live-provider capture needs user authority. |

## Tests and acceptance

Follow [R9](https://github.com/mblauberg/provenant/blob/1b3d48e4c549811ce3636edc30b047f36fd24ada/docs/specs/rust-port/PORTING.md#r9-tests-prove-the-new-design): beside-module unit tests
use explicit clocks and injected observations. Cover transitions/idempotence,
natural-exit precedence, unknown identities, failed signals, stale generations,
registration versus publication, integer/size bounds, secret chunk splits,
path aliases, and cleanup expiry/digest/drift. Delete tests of shapes that the
new enums, capabilities and compiler make impossible.

Binary integration drives public commands and MCP with a Rust fake-provider
fixture and isolated fresh state root. Cover launch/wait/cancel/recover,
worktree contention, disclosure, Git evidence, quarantine and cleanup; old-root
sentinels must remain untouched. No Python adapter, selector or old-code oracle.
Add a test-only `process-fixture` binary target under the binary package's test
support, excluded from installation and release products. It implements FIFO/
socket handshakes, fake provider modes and lock holders. A narrow test-module
`#[allow(clippy::disallowed_methods)]` with a reason permits `Command::new`
only to start this fixture or the built `provenant` in multi-process tests;
it grants no production exception or new unsafe-code location. Process signals
and descriptor setup still use supervisor's safe test APIs. Production has one
binary. Test fixed Git/gh argv and refusal of arbitrary operations, lock
deadlines/cancellation, and both publication modes including post-rename sync
failure.

Linux and macOS process tests use FIFO/socket handshakes and observed EOF,
not sleeps. Rebuild the custody scenarios from `tests/lane_conformance/` and
`test_issue_697_custody.py` before deleting their old subjects:

- Fast exit before watch; inherited ignored SIGCHLD/masks; prelaunch, live and
  late cancellation; silence with/without writer progress; terminal stdin EOF;
  cap followed by terminal frame; decoder/sink failure; PID reuse.
- TERM-resistant provider, setsid grandchildren, nested owners, unlocked-lock
  impostor, shared hosts/siblings and incomplete census. Parent/MCP death leaves
  the guardian and lease live; double-owner death fences the orphan. Race two
  recoverers and a late publisher; retain one complete terminal record.
- Inject ENOSPC at capture/progress/publication, reserve retry and post-link
  fsync; assert no healthy-provider signal, retained custody and observable
  pending publication. Deadline/pipe-drain races, Darwin CWD zero with stale
  errno, detached reaper transfer/admission and Capture/Discard/Tee each have
  direct process fixtures.
- Sandbox probes try credential/config/protected reads, ref/index/gitfile
  redirects, hard-link/symlink/root swaps, denied IPC/network and outside writes.
  Quarantine failure preserves unrelated work and evidence; every cleanup race
  retains transcripts, telemetry, captures, quarantine and live/unknown runs.

Canonicalise macOS temp paths (`/var` versus `/private/var`). A macOS descendant
that escapes/reparents before the first census cannot be safely attributed;
never compensate with environment guesses or uncertain signals. Locally run
fmt/clippy and touched-crate/targeted tests. CI runs Linux workspace tests and
macOS release plus process/custody tests, and remaining Python/TS subjects.
Unsafe gets a second reviewer; the chair owns cross-family design acceptance.
Healthy-process signalling, weakened confinement or lost publication escalates
under R11. Live-provider recordings require user authority.

## Deletions and build order

Counts are `wc -l` at the source baseline above, not the older supervisor
spec's test-body estimates. Every path below is removed with its Rust subject;
shared caller conversion is coordinated with its named owner, without adapters.
Counts include tests/helpers, not just product code.

| Whole file(s), relative to directory shown | Lines per file |
| --- | ---: |
| `skills/orchestrate/scripts/`: `process_info.py`, `worker_liveness.py`, `writer_lease.py` | 452; 435; 245 |
| Same directory: `provider_confinement.py`, `trusted_git.py`, `agy_home.py` | 1,085; 112; 268 |
| Same directory: `instruction_quarantine.py`, `lane_protection.py`, `output_custody.py`, `secret_scan.py` | 202; 202; 224; 223 |
| Same directory: `lane_launch.py`, `lane_exit.py`, `git_evidence_packet.py`, `bin/ps`, `attempt_storage.py` | 190; 130; 88; 188; 176 |
| `skills/_shared/`: `custody.py`, `bounded_process.py` | 327; 230 |
| `scripts/`: `worktree.py`, `worktree` (Python wrapper), `clean.py`, `reap.py` | 982; 7; 953; 258 |
| Same directory: `lane_safety_guard.py`, `git_evidence.py` | 456; 341 |
| `runtime/fabric/src/`: `process-identity.ts`, `ps.mjs` (JS helper) | 392; 20 |
| `tests/`: `test_process_info.py`, `test_worker_liveness.py`, `test_bounded_process.py` | 286; 466; 294 |
| Same directory: `test_lane_launch.py`, `test_lane_exit.py`, `test_worktree_policy.py` | 191; 106; 1,018 |
| Same directory: `test_clean.py`, `test_git_evidence.py`; `tests/support/confinement.py` | 1,526; 346; 8 |
| `tests/lane_conformance/`: `__init__.py`, `conftest.py`, `fake_provider.py`, `support.py` | 1; 66; 130; 256 |
| Same directory: `test_seed.py`, `test_s2_capture.py`, `test_s2_lifecycle.py`, `test_s2_rust_faults.py` | 90; 28; 185; 119 |
| `runtime/fabric/tests/process-reaping.test.ts` | 278 |

Whole-file subtotal: **13,560 Python/TS lines**, plus 20 JS lines. Remove
`tests/lane_conformance/README.md` with its suite, and rewrite its custody
invariants into the Rust integration/process tests first. Shared custody and
bounded execution helpers disappear with their last Rust consumer conversion;
Fabric owns the process-identity/ps caller rewrites, not a new process engine.

These mixed files lose the relevant subjects/helpers with the owning build
PR, then disappear entirely when their other Rust owner lands. Their full-file
counts identify the deletion surface; **none** is added to the subtotal above.

| Shared file(s) | Whole-file lines; replacement/deletion owner |
| --- | --- |
| `skills/orchestrate/scripts/provider_exec.py`; `provider_observer.py` | 2,760; 175. Supervisor replaces capture/census/process loop, confinement replaces permission/instruction helpers; providers owns decoding/plans and deletes the remainder. |
| Same directory: `run_lineage.py`; `dispatch_run.py`; `run_worker_detached.py` | 656; 2,483; 254. Run/Fabric replace custody, cancellation, writer leases, reaping and detached launch; Run owns scheduling/receipts. |
| Same directory: `stall_watchdog.py`; `log_reducer.py` | 190; 204. Run keeps observational progress and bounded output reduction; remove duplicate liveness/secret helpers with their callers. |
| `tests/test_provider_exec.py`; `test_dispatch_run.py`; `test_cf_dispatch_git_custody.py` | 9,272; 7,255; 211. Remove process/confinement/lease/evidence subjects here; providers/Run delete their remaining subjects, without porting monkeypatch seams. |
| `tests/test_issue_697_custody.py`; `test_lane_safety.py` | 507; 515. Replace custody/path/process safety in Rust; Run owns finaliser tests, CLI owns pre-push and hook-install subjects. |
| `skills/session/scripts/cleanup_run_artifacts.py`; `tests/test_run_artifact_cleanup.py` | 202; 199. Workflow replaces delivery scratch eligibility/authority; use this area's retention API instead of a second deletion engine. |
| `tests/test_exec_layout.py`; `test_batch_run.py` | 158; 1,397. Remove orphan cleanup/process cases; Run owns layout/scheduling remainder. |
| `tests/test_provider_observer.py`; `test_log_reducer.py`; `tests/lane_stub.py`; `tests/support/proc.py` | 145; 248; 161; 84. providers/Run replace observer/reduction fixtures; remove stub/process helper when their last old test disappears. |
| `runtime/fabric/src/json-subprocess.ts`; `runtime/fabric/tests/json-subprocess.test.ts` | 107; 55. Fabric uses supervisor Executor; deletes Node child helper and its subjects. |
| `runtime/fabric/tests/helpers/fixture-processes.ts`; `runtime/fabric/tests/fixture-processes.test.ts` | 68; 74. Fabric replaces shared Node process fixtures with Rust fixtures. |
| `runtime/fabric/tests/host-liveness.test.ts`; `execution-lifecycle.test.ts`; `execution-pruning.test.ts`; `named-sessions.test.ts` | 92; 2,555; 375; 677. Fabric removes process/unknown-identity/pruning subjects and owns the remaining session/runtime tests. |
| `runtime/fabric/tests/lane-reader.test.ts`; `runtime/fabric/tests/lanes-cli.test.ts` | 212; 259. Run/Fabric replace orphan-reap and watchdog-process subjects with Rust integration cases; Fabric/CLI deletes the TS files with its remaining reader/command subjects. |
| `scripts/node-workspace-preflight.mjs`; `scripts/node-workspace-provision.py`; `tests/test_node_workspace_preflight.py`; `tests/test_node_workspace_provision.py` | 119; 130; 173; 207. Remove worktree bootstrap here; CLI/CI owns deleting these shared Node lifecycle helpers and tests when their remaining CI/package callers disappear. |

Retire supervisor `pending.rs`, `test_support.rs`, ps/fabric_ps presentation and
Python differential helpers/tests; retain only useful native observation code
from process_info/worker_liveness. Retire confinement `pending.rs`,
`pending/python_unicode.rs`, `python.rs` and Python-oracle tests in its modules.
Move/reuse S2 Rust rather than count the move as new code. Rehome the inert
cli worktree/clean/reap/git_evidence/lane_safety and workflow custody/writer_lease
copies into the owners above; delete their old modules and parity-only tests.
R0 also removes their oracle generators/golden judges with those subjects;
CLI owns shared port-tooling deletion. No schema/Unicode emulation is retained.

Budget **+4,000 net Rust lines including new tests** after reuse; expected net
across Python/TS/Rust is therefore **-9,560 lines or better**, before mixed-file
and further Rust-emulation savings (JS saves another 20). This is an estimate,
not a test quota; every build PR reports measured `Net lines:` and explains
growth. Do not double-count shared-file totals as this area's savings.
No build step installs the binary, reads old state, adds a selector, or leaves a
Python/TS adapter. Each step removes its source, subject tests, dead helpers and
inert emulation in the same PR; still-Python callers wait for their owning Rust
area rather than gaining a bridge.

| PR-sized step | Land and delete together |
| --- | --- |
| 1. Safe OS owner | Reuse #1348 plus stream enum, corrected timeout/CWD, bounded detached reaper and SafeFs APIs. Move remaining lane unsafe into `supervisor::sys`; delete shared bounded-process and native process-fact owners/tests when Rust callers own them. Keep only safety-oriented Rust tests; remove parity harnesses. |
| 2. Provider custody | Move S2 machine/census/capture into supervisor; land guardian handshake, lease transfer, persistent reservation and exit publication. Delete Python process mechanics and lane launch/exit/observer transport, TS process-identity/reaping and conformance subjects after new Rust process cases pass. Fabric/Run own their caller rewrites and settlement. |
| 3. Permissions and evidence | Land typed permission backend, streaming redactor, safe Git/output custody, instruction quarantine and pinned protected-tip release. Delete provider_confinement and its confinement helpers/tests; remove confinement's pending/python emulation. Providers supplies supported profile fixtures. |
| 4. Worktrees and retention | Land typed worktree services, scratch-only plan/apply, managed run reap and scoped external-worker observations. Delete worktree/clean/reap/worker-liveness owners and tests; update surviving skills/hooks/docs to the frozen command tree with CLI/Fabric owners. |

A step may be split by safe filesystem/utility, custody, permissions or worktree
policy module, but cannot land a second executable owner for the same behaviour.
Every build PR reports measured `Net lines:` across Python, TS and Rust,
including deletions and tests. Design acceptance precedes those build PRs;
opening this design PR does not accept it or arm auto-merge.
