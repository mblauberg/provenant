# Supervisor and confinement

`provenant-supervisor` holds processes, output and kernel locks from launch
through cleanup and durable exit publication. `provenant-confinement` turns an
authorised execution plan into filesystem, Git, network and provider
permissions, checks evidence and quarantines instruction changes. Together they
ensure that a failed write, a lost client or a mistaken process identity cannot
kill a healthy lane, free a busy worktree or publish an incomplete result.

The [cross-cutting decisions](README.md#cross-cutting-decisions) bind state,
layout, commands, waits, records and hooks. [cli](cli.md) owns exact flags.
The crate notes in
[`crates/supervisor/README.md`](../../tools/front-door/crates/supervisor/README.md#spawn-polling-and-publication)
cover consumer wiring and platform limits.

## Purpose and ownership

Supervisor owns the process state machine, the syscall boundary, bounded
execution, descriptor-relative filesystem access, locks and atomic publication.
Confinement owns permission compilation, disclosure scanning and redaction,
quarantine, safe Git evidence, output custody and worktree management. Neither
owns the run registry, admission, attempts, scheduling, retries or result
semantics; those belong to run ([run](run.md)). `fabric-runtime` owns identity,
coordination and federation routing ([fabric](fabric.md)).

All first-party `unsafe` code, FFI and post-fork code live in
`supervisor::sys`. Each block documents validity, ownership, lifetime,
concurrency and errors, and an independent second reviewer checks every block.
Supervisor depends on core and records only. Confinement adds config and
supervisor's safe API, and no library imports `cli`.

## Read-only allocation observations

`Directory::private_size_at(name)` observes one normal entry without following
its final symlink. On macOS, supervisor calls `getattrlistat` with
`ATTR_CMN_RETURNED_ATTRS`, `ATTR_CMNEXT_PRIVATESIZE` in `forkattr`, and
`FSOPT_ATTR_CMN_EXTENDED | FSOPT_NOFOLLOW`. It returns an optional byte count;
omitted attributes and unsupported filesystems return `None`, while access and
I/O errors remain errors. Other platforms return `None`. Consumers use
`Metadata::allocated` for the explicit fallback. Private size applies to the
entry itself; a directory's value does not include descendant files.

## Boundaries

Supervisor adds no run verbs and no MCP tools. Fabric's tools call run, whose
top-level `cancel` and `reconcile` own control and recovery. Fabric's landing
lease is separate from the writer lease, and neither the landing lease nor a
driver lease substitutes for kernel writer custody.

Run composes confinement's output filter with supervisor's injected capture
filter, so supervisor never imports confinement. Providers supply argv,
environment, selected authentication inputs, a fixture-proven permission
profile and bounded decoding into progress, terminal or `ProtocolRejected`
events, with no signalling callback. Workflow supplies the integration ref,
authorised branch pruning and protected-tip release. No area probes then kills
a PID, or infers authority from a file containing a name. An unmanaged worker
observed by a diagnostic is not adopted by PID and has no custody.

## Contract

### Custody invariants

- Child observation is registered before spawn. Inherited signal dispositions
  and masks are reset. Exit is observed with `WNOWAIT`, cleanup runs while the
  leader still reserves its PID and PGID, and only then is the handle consumed
  to reap. A reused identity is never signalled.
- Only `ProviderGroup::stop(StopReason)` signals providers. The closed reasons
  are explicit cancellation, wall deadline, provider protocol rejection,
  supervisor signal and post-terminal cleanup. Silence, CPU inactivity, capture
  failure, ENOSPC and failed publication cannot construct a reason.
- A natural exit observed in a tick wins over late cancellation or deadlines.
  Startup progress is any byte on either stream or observed CPU progress. There
  is no startup deadline, and the wall deadline bounds execution. Silence is an
  observation, never a stalled verdict.
- One writer per registered linked worktree. Custody and writer leases outlive
  the requesting call and any provider still writing. Lock files are stable
  inodes, never unlinked and never explicitly unlocked while a duplicate lives.
- A registered nested owner is spared only with matching native identity,
  observed ancestry and proof of the held custody description. An unlocked
  descriptor, an environment token or a process name is insufficient. Shared
  code-mode hosts and sibling groups are never signalled, and parents cancel
  registered children by request, not by PID.
- One terminal record is published with no-replace semantics after the files it
  references are durable. Failure returns custody. An I/O warning cannot change
  lifecycle state. A visible link followed by a failed directory sync means
  `durability_unconfirmed`, not an absent record to overwrite.

### Process model

`Run owner -> attempt guardian -> provider`. The guardian is a short-lived
invocation of the same binary in its own session. A private, bounded launch
channel transfers the plan and a duplicated writer lease, with an ownership
acknowledgement before the parent disconnects. Secrets travel in memory, pipes
and the child environment, never argv or disk. Hidden `internal supervise`
takes no arguments and accepts only this handshake.

The pure machine has Prepared, Running, TerminalGrace, Stopping, Cleaning,
Exited and Published phases. Each turn observes `WNOWAIT` before stop requests.
Defaults are a 500 ms descendant grace and a 2 s root flush, and run supplies
the wall and terminal budgets. Linux subreaping and process-wide signal setup
happen only in the guardian, and macOS uses kqueue plus SIGCHLD. Abort and
`Drop` are never a cleanup mechanism.

The guardian keeps the provider, the attempt lock and the writer-lease
duplicate after a run owner or MCP host dies, and publishes `process-exit.json`.
Provider exec is gated on the guardian holding both leases and the durable
pre-spawn reservation. A missing handoff acknowledgement is an unknown launch
and never permission to launch again. Provider exec closes every custody
descriptor, so no provider holds an unlockable writer lease. Run records
reservation ownership across attempt generations and clears it only after final
settlement. Unresolved descendant custody keeps the worktree fenced after the
leader exits.

**Quiescence.** On macOS the provider must be reaped and two complete libproc
census snapshots at least 20 ms apart must show no other process in its session
or group and no surviving identity from the guardian's census history (ppid
edges are checked against native start-time identity). Failed or racing reads
stay unknown and make the snapshot incomplete. Linux needs two identical
complete per-thread kernel children snapshots. Overflow or incomplete proof
keeps the attempt fenced. After the requester closes its copy, the guardian
closes its own copy of the inherited lease without unlocking and tries a fresh
non-blocking writer lock on the original inode. Success proves no descendant
retains the lease. The exit's `settlement_evidence` is one of
`darwin_census_and_custody_lease`, `linux_kernel_children`, `never_spawned`,
`orphans_ended`, `leftover_not_proven` or `unresolved`.

**Escapees.** Every confined launch's seatbelt profile allows one Mach lookup
name unique to the attempt, `dev.provenant.attempt.<attempt id>`, which no
service answers. A seatbelt sandbox is inherited and cannot be dropped, so a
detached descendant still carries it. On macOS, post-terminal cleanup treats a
live process outside the provider and any spared owner as an escapee when
`sandbox_check` reports it sandboxed, allowed to look that name up and denied an
unrelated control name. It joins the tracked set and receives TERM, then KILL
after the grace, each time after its full identity is re-observed. If the nested
child store cannot be read or holds a nested run, nothing is signalled and the
escapee stays unresolved. macOS cannot prove a registered owner's custody, so
that owner and its descendants stay protected. Census observations never grant
signal authority. Only the unreaped provider, kernel-confirmed adopted children
and attempt-sandbox escapees may be signalled, plus the provider's own process
group while its leader is unreaped (on explicit cancellation, including one that
lands after exit, and on every natural exit during cleanup). Members that left
the group are never signalled, and survivors are published in `unresolved_pids`
and `unresolved_identities`. An unconfined launch is the residual that libproc
cannot reconstruct. Every production launch is confined, and Linux has no
confinement backend.

**Unresolved exits.** After publishing an unresolved exit the guardian stays
alive holding the leader and keeps cleaning. On cancel it signals the group
(TERM, grace, KILL) unless a spared owner shares it. When cleanup proves
quiescence it resolves the reservation and replaces `process-exit.json` once
with the settled facts, the only replace of that file. A supervisor signal
ends the wait only after one more bounded cleanup pass.

**Orphan recovery.** If the guardian is gone, `reconcile` (and `cancel` for a
run with no live actor) takes the writer lock without waiting. If it cannot, a
guardian or lease inheritor still holds custody, and reconcile reports
`escaped` while a named pid is alive (signal 0 only) and `held` otherwise. If it
can, `Recovery::end_orphans` re-observes each named pid. A gone, zombie or
reused pid needs nothing. A pid whose boot, pid and native start time match the
record (or, with no recorded identity, that macOS reports inside the attempt's
sandbox) receives TERM then KILL after a bounded wait. Anything else is never
signalled and is named in the settled exit. The result is `orphans_ended`, or
`leftover_not_proven` when a pid stayed unproven. A proven process that survives
KILL leaves the exit unresolved and reported as `escaped`. Run then commits the
settled exit, and `leftover_not_proven` makes a non-successful outcome `lost`.

If absence cannot be proved, run's `reconcile` accepts a typed recorded decision
only with matching authority, recording actor, scope, generation, evidence and
disposition before it changes the fence. Uncertainty alone never clears a
fence, and the decision permits no signal.

### Bounded execution

- Each stream is `Capture { limit }`, `Discard` or `Tee { limit, sink }`.
  Discard has no pipe, and Tee uses a bounded non-blocking queue whose loss is
  recorded without stalling capture. Defaults: 8 MiB retained per stream, 64
  MiB maximum, a 64 KiB frame cap. Frames are decoded before retention
  truncation. An oversized protocol frame is explicit decoder loss, never a
  fabricated terminal event.
- `timed_out` means a deadline caused TERM to a still-owned child. A natural exit
  followed by a pipe-drain deadline sets `drain_truncated` instead. Exit versus
  signal, byte totals and decoder loss are preserved.
- A Darwin CWD `proc_pidinfo` of 0 means Gone without consulting errno. CWD
  never grants signal authority.
- Utility final-wait exhaustion hands the owned child to a detached reaper and
  returns `CleanupPending`. One bounded queue reserves capacity before spawn.
  Utility handles carry direct-child authority only.
- Lock acquisition takes core `WaitBudget` and `CancelToken`, derives one
  monotonic deadline and polls `flock(LOCK_NB)`, retrying EINTR within the
  budget. Expiry returns `LockTimeout`, cancellation returns `Cancelled`, and a
  blocking `flock` is never entered. A deadline bounds acquisition, not lease
  lifetime: it never revokes custody or steals a lease. Lock ordering is run
  custody, repository metadata, writer reservation, attempt publication, then
  children journal. No SQLite transaction spans a lock wait.

### Permission policy

Confinement compiles `ReadOnly` or `WorktreeWrite(VerifiedWorktree)`, explicit
read and write roots and a network level, holding verified path anchors. It
rejects traversal, symlinked state or lock leaves, credential roots and broad
home or tmp grants.

- **Git.** Writers may change their linked tree, the private Git index, HEAD,
  COMMIT_EDITMSG, AUTO_MERGE, merge state and ORIG_HEAD, objects (pack writes,
  not `objects/info`), exactly their branch ref, lock and reflog, the private
  and common `FETCH_HEAD` and the common `packed-refs.lock`. `.git`,
  `commondir`, `gitdir`, `config.worktree`, other refs, packed refs, common
  config, remote-tracking refs and their reflogs, provider configuration and
  protected instructions stay immutable. The push guard trusts
  `refs/remotes/<remote>/HEAD` to identify the default branch. Common
  `info/exclude` is read-only. Read-only lanes read the bound common Git
  directory, including each linked worktree's private directory, and never
  write it.
- **Network.** A level: `provider` (the profile's API endpoints only, the
  read-only default), `local` (adds localhost and the Postgres socket) or
  `open` (adds any outbound, the writer default). `open` adds IP peers only.
  At every level a lane's only Unix-socket peers are the system DNS responder
  and, from `local`, the Postgres socket. Read-only and writer lanes therefore
  cannot connect to the peer agent's `<state>/peer-agent/peer.sock`, which
  relays to unconfined `fabric peer` children; a native probe proves it.
- **Toolchains.** Lanes use the user's own toolchains in place. `~/.cargo` and
  `~/.rustup` are readable with `RUSTUP_HOME` and `RUSTUP_AUTO_INSTALL=0`.
  No lane may write a store the host later runs unconfined: the Cargo registry
  and Git caches and lock files, `~/Library/pnpm`, `~/Library/Caches/pnpm`,
  `~/.npm` and `~/.cache/node` are never writable, because a poisoned
  `build.rs`, planted `PNPM_HOME` executable or altered Corepack package would
  run on the host's next build. A writer's cache instead holds private
  `cargo-home`, `npm`, `pnpm-home`, `pnpm-store`, `pnpm-cache` and `corepack`
  directories (with both the `npm_config_*` and `pnpm_config_*` store and
  cache names set), so writers download cold. A read-only lane keeps a scratch
  `CARGO_HOME` whose `registry` and `git` link to the shared stores read-only,
  so offline builds use extracted sources; it also sets `CARGO_NET_OFFLINE=true`
  and `CARGO_CACHE_AUTO_CLEAN_FREQUENCY=never`. Both lane kinds set `CARGO_TARGET_DIR`
  and `CARGO_BUILD_BUILD_DIR` to the lane cache's `target` directory (scratch
  for read-only lanes), overriding inherited Cargo build-dir configuration.
  A read-only lane reads and executes the host's
  existing `~/.cache/node/corepack` without write. Executables
  (`~/.cargo/bin`, `~/.rustup/toolchains`) are read and exec only, except the
  Corepack cache and a writer's `--write-root` and `domains.run.writer_writes`
  roots, which are also executable. Homebrew prefixes are readable except
  `var/`.
