# Supervisor and confinement

Owner: `crates/supervisor` (custody), `crates/confinement` (permissions,
evidence, worktrees), `crates/os` (bounded I/O and execution)
Anchor: `crates/supervisor/src/custody.rs`, `crates/supervisor/src/machine.rs`,
`crates/confinement/src/permissions.rs`, `crates/confinement/src/worktrees.rs`

Supervisor holds a provider's process, output and custody through durable exit
publication; confinement turns an authorised plan into filesystem, Git, network
and provider permissions. Wiring and platform limits are in the
[crate notes](../../tools/front-door/crates/supervisor/README.md). Paths are
relative to `tools/front-door/`.

## Custody

### Closed stop reasons; natural exit wins

Only `ProviderGroup::stop(StopReason)` signals a provider: cancellation, the
wall-clock runtime deadline, protocol rejection, a supervisor signal or
post-terminal cleanup. Silence, inactivity, capture failure, ENOSPC and failed
publication are never stop reasons, and there is no startup deadline. A natural
exit wins over a late cancel or deadline, and a host sleep cannot extend the
deadline.
- Why: a bookkeeping fault or slow start must never kill a healthy provider,
  and a stuck one must not hold a worktree for hours.
- Owner: `crates/supervisor/src/machine.rs`

### Signals need proven identity and scope

The PID stays reserved until the handle is reaped last. Only the unreaped
provider, its group while the leader is unreaped, descendants with retained
proof of provider ancestry, and proven attempt-sandbox escapees may be signalled.
Descendant proof survives session changes and reparenting, but every signal
revalidates the full native identity. Native descendants may be stopped only for
a recorded provider stop (cancellation, deadline, budget, context handoff, protocol
rejection or supervisor signal); an ordinary provider exit leaves them under
conservative custody. Registry and host child checks must permit cleanup. When
there is no native child registry, a present host holder must affirm that no graph
child remains. Spared and protected owners and their possible descendants stay excluded.
A census listing or cwd observation alone never grants signal authority, and
recovery signals only a PID whose recorded identity matches.

A launch is named by its attempt's `WorkKey`: the guardian's launch summary
records the key, and a stop request is identified by attempt and generation.
A supervisor restart finds a live writer by the key through the attempt row,
then reads that attempt's custody records.
- Why: signalling a recycled PID or a sibling's group kills unrelated work.
- Owner: `crates/supervisor/src/process_os/`, `crates/supervisor/src/custody.rs`

### Read-only writer presence

Status uses the recorded boot, PID and native start identity for the guardian,
provider and tracked descendants. It reports a writer as live only when the
guardian or provider's exact identity is present and non-zombie. A changed boot
proves the old process tree is gone. Same-boot absence remains unknown even
with a complete census: the durable process checkpoint can predate a fork into
another session or process group, and the census cannot identify that untracked
child after reparenting. A sealed process exit reports settlement as pending.
Missing or malformed checkpoints, reused PIDs, inaccessible processes and
incomplete censuses are unknown. Status only projects these facts; scoped
reconciliation rechecks custody before changing attempt state.
- Why: a dead guardian or recycled PID cannot hide a surviving provider child,
  and uncertainty must not be turned into a running writer or an unsafe retry.
- Owner: `crates/supervisor/src/custody.rs`, `crates/run/src/controls.rs`

### Tick-driven settlement

Tick queues recovery for settling and unattended attempts in every registered
project, using one bounded host process census for the pass. Failed project
recovery does not prevent other projects or scheduled tasks from running.
A sealed exit remains settlement evidence when the census is unavailable;
live attempts retain their automatic fence. Unknown attempts are queued for
proof-only recovery: the original guardian and custody locks, writer lease and
a fresh empty worktree census must prove absence before same-boot settlement.
Unknown evidence itself never clears custody.
Repeated ticks reuse the recovery driver's backoff, queued-worker marker and
native settlement locks rather than duplicating settlement work. The guardian
persists a failed terminal publication in the scoped settlement journal so a
status read can explain the pending settlement without consulting stderr.
- Why: losing the guardian or its immediate terminal publication must not lose
  the automatic path to a terminal attempt.
- Owner: `src/tick.rs`, `crates/run/src/controls.rs`,
  `crates/supervisor/src/settlement_progress.rs`

### Explicit abandonment

A chair may explicitly abandon a nonterminal writer with a reason when its
presence is not live, its native writer lease is free and the configured
worktree process census positively reports `Free`. Same-boot unknown presence
is eligible only with both exclusion and positive non-occupancy evidence.
Live presence, held exclusion and every non-free or incomplete occupancy
result refuse abandonment with typed blocking evidence. Launch and attempt
custody stay excluded during the decision. This escape hatch does not change
the proof required by automatic reconciliation.

