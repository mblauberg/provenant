# Supervisor and confinement

Proposal for chair acceptance under [PORTING.md v7](https://github.com/mblauberg/provenant/blob/1b3d48e4c549811ce3636edc30b047f36fd24ada/docs/specs/rust-port/PORTING.md).
Author: `codex/gpt-6.1-sol@high` via Fabric. Source baseline:
`1b3d48e4c549811ce3636edc30b047f36fd24ada`; also reuse the bounded OS APIs in
[PR #1348](https://github.com/mblauberg/provenant/pull/1348), with the corrections
below. The [cross-area decisions](README.md#cross-area-decisions) bind state,
layout, commands, waits, records and hooks; cli owns the exact flag freeze.

## Purpose

`provenant-supervisor` holds processes, output and kernel locks from launch
through cleanup and durable exit publication. Its pure state machine orders
exit, cancellation and deadlines; its syscall boundary grants authority only
over processes it owns. `provenant-confinement` turns an authorised execution
plan into filesystem, Git, network and provider permissions, checks evidence,
and quarantines instruction changes. Together they prevent a failed write,
a lost client or a mistaken process identity from killing a healthy lane,
freeing a busy worktree or publishing an incomplete result. Worktree management
and scratch settlement use these same capabilities. Run owns the run registry, admission,
attempts, status, scheduling, retries and result semantics. `fabric-runtime`
owns identity, coordination and federation routing (R2 as amended by the chair).

## Keep and remove

Keep the custody invariants in the [supervisor spec](../../harness/rust-supervisor.md#structural-guarantees),
subject to v7's abort profile and single Rust implementation:

- Register child observation before spawn, reset inherited signal dispositions
  and masks, observe exit with `WNOWAIT`, clean up while the leader reserves its
  PID/PGID, then consume its handle to reap. Never signal a reused identity.
- Only `ProviderGroup::stop(StopReason)` can signal providers. Its closed reasons
  are explicit cancellation, wall deadline, provider protocol
  rejection, supervisor signal and post-terminal cleanup. Silence, CPU inactivity,
  capture failure, ENOSPC and failed publication cannot construct a reason.
- Natural exit observed in a tick wins over late cancellation/deadlines. Startup
  progress includes any byte on either stream or observed CPU progress. There
  is no StartupDeadline stop; the wall deadline bounds execution. Silence is
  an observation, never a stalled verdict. Keep bounded CPU/tree probes.
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
`SafeFs`, writer retry evidence, provider permission plans, instruction quarantine,
protected-tip release checks, output installation/digest verification, safe Git
evidence and scratch deletion at settlement. Their policy is specified below.

The council's executed-command tally across transcripts found worktree create
80, list 23, remove 15, check 2 and verify 0; clean 6 (including 2 help and
2 merged-pruning calls), with no plan call; git-evidence and worker_liveness
1 each; reap 5, all by PID; system ps 922. Searches, examples and heredocs were
excluded. Keep worktree create/list/remove and merged pruning. Native process
facts support custody internally; the occasional diagnostic calls do not need
new commands. Arbitrary-PID reap is removed despite use because it cannot
reserve process identity. Repository hooks and skills move to the frozen tree.

| Remove or consolidate | Evidence and replacement |
| --- | --- |
| Python observer process, opaque staged bundles, duplicate canonical/legacy attempt files and digest sidecars | These are internal seams in `provider_exec.py`, `dispatch_run.py`, `run_lineage.py` and `lane/observer.rs`. All owners become Rust; Run's decoder sends typed events and records supplies one canonical terminal record. Retain the publication invariant, delete the transport. |
| `Drop`-driven provider stopping and unbounded cleanup loops | `lane/group.rs` stops and polls in `Drop`; abort/SIGKILL bypass it. Explicit guardian ownership and a detached reaper replace it. A bookkeeping error must not trigger a destructor kill. |
| Python/Node Unicode, locale-start strings, ps layout and subprocess error emulation | Existing supervisor `pending`, `ps`, `fabric_ps`, and confinement `python`/`pending` implement runtime parity. Native integer identity and typed errors serve the actual safety decisions; v7 R0 removes parity as a requirement. |
| Whole-run retention and cleanup plans | Keep generated scratch deletion by the guardian at settlement. Drop clean plan/apply, CleanupPlan and its directory; history, captures and unclassified data remain retained. No `clean` verb survives. |
| `worktree remove --force`, automatic dependency-tree cloning/linking | `worktree.py` can bypass live-writer checks and provision Node modules. A Rust product needs neither Node bootstrap nor a live-writer override. Keep clean linked-worktree creation/removal and merged-branch pruning only within supplied authority. |
| Standalone writer lease, launch/exit wrappers and Git-evidence packet wrapper | `writer_lease.py`, `lane_launch.py`, `lane_exit.py`, `git_evidence_packet.py` duplicate decisions now shared by Run, confinement and workflow. Preserve their safety checks in library calls; run owns git_evidence_packet and completed-attempt facts, workflow owns review policy, and Fabric owns the landing lease. |
| Cross-session transcript scanning and caller-supplied terminal-report CLI | `worker_liveness.py` correlates newest transcript by cwd and accepts an exit status from its caller; multiple workers make that correlation ambiguous. Keep native repo-scoped PID/start/CWD/CPU diagnostics, also consumed by custody and the documented direct-CLI fallback. Drop old-root transcript correlation under R1 and require owned wait evidence for managed terminal reports. |
| System-ps replacement CLI | `bin/ps` and `ps.mjs` exist to work around process inspection in the old Python/Node paths. Internal callers use native `ProcessObserver`/`CwdObserver`; no general ps formatting/option emulation is needed. |
| Arbitrary-PID/tree signalling from `reap` | `reap.py` rechecks a start string before `kill`, which does not reserve identity between check and signal on macOS. Replace it with recovery/cancellation by managed run ID; external workers must enter through Run to obtain custody. This is a deliberate restriction even where the old command was used. |

## Process and permission design

`Run owner -> attempt guardian -> provider` preserves S2's custody. The guardian
is a short-lived invocation of the same binary in its own session, with Run's
provider decoder composed beside supervisor; supervisor imports neither Run nor
providers. A private, bounded launch channel transfers the plan and duplicated
writer lease, with an ownership acknowledgement before the parent disconnects.
Secrets use memory/pipes and the child environment, never argv or disk. Hidden
`internal supervise` takes no arguments and accepts only this handshake, which
names Run's scheduler or guardian role, not a public raw launch spec.
MCP cancellation stops a wait; explicit Run cancellation writes a scoped request.

The guardian keeps the provider, attempt lock and writer-lease duplicate after
a Run owner or MCP host dies, and publishes `process-exit.json`. The Run owner
holds its lease through retries and settlement. If it dies, the reservation
remains until recovery acquires the same writer exclusion and commits quarantine
and the canonical terminal bundle; a process-exit file alone never frees a
writer. If the guardian also dies, a persistent writer
reservation fences acquisition until provider/group absence is proved. Free
flock plus missing/unknown identity is insufficient, including death between
spawn and identity publication. Recovery censuses the guardian-recorded session
and PGID, with native identity checks; a changed boot identity also proves
absence. If absence cannot be proved, Run's `reconcile` accepts a typed,
recorded decision only with matching authority. It records actor, scope,
generation, evidence and disposition before changing the fence; uncertainty
alone never clears it. Neither recovery nor that decision permits killing by PID.
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
bounded wall/terminal budgets. Signal errors remain failed deliveries.
Linux subreaping and process-wide signal setup run only in the guardian;
macOS uses kqueue plus SIGCHLD. Explicit cleanup precedes consuming reap; abort
or `Drop` is never a provider cleanup mechanism.

On macOS, quiescence requires the provider to be reaped and two complete
libproc census snapshots at least 20 ms apart. Libproc enumerates all PIDs;
foreign-UID services that deny full BSD records use the short BSD record plus
the public `KERN_PROC_PID` start-time prefix. Two matching native reads bracket
PID/ppid/group/status/session and start time; failed or racing reads remain
unknown and make the snapshot incomplete. Both must contain no other
process in the provider's session or process group and no surviving identity
or descendant (ppid edges checked against native start-time identity) from the
guardian's recorded census history. Each tick prunes identities proved absent
or replaced; overflow or incomplete proof keeps the attempt fenced.
The provider inherits the writer lease's open file description. After the
requester closes its copy on acknowledged handoff, the guardian closes its own
copy without unlocking and attempts a fresh nonblocking writer lock on the
original device/inode. Success proves no surviving descendant retains the
inherited lease description; the new guard remains held through terminal
publication. Failure keeps the durable reservation fenced.
A concurrent `acquire_recovery`/`acquire_writer` waiter can win the
drop-to-`try_lock` gap, conservatively fencing an otherwise clean run.
The exit's `settlement_evidence` records
`darwin_census_and_writer_lease`, `linux_kernel_children`, `never_spawned` or
`unresolved`. Linux requires two identical complete per-thread kernel children
snapshots, including stable thread membership, before cleanup settlement.
The Linux global census skips rows whose process group or session is 0 (host
kernel tasks and container shims; never a descendant of the guardian's
session); a direct observation of such a PID stays unknown.

A macOS daemon that escapes with `setsid` before ever being sampled and closes
the inherited lease is the residual: libproc cannot reconstruct that missing
history. It grants no signalling authority. Census observations never grant
arbitrary PID or process-group signal authority; only the unreaped provider
and kernel-confirmed adopted children may receive signals.


Reuse S2's machine/census/capture, `SafeFs` and #1348's `Executor`, raw outcome,
strict text decoding, private direct-child custody, CWD, append/lock and marker APIs. Correct
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
  expose neither raw owning descriptors nor numeric-PID grants. Delete
  `execve_strict` in `src/exec.rs`, the Python delegation shim, and remove
  `clap` from supervisor's `Cargo.toml`; argument parsing belongs to cli.

Lock acquisition takes core `WaitBudget` and `CancelToken`, deriving a monotonic
deadline once at entry. Delete `LockOptions.timeout: None` in `fs/path_io.rs`.
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

### Safe syscall wrappers

All unsafe blocks, FFI declarations, raw-pointer/descriptor conversions and
post-fork callbacks live in `supervisor::sys`. Each documents validity,
ownership, lifetime, concurrency and errors; an independent second reviewer
checks every block (R7). Safe handles own descriptors exactly once and expose
no raw custody capability. The wrapper inventory covers existing sys/lane
calls and the retained implementation:

| Wrapper / native calls | Invariants |
| --- | --- |
| Child observation: `waitid(WNOWAIT)`, `waitpid` | Observe only owned children; initialise/read siginfo only on success, retry EINTR within budget, preserve the unreaped leader through cleanup, reap last and once. |
| Signalling: `kill`/`killpg` | Only an owned, still-reserved PGID or direct-child handle plus StopReason can deliver a signal. Reject caller-supplied PIDs, zero/all targets and reused identities; ESRCH/EPERM are distinct. Census never grants signal authority. |
| Spawn/exec: `setsid`, `execve`, `fcntl`, `open`, `close` | Preallocate NUL-terminated argv/env and null-terminated pointer arrays before fork; keep buffers and passed descriptors alive through exec. Post-fork code uses only async-signal-safe calls, no allocation/locks; preserve the exec-error pipe. Only acknowledged guardian handoff inherits custody; providers close it. |
| Descriptor sweep: `syscall(close_range, CLOEXEC)`, Linux `getdents64`, Darwin `getdirentries64` (raw syscall 344) | Inspect the actual post-fork table with fixed buffers; bounds-check record lengths/alignment and fd numbers, set CLOEXEC, then allow only explicit descriptors. Fail launch on incomplete sweep; never use allocating readdir after fork. |
| Signal setup: `sigfillset`, `sigemptyset`, `sigaddset`, `sigprocmask`, `sigaction`, `signal` | Guardian owns process-wide installation, saved actions/mask and handler lifetime; reset inherited dispositions/masks before exec. Handler only writes a byte to a live nonblocking self-pipe and preserves errno. Restore before pipe close; no Rust allocation/locks in handler. |
| Linux `prctl` | Only the dedicated guardian changes subreaper state; check GET/SET errors and restore prior state after child cleanup. No process-wide changes in shared MCP/CLI hosts. |
| Event loop: `kqueue`, `kevent`, `pipe`, `poll`, `read`, `write` | Own CLOEXEC/nonblocking fds; register observation before spawn, use live bounded arrays, initialise only returned events/bytes and bound EINTR retries by WaitBudget. Pipe saturation loses a wakeup byte, never custody; recheck child state every turn. |
| Native census: `proc_listallpids`, `proc_pidinfo`, `proc_pidfdinfo`, `sysctl`, `sysconf`, `mach_timebase_info` | Check ABI flavour, alignment, returned size/capacity and integer conversion; native boot/start/session/PGID and ancestry stay distinct from display names. Zero CWD result is Gone; permission/short reads are Unknown. Bound census growth; validate timebase denominator and use integer CPU units. Held-lock flags require matching description/inode and monotonic no-unlock ownership. |
| Held-path I/O: `openat`, `fstatat`, `fstat`, `mkdirat`, `unlinkat`, `symlinkat`, `readlinkat`, `linkat`, `futimens` | Live parent fd, single validated NUL-free component, explicit no-follow policy; check type/owner/link count and device/inode. Create private exclusive files; do not unlink locks or traverse mutable foreign directories. Link publication must not replace; metadata never proves custody alone. |
| Atomic publication: `renameat`, `renameat2`, `renameatx_np` | Same verified filesystem/parents; Replace versus NoReplace explicit (exchange only for an authorised caller). NoReplace never degrades to replace; sync file then parents and distinguish visible-but-unsynced publication. |
| Directory iteration: `fdopendir`, `readdir`, `closedir` | Transfer one fresh fd exactly once to DIR; serial owner copies each bounded name before next read, distinguishes EOF/errno, closes once. Used outside post-fork code only. |
| Lock: `flock` | Stable regular owner-controlled inode, nonblocking acquisition only; poll within WaitBudget/CancelToken, no public unlock of a shared description, no TTL stealing. |
| Entropy: `getentropy` | Live exclusive buffer at most 256 bytes; failure refuses name/token creation, no predictable fallback. |
| Pipe capacity: `fpathconf`, `ioctl`, `fcntl`; fixture socket `setsockopt` | Valid owned fd, matching request/ABI buffer and checked positive sizes; failed capacity probes report loss/unknown, never block a provider. Socket tuning is fixture-only. |
| Errno: `__error`, `__errno_location` | Read/write the current thread's live errno slot immediately; preserve it across signal handlers and distinguish readdir EOF from failure. No pointer escapes. |

Delete parity-only `newlocale`, `freelocale`, `strftime_l`, `localtime_r` and
`gmtime_r` wrappers in `sys/process.rs` and `sys/lane_files.rs`. Core's `jiff`
clock supplies timestamps; no locale or Python-start-string emulation remains.

### Permission policy

Confinement compiles `ReadOnly` or `WorktreeWrite(VerifiedWorktree)`, explicit
read roots, network policy and named capabilities. Hold verified path anchors;
reject traversal, symlinked state/lock leaves, credential roots and broad home
or tmp grants. Writers may change their linked tree, private Git index/HEAD,
objects and exactly their branch/ref lock/reflog. Keep `.git`, `commondir`,
`gitdir`, `config.worktree`, `objects/info`, other refs, provider configuration
and protected instructions immutable. The task's network policy never blocks the
selected provider's own API endpoints, which its profile permits. Read-only execution uses declared reads
and private scratch. Browser/Postgres IPC is explicitly granted.

The providers owner loads and verifies profile fixtures, then constructs the typed
profile data consumed by confinement. Provider profiles contain fixture-proven
runtime-state/authentication exceptions;
only the selected account mechanism and own continuation transcript are allowed.
No unrelated credential homes or other sessions. Compose provider-native
controls with macOS sandbox-exec when needed; unsupported/nested confinement
fails prelaunch as `ConfinementUnavailable`, never silently broadens access.
Linux fake-provider tests do not establish real-provider confinement: a retained
profile needs an enforcing backend and fixtures or its mode is refused.
`ConfinementBackend` has `probe` and `compile`; macOS SBPL is one backend
module. Provider-profile policy is typed data (roots, network, capabilities
and runtime exceptions), not branches in a monolithic compiler. A new backend
implements this trait in one module without changing provider lifecycle code.

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

The [cross-area decisions](README.md#cross-area-decisions) own the command
contract, wait budgets, run verbs and `HookSpec`. CLI supplies its JSON envelope,
1 MiB result cap, exits and exact flags; this area defines none separately.
Diagnostics stay on redacted stderr. Supervisor adds no run verbs or MCP tools;
Fabric's tools call Run, whose top-level `cancel` and `reconcile` own control
and recovery. Fabric's landing lease is separate from the writer lease.

| Command / handler | Contract |
| --- | --- |
| `worktree create BRANCH` | New or existing branch, optional `--base` for a new branch only; global `--project` selects the repository; create a clean linked tree under verified `.worktrees/`, return path, branch and pinned base. No Node provisioning. |
| `worktree list` | Repo-scoped path, branch/head, dirty state and writer state free/live/unknown. |
| `worktree remove NAME` / `worktree remove --merged` | Refuse primary, dirty, live or unknown writers; no force. Merged pruning requires explicit authority, a pinned integration tip and exactly one of `--branch NAME…` or `--since OID` under cli's frozen flags. Recheck OID before ref deletion; a scoped post-merge hook cannot sweep other branches. |
| `hook claude pre-tool-use` | Lane-safety handler registered in cli's `HookSpec` table. Bound stdin to 1 MiB; allow/non-Bash emits empty stdout/0, deny uses recorded harness decision JSON/0. Unreadable, malformed, oversized or timed-out guard input emits deny JSON with exit 0. Only failure to write the denial uses blocking exit 2 and content-free stderr. Installer consumes the same table. |

Drop worktree check/verify/prune, clean plan/apply, git evidence and run workers.
Checks register findings under `check`'s binding subjects; workflow uses pinned
Git evidence and tip verification as library calls. The guardian deletes
scratch at settlement; no `clean` command survives.

Keep lane-safety's prohibition on name/pattern-based kills (`pkill`, `killall`,
pattern-fed kill/xargs) and worktree cleanup racing an owned writer. Its bounded
shell-token inspection is advisory defence around direct harness commands;
OS confinement and owned handles enforce provider isolation. Unknown/too-deep
shell constructs cannot be certified safe for destructive operations. There
is no claim to parse all shell languages or infer authority from a prompt.

## Records and state

State supplies the verified `StateRoot` from the binding State root and Layout
rows: default `~/.local/share/provenant/v1`, with `--state-dir ABS` overriding
`PROVENANT_STATE_DIR`. No XDG selection or old-root fallback. One root serves
the host; ProjectId is metadata on runs. No old state or reservation is adopted.

The run-owned tree is the shared execution-run contract (paths relative to root):

```text
run.sqlite3                    # run's atomic admission, operations, registry/status
run.sqlite3-wal / -shm
runs/<RunId>/
  request.json                 # immutable canonical submission/prompt digests
  owner.json                   # supervisor-issued scheduler custody reference
  custody.lock
  children/{journal.lock,<ChildId>.json}
  attempts/<AttemptId>/
    prompt.txt                 # run-owned immutable exact provider prompt
    result.md                  # run-owned sealed result, including failure
    custody.lock
    launch.json                # redacted summary
    process.json               # replaceable observation
    cancel.json                # sole supervisor-consumed cancellation request
    process-exit.json           # immutable exit/cleanup evidence
    stdout.log                 # sole bounded redacted stdout wire capture
    stderr.log                 # sole bounded redacted stderr capture
    reserve                    # 64 KiB publication headroom
    provider-state/            # retained history; no credentials
    quarantine/                # retained patches/blobs and manifest
    scratch/                   # disposable launch staging only
worktrees/<ProjectId>/
  worktrees.lock               # repository worktree/ref mutations
  <WorktreeId>/{identity.json,writer.lock,reservation.json}
```

`worktrees/<ProjectId>/` is supervisor/confinement's top-level subtree in state's
layout terms; linked trees themselves remain repository resources. Run owns
its store, request, prompt, result and terminal facts; Fabric indexes IDs
without copying lifecycle truth. Structured run events live in SQLite, with
no duplicate capture. Owner references contain no PID-derived authority.

Only generated sandbox profiles and disposable staging enter scratch. The
guardian deletes classified scratch after settlement, confirmed quiescence,
quarantine and durable canonical Run terminal publication, while retaining
writer exclusion. Recovery applies the same rule. Never recursively delete
above scratch or stage history/transcripts there; retain unknown or
unclassified content. Provider history stays in provider-state; never copy
credentials into either subtree. Managed worktree removal is separate.

All records use records' serde envelopes with `schema_version: 1`; refuse
unknown versions and unknown fields for launch, cancellation, reservations,
release evidence. Read-only observations permit added fields
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
| `WriterReservation` | Worktree device/inode, verified Git location, branch/base OID, run/attempt IDs, guardian identity/session/PGID, launch generation and unresolved-cleanup bit. Atomic publication under writer lock before spawn; clear only after proved quiescence, quarantine and canonical Run terminal commit, or the authorised recorded reconcile disposition, under writer exclusion. A missing/malformed record with conflicting run evidence refuses acquisition. |
| `CancelRequest` / `ChildRegistration` | Request ID, target run/attempt generation, reason and requester; child run ID/token and parent attempt. Atomic no-replace request/registration; journal lock orders child registration against parent's terminal commit. Stale generations cannot cancel a later attempt. |
| `QuarantineManifest` / `GitEvidence` | Pinned OIDs, selected byte-paths, before/after fingerprints, file digests, view/restore status. Evidence files durable before no-replace manifest publication; no claim of success on partial capture. |

Stored records deserialize only at validated storage edges. Edge-built facts
implement `Serialize` only; consumers cannot construct them from arbitrary JSON.

All authoritative writes use held-directory `SafeFs` capabilities, exclusive
0600 temporaries in 0700 owner directories, full writes, file sync, atomic
rename/no-replace publication and parent sync. Lock/custody leaves must be
regular, single-link, owner-controlled files. Following reads are explicit
#1348 `ReadOptions` operations for trusted inputs, never custody opens. A
candidate directory writable by another live actor cannot be made safe with a
check-then-unlink; deletion refuses it. Scratch deletion holds run/worktree
exclusion through its no-follow walk, refuses mounts/links/root replacement
and skips drift. Age alone is never eligibility.

Before spawn Run checks its disk-admission floor; supervisor allocates a 64 KiB
reserve. On publication ENOSPC, unlink the reserve and retry once, with no
provider signal. If storage still fails, retain custody and the bounded outcome
in the guardian, return publication-pending to the caller and retry on bounded
backoff. Recovery never fabricates a provider exit from silence or a missing
file. Complete evidence cannot be guaranteed after simultaneous process loss
and permanent storage failure; report unconfirmed and fence the writer rather
than publish invented terminal success.

## Library boundary

Signatures match the run draft's supervisor table. Core owns `WaitBudget`
and `CancelToken`; each wait derives one monotonic deadline, never an unbounded
None path. Libraries return typed `thiserror` failures (Refused, Busy,
UnknownIdentity, InvalidRecord, Cancelled, CleanupPending, I/O); the table shows
success values. CLI/MCP renders them. Supervisor imports core/records only;
confinement adds config and supervisor's safe OS API. No library imports cli.

| API | Contract and consumer |
| --- | --- |
| `machine::step(State, Event, Tick) -> (State, Vec<Effect>)` | Pure OS-driver decisions with explicit clocks/delivery outcomes. Silence/I/O cannot construct StopReason. |
| `launch(OwnedLaunch, CapturePlan, PublicationReservation) -> CustodyHandle` | Run passes verified plan, attempt custody and controlled writer-lease handoff in OwnedLaunch, with its startup handshake WaitBudget/CancelToken. PublicationReservation reserves destination/headroom before spawn. Acknowledgement transfers obligation to guardian; failure returns handles or unresolved-handoff token. Run retains run/writer custody through retries/settlement. Handle is a channel, never a PID grant. |
| `observe(handle, WaitBudget, &CancelToken) -> Observation` | Run/Fabric observe Live/Exited/Unknown and publication state. Reader cancellation ends observation, retaining custody. |
| `stop(handle, StopReason) -> StopReceipt` | Run maps an authorised generation-bound cancel to ExplicitCancellation, wall expiry to WallDeadline, provider rejection to ProtocolRejected; guardian signal and post-terminal cleanup use their closed reasons. Durable attempt-bound request/receipt; guardian alone signals. Acknowledgement does not prove death; channel loss is unknown. |
| `recover(CustodyRef) -> Reattached\|Exited\|Unknown` | Run supplies a validated stored custody reference with bounded observation context. Verify guardian session/PGID and native identity; free lock alone proves nothing. Unknown preserves fence pending authorised typed reconcile. Recovery never kills by PID. |
| `acquire_custody(RunRef, WaitBudget, &CancelToken)`; `acquire_writer(VerifiedWorktree, ReservationPermit, WaitBudget, &CancelToken)` | Opaque custody, controlled duplicate handoff, lock plus reservation checks; no TTL takeover or SQLite access. Fabric supplies graph permits. |
| `exec::Executor::execute(ExecOptions)`; `run_text`; `pid::{CwdObserver,SystemCwdObserver}` | Bounded utility execution/strict UTF-8 and stream enums. Direct-child custody stays private to Executor; CWD observations grant no signal authority. |
| `fs::{SafeFs,SafeDir,SafeFile,PathIo}`; `lock(Mode, WaitBudget, &CancelToken) -> LockGuard` | Config, confinement, fabric-store and workflow use held-directory bounded/following reads, append and nonblocking lock polling. No public unlock of a shared custody description. |
| `publish(SafeDir, Name, Bytes, PublishMode::{Replace,NoReplace}) -> Publication` | Sync file, atomically publish, sync directory. Returns Durable, VisibleDurabilityUnconfirmed or pre-publication error. NoReplace conflict preserves existing bytes; reconcile generation/digest and retry sync, retaining custody. |
| `confinement::run_utility(UtilityRead, VerifiedRepo, UtilityBudget, &CancelToken) -> ExecOutcome` | Workflow and run's git_evidence_packet replacement use fixed Git/gh reads via Executor. Closed Git status/rev-parse/diff/log/worktree/merge-base and gh PR/issue/check operations; validated repo/IDs, fixed fields, sanitised env/pinned executables. No shell/arbitrary argv/API/write verbs. Budget bounds wall/grace/reap/streams; truncation refuses evidence. Secret auth never enters argv/logs. |
| `ConfinementBackend::{probe,compile}` | `probe(BackendContext) -> Availability`; `compile(PlanPermissions, ProviderProfile, Anchors) -> ConfinedLaunch`. SBPL is one module, provider policy is data. Providers/Run receive enforcement and digest, never stop authority. |
| `verify_worktree`; `Git::evidence`; `verify_tip`; `quarantine`; `OutputCustody::{install,verify}`; `scan_disclosure`; `Redactor` | Run/workflow consume verified paths, pinned GitOid/evidence and safe outputs. Workflow supplies exact protected-tip release authority. |
| `confinement::worktrees::{create,list,remove}`; `settle_scratch` | CLI/Fabric services and guardian settlement; remove supports scoped merged pruning. Supervisor supplies OS/locks, confinement supplies policy. |

Required from state/config: fresh verified StateRoot, ActorId/ProjectId/WorkScope, GitOid
(SHA-1 and SHA-256), WaitBudget/CancelToken, integer clocks and jiff clock,
byte-path encoding, canonical digests, bounded versioned serde records and
resolved policy/profile settings. Help/root resolution starts neither
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
callback. Run (`design/execution-run.md`) owns atomic admission/operation
insertion in its durable store, the run registry,
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
infers authority from a file containing a name. Run deletes `lease.py` with
the run-directory contract; Fabric keeps its landing lease. Neither the old
driver lease nor the landing lease substitutes for kernel writer custody.

Replace direct-PID reap guidance in `docs/runbooks/shell-pitfalls.md` and
`skills/orchestrate/references/{lane-safety,direct-cli-fallback,worker-liveness}.md`
with Run-managed launch/cancel. An observed unmanaged worker is diagnostic only;
its owning harness controls cancellation. Provenant neither adopts it by PID
nor pretends a diagnostic row gives it custody.

## External contracts and fixtures

Provider wire forms, harness hooks, `SKILL.md`, MCP and Git remain fixed under R0.
Our launch/receipt/CLI formats change freely; no Python/Node oracle or reader.
The external hook/Git/OS recording directories below do not yet exist at this
baseline: they are required build inputs, not evidence already collected.

| External contract | Fixture and assertion |
| --- | --- |
| Provider argv/env, streams, results and continuation | Sanitised real streams in `tests/fixtures/fabric-context/{claude,codex,cursor,opencode,agy}.jsonl` seed decoder tests. Providers owns version/provenance manifests and missing launch/permission/continuation recordings. Assert typed events beyond capture cap and no credential persistence. `fabric-v1/provider-*.json` are owned receipts, not provider-wire recordings. |
| Harness Bash pre-tool hook | CLI records real tool/session/cwd stdin and allow/deny stdout/exit under `tests/fixtures/external/hooks/`. `test_lane_safety.py` supplies scenarios, not recordings. Assert broad-kill denial, malformed-input handling and nesting bounds. Other harness mappings/installation stay with CLI. |
| Git porcelain/OIDs/worktrees/ref/index/patch forms | Capture system-Git version plus raw NUL worktree/status/diff/index output under `tests/fixtures/external/git/`, including SHA-1/SHA-256 and linked trees. Also run temporary-repository tests for evidence races and redirects. |
| `SKILL.md` and MCP | records owns R6's tracked frontmatter subset fixtures; confinement protects instruction bytes without another YAML reader. fabric-mcp owns recorded JSON-RPC 2.0 stdio/cancellation fixtures; integration proves clean stdout and retained custody after cancelled waiters. |
| OS and sandbox interfaces | Linux/macOS handshake fixtures record platform/backend and operation allowed/denied, not generated SBPL text equality. Fake providers cannot certify a real profile. Missing external recordings block that profile's support; live-provider capture needs user authority. |

## Tests and acceptance

Follow [R9](https://github.com/mblauberg/provenant/blob/1b3d48e4c549811ce3636edc30b047f36fd24ada/docs/specs/rust-port/PORTING.md#r9-tests-prove-the-new-design): beside-module unit tests
use explicit clocks and injected observations. Cover transitions/idempotence,
natural-exit precedence, unknown identities, failed signals, stale generations,
registration versus publication, integer/size bounds, secret chunk splits,
path aliases and scratch classification/drift. Delete tests of shapes that the
new enums, capabilities and compiler make impossible.

Binary integration drives public commands and MCP against isolated fake
providers and temporary repositories. Every test sets temporary HOME,
StateRoot/PROVENANT_STATE_DIR, XDG_CONFIG_HOME, GIT_CONFIG_GLOBAL,
GIT_CONFIG_NOSYSTEM=1 and every harness config path (including CLAUDE_CONFIG_DIR
and CODEX_HOME) before starting children;
no test reads the real install, old state, global Git config or harness settings.
Old-root sentinels must remain untouched. Cover launch/wait/cancel/recover,
worktree contention, disclosure, Git evidence, quarantine and settlement.

Supervisor exposes a `fixture` module behind a dev-only feature, never enabled
in release/install products. Each crate's process tests, including workflow,
use a `harness=false` integration test target whose main dispatches scenario
arguments before running its suite, re-executing `current_exe()` for children.
No extra binary or test crate is added; libtest never parses scenario arguments.
The fixture module owns scenario dispatch/handshakes, fake providers and lock
holders, with the only scoped `#[allow(clippy::disallowed_methods)]` permitting
`Command::new` to re-exec that test executable. Production has no exception;
signals/descriptor setup use supervisor's safe APIs. Binary integration tests
launch the real command/MCP through supervisor's safe runner, using the
binary package's `CARGO_BIN_EXE_provenant` path; only self-re-exec uses the
fixture module's scoped `Command::new` allow.
Use Fabric's small in-house snapshot helper if needed; no `insta` dependency.
Test fixed Git/gh argv and arbitrary-operation refusal, lock budgets/cancellation,
and both publication modes including post-rename sync failure.

Linux and macOS process tests use FIFO/socket handshakes and observed EOF,
not sleeps. Rebuild the custody scenarios from `tests/lane_conformance/` and
`test_issue_697_custody.py` before deleting their old subjects:

- Fast exit before watch; inherited ignored SIGCHLD/masks; prelaunch, live and
  late cancellation; any stdout/stderr byte or CPU progress, and a silent
  healthy provider through the former startup limit; terminal stdin EOF;
  cap followed by terminal frame; decoder/sink failure; PID reuse.
- TERM-resistant provider, setsid grandchildren, nested owners, unlocked-lock
  impostor, shared hosts/siblings and incomplete census. Parent/MCP death leaves
  the guardian and lease live; double-owner death fences the orphan. Exercise
  session/PGID census and authorised/unauthorised typed reconcile decisions. Race two
  recoverers and a late publisher; retain one complete terminal record.
- Inject ENOSPC at capture/progress/publication, reserve retry and post-link
  fsync; assert no healthy-provider signal, retained custody and observable
  pending publication. Deadline/pipe-drain races, Darwin CWD zero with stale
  errno, detached reaper transfer/admission and Capture/Discard/Tee each have
  direct process fixtures.
- Sandbox probes try credential/config/protected reads, ref/index/gitfile
  redirects, hard-link/symlink/root swaps, denied IPC/network and outside writes.
  Quarantine failure preserves unrelated work and evidence; every cleanup race
  retains transcripts, telemetry, captures, quarantine and live/unknown runs. Exercise
  backend probe/compile refusal and adding a policy profile as data.

Canonicalise macOS temp paths (`/var` versus `/private/var`). A macOS descendant
that escapes/reparents before the first census cannot be safely attributed;
never compensate with environment guesses or uncertain signals. Locally run
fmt/clippy and touched-crate/targeted tests. CI runs Linux workspace tests and
macOS release plus process/custody tests, and remaining Python/TS subjects.
Unsafe gets a second reviewer; the chair owns cross-family design acceptance.
Healthy-process signalling, weakened confinement or lost publication escalates
under R11. Live-provider recordings require user authority.

### Run settlement attestations

After its canonical terminal transaction and quarantine commit, Run durably
publishes private v1 `terminal-commit.json` and `quarantine/manifest.json`, with
exact `scope` (run, attempt, generation), `schema_version: 1`, and `kind` equal
to `canonical_terminal` and `quarantine_manifest`, respectively. It then
publishes `release.json` with the same scope/version and `terminal_digest` and
`quarantine_digest` as `CanonicalV1::file_digest` values over the exact raw
artifact bytes. These attestations are bounded to 1 MiB each, reject unknown
fields and duplicates, and remain excluded from provider mutation. Run owns
and protects their directories and publication order. Supervisor checks the
original attempt inode and custody lock, verifies both digests and scopes,
and syncs held files/directories before minting `SettlementCommit`. That
receipt permits classified scratch settlement and fence clearing under writer
exclusion; an absent or invalid attestation retains the fence.

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
| Same directory: `lane_launch.py`, `lane_exit.py`, `bin/ps`, `attempt_storage.py` | 190; 130; 188; 176 |
| `skills/_shared/`: `custody.py`, `bounded_process.py` | 327; 230 |
| `scripts/`: `worktree.py`, `worktree` (Python wrapper), `clean.py`, `reap.py` | 982; 7; 953; 258 |
| `tests/`: `test_process_info.py`, `test_worker_liveness.py`, `test_bounded_process.py` | 286; 466; 294 |
| Same directory: `test_lane_launch.py`, `test_lane_exit.py`, `test_worktree_policy.py` | 191; 106; 1,018 |
| Same directory: `test_clean.py`; `tests/support/confinement.py` | 1,526; 8 |
| `tests/lane_conformance/`: `__init__.py`, `conftest.py`, `fake_provider.py`, `support.py` | 1; 66; 130; 256 |
| Same directory: `test_seed.py`, `test_s2_capture.py`, `test_s2_lifecycle.py`, `test_s2_rust_faults.py` | 90; 28; 185; 119 |

Whole-file subtotal: **11,659 Python lines**. Fabric alone counts
`process-identity.ts` (392), `ps.mjs` (20) and `process-reaping.test.ts` (278); cli
counts `lane_safety_guard.py` (456); run counts `scripts/git_evidence.py` (341),
`tests/test_git_evidence.py` (346) and `git_evidence_packet.py` (88). None is in
this subtotal. Remove `tests/lane_conformance/README.md` with its suite, and
rewrite its custody
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
| `skills/session/scripts/cleanup_run_artifacts.py`; `tests/test_run_artifact_cleanup.py` | 202; 199. Workflow retires scaffold cleanup with run-directory removal; guardian settlement owns scratch deletion, with no second cleanup engine. |
| `tests/test_exec_layout.py`; `test_batch_run.py` | 158; 1,397. Remove orphan cleanup/process cases; Run owns layout/scheduling remainder. |
| `tests/test_provider_observer.py`; `test_log_reducer.py`; `tests/lane_stub.py`; `tests/support/proc.py` | 145; 248; 161; 84. providers/Run replace observer/reduction fixtures; remove stub/process helper when their last old test disappears. |
| `runtime/fabric/src/json-subprocess.ts`; `runtime/fabric/tests/json-subprocess.test.ts` | 107; 55. Fabric uses supervisor Executor; deletes Node child helper and its subjects. |
| `runtime/fabric/tests/helpers/fixture-processes.ts`; `runtime/fabric/tests/fixture-processes.test.ts` | 68; 74. Fabric replaces shared Node process fixtures with Rust fixtures. |
| `runtime/fabric/tests/host-liveness.test.ts`; `execution-lifecycle.test.ts`; `execution-pruning.test.ts`; `named-sessions.test.ts` | 92; 2,555; 375; 677. Fabric removes process/unknown-identity/pruning subjects and owns the remaining session/runtime tests. |
| `runtime/fabric/tests/lane-reader.test.ts`; `runtime/fabric/tests/lanes-cli.test.ts` | 212; 259. Run/Fabric replace orphan-reap and watchdog-process subjects with Rust integration cases; Fabric/CLI deletes the TS files with its remaining reader/command subjects. |
| `scripts/node-workspace-preflight.mjs`; `scripts/node-workspace-provision.py`; `tests/test_node_workspace_preflight.py`; `tests/test_node_workspace_provision.py` | 119; 130; 173; 207. Remove worktree bootstrap here; CLI/CI owns deleting these shared Node lifecycle helpers and tests when their remaining CI/package callers disappear. |

Retire supervisor `pending.rs`, `test_support.rs`, ps/fabric_ps presentation and
Python differential helpers/tests; retain only useful native observation code
from `process_info`/`worker_liveness`. Retire confinement `pending.rs`,
`pending/python_unicode.rs`, `python.rs` and Python-oracle tests in its modules.
Move/reuse S2 Rust rather than count the move as new code. Rehome the inert
cli worktree/clean/reap/git_evidence/lane_safety and workflow custody/writer_lease
copies into the owners above; delete their old modules and parity-only tests.
R0 also removes their oracle generators/golden judges with those subjects;
CLI owns shared port-tooling deletion. No schema/Unicode emulation is retained.

Measured S1-S3: **+18,872 net Rust lines including tests**: S1
**+2,564** (#1370), S2 **+6,248** (#1379), S3 **+10,060** against main
`54b611bb2f9dcd39522a8c3ab74d9b2e83aa46d7`. S3's library and config binding
contain **10,801 Rust lines including tests**, above the roughly 6,000-line
target. The original +4,000 estimate omitted much of the native custody,
permission and hostile-filesystem coverage, while last-importer gates defer
Python deletion savings. Every build PR reports measured `Net lines:`; do not
double-count shared-file totals as this area's savings.
No build step installs the binary, reads old state, adds a selector, or leaves a
Python/TS adapter. Each step removes its source, subject tests, dead helpers and
inert emulation in the same PR; still-Python callers wait for their owning Rust
area rather than gaining a bridge.

The first three steps land Rust primitives, not early deletion of leaves still
imported by Python. The last-importer matrix below gates deletions in the
coupled run controls/facade steps 4+5; no adapters are added while waiting.

| PR-sized step | Land and delete together |
| --- | --- |
| S1. Safe OS owner | Reuse #1348; correct streams, timeout/CWD, reaper and `SafeFs`. Move lane unsafe into sys; land the wrapper inventory, bounded locks/publication and fixture module. Delete parity wrappers, `execve_strict` and supervisor `clap`. Old Python leaves remain with their old subjects. Explicit S2 exemptions in `legacy_lane`: census-based numeric-PID kills in `group.rs` and Python start-string emulation (`files.rs::canonical_start`, `fixtures.rs` `localtime_r`/`strftime`). |
| S2. Provider custody | Move S2 machine/census/capture; land guardian handoff, persistent reservation, scratch settlement and exit publication. Rebuild custody tests before deleting conformance subjects. Replace the S1-exempt numeric-PID census kills and remove `canonical_start` plus fixture `localtime_r`/`strftime` start-string emulation here. Run owns old launch/control caller removal in R4+R5. |
| S3. Permissions, evidence and worktrees | Land the confinement library: backend trait/SBPL, redactor, Git/output custody, quarantine, tip verification and create/list/remove with merged pruning. Config binds a random host-local project ID under lock. Delete confinement emulation and obsolete Rust command copies; surviving Python helpers wait for R4+R5's last importer. CLI step 4 (area bindings) wires worktree/cleanup onto the frozen Clap model and updates hooks/skills; S3 leaves the existing command and hook in place. |
| R4+R5. Run controls/facade deletion PR | Run's steps 4+5 remove provider/control/finaliser importers with their owners; delete each leaf and subject tests only when its matrix gate is met. Run cuts scaffold and git_evidence_packet; Fabric/cli/workflow delete/count their own files. No tests of deleted subjects survive and no Python bridge lands. |

Direct production importers/callers of counted leaves are listed below;
subject tests retire with the leaf. R2+R3 are run's dispatch/batch removal;
R4+R5 are its coupled controls/facade/finaliser removal. A gate includes all
named outside-area callers, even if that moves the leaf's deletion later.
Primitives land in S1–S3; old-subject leaves stay until that gate, without a
bridge to Rust.

| Leaf | Importers/callers and last-importer deletion gate |
| --- | --- |
| `bounded_process.py` | `dispatch_run.py`, `batch_run.py` (R2+R3), `capabilities.py` (CLI), and dynamic loaders `scripts/fabric_lanes.py`, `scripts/fabric_git.py`, `scripts/fabric_hosts.py`; R4+R5 after CLI/Fabric retire every remaining caller. |
| `custody.py` | `batch_run.py`, `dispatch_preflight.py`, `dispatch_run.py`, `layout.py`, `run_controls.py`, `run_dir_finalize.py`, `run_lineage.py`, `run_records.py`, `stall_watchdog.py`, workflow's `review_plan.py` and dynamic loader `scripts/fabric_git.py` (11). Confinement deletes at R4+R5 only after workflow/Fabric also retire their imports. |
| `provider_confinement.py` | `provider_exec.py`; `cf_dispatch.py`, `dispatch_preflight.py`, `dispatch_run.py`, `utility.py` and `agy_home.py` also reach `provider_exec.provider_confinement`, with `utility_output_hook.py` reaching it through `utility.provider_exec`. R4+R5 only after every direct and facade caller retires, never S3 alone. |
| `process_info.py` | `dispatch_run.py`, `layout.py`, `memory_admission.py`, `provider_exec.py`, `run_lineage.py`, `worker_liveness.py`, `bin/ps`; `clean.py` dynamically loads it and `agy_home.py` stages it. R4+R5 after clean/worktree, provider shim and diagnostic callers retire. |
| `worker_liveness.py`, `bin/ps` | Direct CLI/skill diagnostic invocations; `bin/ps` imports `process_info.py` and `agy_home.py` stages both. Retire the shim with providers and R4+R5's managed controls and updated fallback/worker-liveness guidance. |
| `writer_lease.py` | `dispatch_run.py`, `dispatch_preflight.py`, `batch_run.py`, `provider_exec.py`; R4+R5 removes last `provider_exec.py` importer. |
| `secret_scan.py` | `dispatch_run.py`, `dispatch_preflight.py`, `batch_run.py`, `log_reducer.py`, `stall_watchdog.py`; R4+R5 after output/liveness callers retire. |
| `output_custody.py` | `cf_dispatch.py`, `fabric_records.py`, `instruction_quarantine.py`, `provider_exec.py`; R4+R5 after providers/Fabric replace all four. |
| `trusted_git.py` | `lane_protection.py`, `provider_confinement.py`, `provider_exec.py`; R4+R5 after all three retire. |
| `attempt_storage.py` | `dispatch_run.py`, `provider_exec.py`; `clean.py` dynamically loads it. R4+R5 after provider and cleanup callers retire. |
| `lane_launch.py`, `lane_exit.py` | `provider_exec.py` imports both; `lane_launch.py` imports `lane_exit.py` and dynamically calls `run_records.py`. Delete together at R4+R5 with `provider_exec.py` and that cancellation call. |
| `agy_home.py`, `instruction_quarantine.py` | `provider_exec.py`; R4+R5 after providers retire it. |
| `lane_protection.py` | `worktree.py`, Fabric CLI and orchestrate skill invoke it; R4+R5 after frozen worktree/hook callers replace these entrypoints. |
| `scripts/worktree.py`, `scripts/worktree` | `node-workspace-provision.py` imports; `fabric_hosts.py`, `fabric_git.py` and `dispatch_preflight.py` load; `cf_dispatch.py` and the wrapper execute. R4+R5 after CLI/Fabric/state replace all callers and merged-pruning policy lands. |
| `clean.py`, `reap.py` | CLI entrypoints and hook/skill callers; `worktree.py` dynamically loads `clean.py`. R4+R5 after CLI switches merged pruning and Run replaces reap with reconcile/cancel. |

A step may be split by safe filesystem/utility, custody, permissions or worktree
policy module. Old leaves serve only their old subjects until the coupled caller
cut; no selector or second public launch path is introduced.
Every build PR reports measured `Net lines:` across Python, TS and Rust,
including deletions and tests. Design acceptance precedes those build PRs;
opening this design PR does not accept it or arm auto-merge.

The rehome/reuse list above, including native `process_info`/`worker_liveness` and `sys/process/darwin.rs`, is recoverable from `e0613438` after deletion from this skeleton.