- **Writer tests.** Only `worktree-write` lanes may read and execute fixtures
  in their own attempt's `tmp/candidate-N` subtree. No parent, sibling
  candidate, other attempt or retained provider-state directory gains exec.
  Writers may signal their children (`signal (target children)`) and read
  `/dev/fd/N` through a file-read grant on the `/dev/fd` subpath. Read-only
  and other modes keep their existing grants. Hard links remain denied and
  short Unix socket paths still require host tests. Private check homes use
  `std::env::temp_dir()`, honouring the lane's `TMPDIR`.
- **Denied last.** Reads and writes of `~/.ssh`, `~/.aws`, `~/.kube`,
  `~/.secrets` and Cargo's `credentials` files are denied whatever a profile
  grants.
- **Writer cache.** Setup derives exactly
  `$HOME/Library/Caches/provenant-lane-cargo/<sha256>` from the canonical
  worktree and common Git paths and their device and inode identities, for
  confined lanes only. It is created under writer custody by no-follow
  traversal, must belong to the effective user with mode 0700, and is rechecked
  at compilation. Hard links stay denied, and symlinks are allowed where a lane
  may write except in provider and private state. With the macOS Command Line
  Tools installed, the recipe selects them with `DEVELOPER_DIR` and `SDKROOT`,
  and writers can execute the selected Git's `libexec/git-core`. Without the
  tools, macOS Rust builds are not supported.