The explicit loss intent is durable before the attempt is committed `lost`;
repeating an interrupted abandonment with the same reason finishes it. An
incomplete intent remains visibly settling until that retry. Abandonment warns
that the attempt was declared lost and retains all attempt evidence, scratch,
worktree contents and private Git. A never-dispatched queued attempt keeps its
existing evidence-preserving cancellation behaviour.
- Why: when native guardian-loss proof is unavailable, deliberate recovery must
  not turn an incomplete process census into permission to reuse a worktree.
- Owner: `crates/run/src/settlement.rs`, `crates/cli/src/run/reconcile.rs`

### Guardian holds custody past its owner

The guardian keeps the provider, attempt lock and writer lease after its owner
dies. Secrets travel in memory and pipes, never argv or disk.
A missing launch acknowledgement is an unknown launch, never permission to
launch again.
- Why: losing the caller must not lose custody or double-launch a writer.
- Owner: `crates/supervisor/src/guardian/`

### Quiescence before release

A writer stays fenced until the provider is reaped, two complete censuses show
no surviving member or escapee, and a fresh lock on the original lease inode
succeeds. Failed, racing or incomplete reads keep the fence; only a recorded
decision with matching authority clears one, and that decision permits no signal.
- Why: a leftover descendant could still write the worktree.

### Provider descriptors and detached services

Writer, custody, port and directory descriptors close on provider exec. Only
explicit executable inputs (secrets and confinement profiles) cross that edge;
the private guardian handoff retains its capability transfer. An ordinary
service detached by an unsandboxed provider therefore cannot retain the lane's
inherited writer lease. Guardian loss still leaves the durable unresolved
reservation fenced against every new attempt, even if the kernel lease is free.

Cleanup has a bounded stop budget. At its deadline a separate, cooperatively
bounded diagnostic census names observable PIDs, short native command names and
open writer-lock paths, with a reconcile action. An open path is not proof of
flock ownership and grants no signal authority. Unknown or unreadable holders
remain explicit uncertainty. The guardian continues retaining unresolved
custody; a timeout alone never authorises settlement or an unrelated signal.
- Why: detached services must not hold a lease or cause unsafe release.

### Durable exit publication

The initial exit record is published without replacing an existing one, after
what it references is durable. Run pins an immutable provider-exit snapshot
before committing its outcome; later cleanup may replace only the process-exit
observation. A failed publication keeps custody and retries; an unconfirmed
sync is `durability_unconfirmed`, never absent.
- Why: a lost or overwritten exit record loses the only proof of outcome.

### Writer reservation and settlement