- **Delivery.** A writer `TaskRequest` may carry `deliver: {remote, branch,
  open_pr}`. Run binds the remote's single push URL and the lane branch under
  writer custody and fetches before launch. After a complete, identity-verified
  successful exit and terminal confinement checks, the host holder pushes the
  final commit OID to `refs/heads/<branch>`, fetches it back and requires it to
  equal the lane head, sets the branch's upstream config and optionally runs
  `gh pr create --head <branch> --fill`. Host commands run from the protected
  primary root, helper and config paths exclude the writer worktree, shell SSH
  overrides and opaque shell credential helpers are refused, and hooks,
  fsmonitor, implicit tag pushes and submodule pushes are off. The credential
  discovery environment travels only on the private guardian handoff pipe, and
  URLs with embedded credentials are refused. Cancelled, failed, stopped and
  recovered attempts never start delivery. A sealed start marker prevents
  repeating the external effect, and terminal publication keeps a typed
  delivered, failed-stage or interrupted receipt.
- **Profiles and backends.** The providers owner loads and verifies profile
  fixtures into typed data. Profiles allow only the selected account mechanism
  and the lane's own continuation transcript. Unsupported or nested
  confinement fails prelaunch as `ConfinementUnavailable` and never silently
  widens access. `ConfinementBackend` has `probe` and `compile`, macOS SBPL is
  one module, and a new backend or provider profile is data or one module.
  A retained profile needs an enforcing backend and fixtures, and Linux fake
  providers do not certify a real profile.

**Disclosure and redaction.** Disclosure scanning is bounded (10,000 files, 64
MiB) and one streaming redactor handles known secret values, credential URLs and
token patterns, with overlap across chunks. Prompt-training routes refuse
overlap with protected prompt, cwd or read roots. Budget exhaustion refuses
disclosure, and an exception needs run's scope-bound authority, never prompt
text. Output is redacted before captures, diagnostics or JSON reach disk, raw
protocol bytes stay in bounded memory, and the inherited environment is never
persisted. A redactor failure discards the affected output and records a
content-free error.

**Quarantine.** After confirmed quiescence, quarantine holds the writer lease
and archives protected disk, index and HEAD patches, blobs and digests before
restoring anything. Only attributable ordinary `.agents/` edits are restored
automatically. Policy or control-file changes, concurrent edits or
unaccountable history need review: evidence is kept, a terminal failure is
published, and unrelated work and history are untouched. A tracked symlink at a
protected path (mode 120000, such as `CLAUDE.md -> AGENTS.md`) is evidence whose
bytes are the link target, read with `readlink` and checked against the
committed blob, never followed. Protected-tip verification returns a pinned SHA,
and workflow or chair authority releases that exact tip separately.

**Safe Git.** Evidence uses a trusted executable, typed operations, literal
pathspecs after `--`, bounded execution and a sanitised environment: Git
redirection and config injection are removed, and hooks, fsmonitor, textconv,
external diff, pager and lazy fetch are off. Common and private directories and
registration are verified, and copied gitfiles and alternate object stores
cannot vouch for protected history. Base and head OIDs are pinned once. NUL
status, selected paths, diff digest and before and after tracked fingerprints
are kept. Selected untracked content, changed HEAD, index or content, a timeout
or truncation refuses complete evidence. `confinement::run_utility` offers
workflow and run a closed set of Git (status, rev-parse, diff, log, worktree,
merge-base) and `gh` (PR, issue, check) reads with validated repository and IDs,
pinned executables and a bounded budget, and no shell, arbitrary argv or write
verbs.