The writer reservation is published before spawn and cleared only after
quiescence and run's terminal commit, or an authorised reconcile; the worktree
flock stays held through import and [publication](run.md#publication-order). A
missing or malformed reservation refuses automatic recovery of that attempt;
[explicit abandonment](#explicit-abandonment) instead requires native exclusion
and positive worktree non-occupancy. Scratch deletion needs a verified settlement
commit.
- Why: a writer must never be freed on a partial or forged settlement.

### Stable locks and atomic writes

Lock inodes are never unlinked while held, and acquisition never revokes or
steals a lease. Authoritative writes are private temporaries, synced, renamed
atomically and followed by a parent sync. Owner records carry no PID-derived or
replayable authority.
- Why: a recreated lock inode lets two owners hold one lock, and a crash must
  leave the old or new file whole.
- Owner: `crates/os/src/fs.rs`

### Build admission is a sourced port

Sandboxed Cargo shims use `PortTable.build_admission`, independently of attempt
admission: builtin uses host slots and the effective disk policy, command runs
the repository wrapper around Cargo, and none omits admission. The host freezes
the backend/source in `PROVENANT_BUILD_ADMISSION_BACKEND` and
`PROVENANT_BUILD_ADMISSION_SOURCE`. Native launches retain their host toolchain;
no shim is installed for them. Local-Cargo policy still applies to every shim
backend. Wrapper execution streams Cargo output without a host-state receipt.
- Why: build coordination varies by repository without changing attempt custody.
- Owner: `crates/providers/src/lane.rs`; `src/build_slot.rs`

### Bounded execution

Captures and frames are bounded; an oversized frame is decoder loss, never a
fabricated terminal event.
- Why: unbounded output must not stall custody or invent an outcome.

## Permission policy

Lane profiles start open and deny the boundaries
([ADR 0040](../adr/0040-open-lane-policy-with-explicit-boundaries.md)).

### Plans refuse unsafe roots

Confinement refuses traversal, symlinked state or lock leaves, declared
credential roots and grants enclosing them, broad home or tmp grants and
Git-internal roots. Credential declarations name concrete resolved paths and
their source: built-in secret stores, provider configuration and data stores,
every registered account's stores and the resolved Cargo home's credential
files. The core root table owns these declarations; state-root selection and
lane grant validation consume it. File declarations retain exact scope.
Credential directories are denied after every grant. The standalone Codex
release package read exception is relative to its declared Codex home; it
confers no writes and never exempts another overlapping credential declaration.
Every declaration is kept, whatever its order or kind; each spelling of a
declared location carries every declaration made at or beneath any spelling of
it, so a store registered at a Codex home or its alias stays refused.

A component named `.claude`, `.codex`, `.cursor` or `.gemini` does not establish
a credential boundary. Project-local agent configuration overlaps warn with
their source and proceed. A verified linked worktree is admitted at its real
path, including beneath project-local agent configuration or outside the
project, unless it overlaps a declared credential store. Git markers, common
directories and private worktree Git directories keep their hard hold.
`run --explain` and launch preparation use one source admission decision and
report its refusal code and reason or sourced warnings.

Unsupported confinement fails `ConfinementUnavailable` and never widens
access. An optional root that resolves into a declared credential root is
omitted with a warning.

Registered Git projects' declared worktree allocation roots remain protected
from extra host write grants, including before those roots exist. Resolve the
project port table from the verified primary checkout in Git's worktree
inventory; the common Git directory's parent does not establish that owner.
- Why: one broad grant silently voids the boundary.
- Owner: `crates/core/src/roots.rs`, `crates/confinement/src/permissions.rs`,
  `crates/cli/src/run/provider/confinement.rs`

### Shared Git metadata is read-only

A writer owns a private Git directory with the shared object store as a
read-only alternate; shared refs, config, hooks and admin metadata stay
read-only, and the host imports the result and moves the branch by
compare-and-swap.
- Why: ordinary Git works in the lane while one host boundary guards shared
  metadata.

### Native environment and sandbox tool stores

Unsandboxed lanes inherit the host environment. `FilteredLanePath` selects
retained requester PATH, otherwise captured host search path, or process PATH
only without a host handoff. It filters canonical directories and executable
symlink targets under the primary project and selected worktree, including both
roots for linked checkouts and requesters starting in a subdirectory. Lane
model/account identity and version probes, bare commands and native provider
launches share that PATH. Sandboxed launches prepend required private wrappers,
then retain requester PATH precedence before fixed tool-search fallbacks. PATH
entries do not add filesystem or executable grants.
Host delivery filters its captured host PATH through the same owner for tool
lookup. Explicit
program paths stay explicit. Graph retries capture PATH before changing state,
then save it with any retry patch under one journal lock. A capture failure
leaves the previous binding intact; a deliberately empty PATH remains a valid
captured value. Remote peers record their own PATH at ingress because origin
directories may not exist there.

Provider preparation may select an account-specific home or redirect state and
cache paths in either confinement mode; account selectors choose the registered
account and its required credentials. Credential values stay in the live launch,
never in prompts or receipts. Native launches skip skill copies, tool dependency
discovery, credential symlink inventory and the build-slot shim. Every lane still
binds the host Cargo home and private target/build directories before seed
validation.

Sandboxed lanes use the minimal environment and staged skills, discover tool
dependencies and credential aliases, and report inventory omissions in one
actionable warning. Cargo registry and Git sources are writable unless
`domains.fabric.lanes.cargo_source_writes` is turned off. Cargo lock and cache-use
tracking files have exact write grants; Cargo credentials are always denied.
The pnpm, npm and Node caches the host later runs unconfined stay read-only;
build output and tool configuration use private lane caches.
- Why: native lanes use ordinary tools and login state; sandboxed lanes keep
  source sharing explicit while keeping credentials and build output isolated.

### Network, IPC and process information

No lane reaches the peer agent's socket. Lanes may read process information
but never another process's arguments, and signals stay inside the lane's own
sandbox. SysV IPC is host-global on macOS, so same-user IPC isolation needs a
separate OS user or VM.
- Why: the peer agent relays to unconfined children, and Seatbelt can enforce
  no more.

### Lane process-tree memory interface

The process-tree interface reads a host-sampled snapshot of the lane's own
descendants without process-listing authority. A missing, stale, incomplete or
unsupported sample is unavailable, never a partial tree or zero memory.
The sample is periodic observation, not an atomic census or a memory limit.
`domains.run.resource_sample_interval_ms` controls CPU and process-tree sampling
(1..=60000 ms, compiled default 5000 ms). Each private snapshot records a
freshness allowance derived from that interval plus the bounded sample budget;
the CLI output and public sample schema do not change. The setting source is
available from `provenant config show --full`.
- Why: a memory guard that reads lost observation as zero would approve a
  runaway build.
- Owner: `crates/supervisor/src/process_tree.rs`, `src/process_tree.rs`

### Disclosure and safe Git evidence

A prompt snapshot with a known secret is refused before sealing, and output is
redacted across chunk boundaries before it reaches disk. Quoted credential values
are masked until a definite closing quote: one that every plausible escape layer,
from the opener's own up to the length of the backslash run, agrees is a close.
No fixed depth cap applies, because a value can be escaped one layer deeper than
any cap. Without a definite close, masking continues through the end of the raw line. Scanning resumes
after a definite close or the line end. An assignment value that is exactly
`[REDACTED]` stays unchanged across chunks and rescans; further value bytes are
masked. Marker recognition never changes the input to token scanning.
Host Git runs with
hooks, filters, fsmonitor and external programs off, and refuses configuration
from a lane-writable include. Host delivery pushes once, from the host, and
refuses credential-bearing URLs.
- Why: repository content and lane output are attacker-controlled input to the host.
- Owner: `crates/confinement/src/redact.rs`, `crates/confinement/src/utility.rs`

## Worktrees

### Managed writer path owner

`run --branch` uses primary root/ports regardless of cwd; relative
`--worktree` stays cwd-relative.
- Why: roots differ.
- Owner: `Context::open`, `bind_launch_controls`, `worktrees::open_primary_repo`

### One writer; removal never forces

Git's own inventory is the worktree registry. One writer holds each linked
worktree, and custody outlives the requesting call. `worktree remove` refuses
the primary, a live writer or observed process, and has no force. Dirty work
requires explicit discard, with its recoverable archive captured before removal;
it deletes a branch only with current landing evidence or explicit discard.
A persisted writer reservation remains a custody fence when its kernel lock is
absent: read-only occupancy is unknown until run-owned reconciliation settles
the attempt, and cache guards refuse the slot without clearing its evidence.
Named removal without current landing or subsumption proof returns
`WorktreeError::UnprovenLanding`, naming the branch and missing proof, with an
explicit-discard recovery command. `IdentityChanged` remains a changed-identity
or changed-observation refusal, not a substitute for missing landing proof.
- Why: removing a tree under a live writer destroys its work.
- Owner: `crates/confinement/src/worktrees.rs`

### Unreadable processes need positive non-occupancy evidence

A root is `Free` only when every possible writer was observed completely with
stable identity; a placed occupant makes it `Live`, and a failed or partial
query leaves it `Unknown`, never empty. A process whose real, effective and
saved uids are all stable system uids other than the root owner's is outside
the writer cohort; host exceptions are in
[configuration](../runbooks/configuration.md#unreadable-process-census-exceptions).
Landing and collection require `Free`; the census grants no signal authority.
- Why: a retained cwd or descriptor survives permission changes, so current
  permissions cannot prove absence.
- Owner: `crates/confinement/src/worktrees.rs`, `crates/os/src/sys/pid.rs`

### Pruning and GC need proof

Pruning proves subsumption or landing from declared retained-ref, tree-equivalent,
integration or forge evidence and revalidates it at mutation. Reference deletion
requires landed evidence and the expected old OID; worktree-only retirement
preserves the branch. GC keeps anything in doubt: a held writer lock keeps a
cache, and foreign, detached or idle worktree collection requires the declared
retention policy and complete current custody, run and process evidence.
Shallow history, conflicted comparisons, moved tips and unavailable evidence
keep the candidate.
Settlement chooses worktree-only retirement for retained-ref subsumption,
preserving the local branch; integration evidence may also retire the branch.
A pruning receipt is a locator, not removal authority. If the checkout is absent
and no durable retirement owns the receipt's reference, an applied GC sweep
retires the orphaned operation receipt with the typed
`missing_retirement_owner` cause and custody-unknown retention evidence. It
never certifies deletion from path absence. Existing checkouts are reconsidered
under fresh ownership and landing proof, and reference cleanup remains subject
to its normal independent proof checks. `provenant gc --apply` performs this
recovery without an absent owner holding the receipt in an endless retry.
Removal previews use the same cleanliness and ignored-output eligibility
checks as apply; a preview never reports dirty user data as removable.
- Why: GC runs unattended; pruning on a guess deletes the user's work.
- Owner: `crates/confinement/src/worktrees.rs`, `crates/confinement/src/cache.rs`