### Worktrees

Create, checkout, remove and prune share a repository-scoped worktree and ref
lock. Writer admission holds it while it verifies registration and publishes its
reservation, and removal holds it through a fresh Git worktree census and the
checked-out-branch refusal. This serialises managed checkouts and grants no
authority over unrelated Git actors.

| Command | Contract |
|---|---|
| `worktree create` | Create a clean linked tree on a new or existing branch under a verified `.worktrees/`. It returns the path, branch and pinned base, and provisions no dependencies. |
| `worktree list` | Repository-scoped path, branch and head, dirty state and writer state (free, live or unknown). |
| `worktree remove NAME [--discard]` | Refuses the primary, a dirty tree and a live writer, and has no force. An unknown writer is accepted when its lease is free and the tree is clean. It deletes the branch when its commits are contained in the integration branch, and `--discard` also deletes an unmerged one and returns its head. |
| `worktree remove --merged` | Needs explicit authority, a pinned integration ref (a local branch or a remote-tracking ref such as `origin/main`) and `--branch` or `--since`. Ordinary merges are proved by ancestry. A squash needs a recorded merged PR in this repository whose head OID equals the branch tip and whose merge OID is an ancestor of the integration tip. A `gh` failure or malformed output keeps the branch unless ancestry alone proves it. The integration branch and a branch at its tip are never pruned. The branch OID is rechecked and the ref is deleted with that expected old OID. Remote deletion is not offered. |
| `hook claude pre-tool-use` | The lane-safety guard, a row in cli's `HookSpec` table. Stdin is bounded to 1 MiB. Unreadable, malformed, oversized or timed-out input emits deny JSON with exit 0, and only a failure to write the denial exits 2. |

`worktree list` never creates a writer lock; a worktree whose lock is missing
is reported as `unknown`.

For each removed worktree, removal derives its lane cache from the verified
worktree before Git deletes it, then deletes only that cache through the
detach-and-verify path while holding writer exclusion. Cache cleanup failures
emit a warning after the worktree has been removed. Removal leaves other
orphaned caches and existing cleanup tombstones for `provenant gc` to sweep.
Cache cleanup accepts v1 owner markers when the owner path is missing, and
`provenant gc` retries their interrupted tombstones. Existing v1 owners are kept, reported
as foreign when outside the project, and never collected as idle.
Idle GC takes the existing writer lock without waiting, checks for reservations
and releases the guard immediately after the tombstone rename. A held lock keeps
the cache as live; a missing or unsafe lock, a reservation, or unverifiable
registration keeps it as unobservable.

Removal and pruning hold writer exclusion, recheck reservations and worker
observations, and refuse live or unknown candidates. A post-merge hook passes
only its just-merged scope. The guard forbids name- and pattern-based kills
(`pkill`, `killall`, pattern-fed `kill` or `xargs`) and cleanup racing an owned
writer. Its bounded shell-token inspection is advisory defence around direct
harness commands, and OS confinement and owned handles enforce isolation.
Unknown or too-deep shell constructs are never certified safe, and the guard
does not claim to parse every shell language.

### Records and state

Paths are relative to the state root:

```text
run.sqlite3                    # run's admission, operations, registry and status
runs/<RunId>/
  request.json                 # immutable canonical submission and prompt digests
  owner.json                   # scheduler custody reference
  custody.lock
  children/{journal.lock,<ChildId>.json}
  attempts/<AttemptId>/
    prompt.txt  result.md      # run-owned, immutable once sealed
    prompt.md                  # optional top-level argv spill, run-owned
    custody.lock
    launch.json                # redacted summary
    process.json               # replaceable observation
    cancel.json                # the only supervisor-consumed cancellation request
    process-exit.json          # immutable exit and cleanup evidence
    stdout.log  stderr.log     # bounded, redacted captures
    reserve                    # 64 KiB publication headroom
    provider-state/            # retained history, no credentials
    quarantine/                # retained patches, blobs and manifest
    scratch/                   # disposable launch staging only
worktrees/<ProjectId>/
  worktrees.lock
  <WorktreeId>/{identity.json,writer.lock,reservation.json}
```

Run owns its store, request, prompt, result and terminal facts, and Fabric
indexes IDs without copying lifecycle truth. Owner references hold no
PID-derived authority. All records use records' serde envelopes with
`schema_version: 1`. Writers refuse unknown versions and fields, and read-only
observers tolerate added fields within v1. JSON is bounded to 1 MiB (the
ephemeral launch message 2 MiB, process exit 16 KiB). OS paths stay
`PathBuf` or `OsString` until the records boundary and use records' explicit
byte-path encoding. Edge-built facts are `Serialize` only.

| Record | Write rule |
|---|---|
| `AttemptRecord` | At starting, records the applied plan's explicit write allowances in `write_grants`, with their source, byte-encoded absolute path and scope, plus project-relative `read_only_writable`. Writers also carry `git_grants_digest`; concrete Git paths stay out of the record. Deny rules still restrict these allowances. Missing fields in older records default to an empty list and no digest. Older binaries with strict attempt decoding reject populated new fields, including peer reads through `lanes --host`, as they did for `read_only_writable`. |
| `ProcessIdentity` | Host and boot identity, PID and native start stamp, plus group leader identity. Display time grants no authority, and absence, denial and unsupported stay distinct. |
| `LaunchSummary` | Resolved route, executable identity, permission-plan digest, worktree identity, environment names and budgets, with sensitive argv redacted and no prompt or values. It is never replayable launch authority. |
| `ProcessObservation` | Atomic replace. A stale observation cannot establish death. |
| `ProcessExit` | No-replace, with file and parent fsync. Failure keeps custody for retry. |
| `WriterReservation` | Published atomically under the writer lock before spawn. It is cleared only after proved quiescence, quarantine and the canonical terminal commit, or an authorised recorded reconcile disposition. A missing or malformed record with conflicting run evidence refuses acquisition. |
| `CancelRequest`, `ChildRegistration` | Atomic no-replace. The journal lock orders child registration against the parent's terminal commit, and stale generations cannot cancel a later attempt. A stop that finds an undecodable request, or one for another requester or an earlier generation, atomically replaces it and re-reads it, since the guardian would ignore it. A stop from a reattached handle never replaces a later generation's request. A stop from a held scope also replaces a later generation's request, which in a child directory can only be forged, and any symlink, FIFO or oversize entry, since the rename swaps the entry without following it. A directory named `cancel.json` still refuses the stop, and the lane can remove a request after it lands, so a lane can pre-empt or undo the host's stop. |
| `QuarantineManifest`, `GitEvidence` | Evidence files are durable before the no-replace manifest, and partial capture claims no success. |

Authoritative writes use held-directory `SafeFs` capabilities, exclusive 0600
temporaries in 0700 directories, full writes, file sync, atomic rename or
no-replace publication and a parent sync. Lock and custody leaves must be
regular, single-link and owner-controlled. A directory writable by another live
actor cannot be made safe by check-then-unlink, so deletion refuses it.

**Settlement.** After its canonical terminal transaction and quarantine commit,
run publishes `terminal-commit.json` and `quarantine/manifest.json`, then
`release.json` carrying both digests over the exact raw bytes, all with the run,
attempt and generation scope and `schema_version: 1`. Supervisor's
`custody::publish_release` owns that byte format, called by the guardian holder
after run's commit, and `Holder::admit` runs before spawn (a refusal publishes a
never-spawned exit and releases custody). Supervisor checks the attempt inode
and custody lock, verifies both digests and scopes and syncs before it mints
`SettlementCommit`. That receipt permits classified scratch deletion and fence
clearing under writer exclusion, and a missing or invalid attestation keeps the
fence. Scratch holds only sandbox profiles and staging, is deleted with a
no-follow walk that refuses mounts and links, skips drift and never uses age
alone as eligibility, and unclassified content is retained.

## External contracts

Provider wire forms, harness hook protocols, Git, `SKILL.md` and MCP are fixed
by their owners; Provenant's own launch, receipt and CLI formats are not.

| External contract | Fixture and assertion |
|---|---|
| Provider argv, streams, results and continuation | Sanitised real streams under `tests/fixtures/fabric-context/` seed the decoder tests, with typed events beyond the capture cap and no credential persistence. The providers crate owns the version manifests. |
| Harness Bash pre-tool hook | Recorded tool, session and cwd stdin with allow and deny stdout and exit under `tests/fixtures/external/hooks/` (broad-kill denial, malformed input, nesting bounds). |
| Git porcelain, OIDs, worktrees, refs, index and patches | Raw NUL worktree, status and diff output from the system Git, including SHA-1 and SHA-256 and linked trees (`crates/confinement/tests/fixtures/external`), plus temporary-repository tests for evidence races and redirects. |
| OS and sandbox interfaces | Handshake fixtures record the platform, backend and which operations were allowed or denied, not the generated SBPL text. Fake providers cannot certify a real profile, and a missing recording blocks that profile's support. |

## Failure and recovery semantics

- Storage exhaustion. Before spawn, run checks its disk-admission floor and
  supervisor allocates the 64 KiB reserve. On ENOSPC at publication the reserve
  is released and the write retried once, with no provider signal. If storage
  still fails, the guardian keeps custody and the bounded outcome, reports
  publication pending and retries with backoff. Recovery never invents a
  provider exit from silence or a missing file. After simultaneous process loss
  and permanent storage failure, evidence is reported unconfirmed and the
  writer fenced rather than published as success.
- A dirty, changed or unreadable writer tree prevents automatic retry, and
  silence never proves an untouched tree.
- Typed errors are `Refused`, `Busy`, `UnknownIdentity`, `InvalidRecord`,
  `Cancelled`, `CleanupPending` and I/O. A macOS descendant that escapes and
  reparents before the first census cannot be attributed safely, and no
  environment guess or uncertain signal compensates. Healthy-process
  signalling, weakened confinement or lost publication escalates to the user.

## Deliberate cuts

| Cut | Reason |
|---|---|
| Arbitrary-PID `reap` | A start-string check before `kill` does not reserve identity between check and signal on macOS; recovery and cancellation go through run by managed run ID |
| `Drop`-driven provider stopping and unbounded cleanup loops | Abort and SIGKILL bypass `Drop`, and a bookkeeping error must not trigger a destructor kill; an explicit guardian and a detached reaper replace them |
| `worktree remove --force` and automatic dependency cloning or linking | Force bypasses live-writer checks, and the product needs no Node bootstrap |
| `worktree check`, `verify` and `prune`, `clean` plan and apply, and a git-evidence command | Checks register under `check`, workflow uses evidence as library calls, and the guardian deletes scratch at settlement; no `clean` verb survives |
| Cross-session transcript scanning and a caller-supplied terminal-report command | Correlating the newest transcript by cwd is ambiguous with several workers; managed reports need owned wait evidence |
| A system-`ps` replacement and Python or Node emulation (locale, start strings, ps layout, error text) | Native identity and typed errors serve the safety decisions |
| A standalone writer lease and launch and exit wrappers | Their safety checks live in library calls, and Fabric owns the landing lease |
| Opaque staged bundles and duplicate legacy attempt files and digest sidecars | One canonical terminal record carries the publication invariant |
| A startup deadline stop and stall verdicts | Silence is an observation, and the wall deadline bounds execution |

## Verification

Tests use explicit clocks and injected observations, FIFO or socket handshakes
and observed EOF rather than sleeps, and canonicalised macOS temp paths
(`/var` versus `/private/var`). Every integration test sets a temporary HOME,
state root, `XDG_CONFIG_HOME`, `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_NOSYSTEM=1`
and every harness config path before it starts children.

| Where | What it proves |
|---|---|
| `crates/supervisor/src` unit tests and `custody_tests.rs` | State-machine transitions, natural-exit precedence, unknown identities, failed signals, stale generations, integer and size bounds, capture and discard modes, lock budgets and cancellation, both publication modes including post-rename sync failure |
| `crates/supervisor/tests/custody.rs`, `process.rs`, `publication.rs` | Fast exit before watch, inherited ignored SIGCHLD and masks, prelaunch, live and late cancellation, a silent healthy provider, stdin EOF, a terminal frame after the cap, PID reuse, a TERM-resistant provider, `setsid` grandchildren, nested owners, an unlocked-lock impostor, shared hosts, incomplete census, parent death leaving guardian and lease live, racing recoverers and late publishers, injected ENOSPC and post-link fsync with retained custody and no healthy-provider signal |
| `crates/confinement/src` tests and `tests/fixtures/external` | Credential, config and protected reads; ref, index and gitfile redirects; hard-link, symlink and root swaps; denied IPC and network, including the peer agent socket from both lane kinds; outside writes; secret chunk splits; scratch classification and drift; quarantine failure preserving unrelated work; backend probe and compile refusal; fixed Git and `gh` argv with arbitrary operations refused |
| `tools/front-door/tests/worktree_driver.rs`, `dispatch_provider.rs`, `fake_provider.rs` | The public worktree commands and launch, wait, cancel and recover through the built binary against isolated fake providers and temporary repositories |

Unsafe blocks get a second reviewer. CI runs the Linux workspace tests and the
macOS release and process and custody tests. Live-provider recordings need the
user's authority.
