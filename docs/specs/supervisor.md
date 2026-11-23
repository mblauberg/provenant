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

### Custody, not activity

The guardian does only what a provider cannot do for itself: start it with its
environment, wait for its exit on events, end the lane's own process tree,
record exit and settlement durably, and hold the writer, credential and
authority boundaries. It never judges activity (CPU, output silence, progress),
samples disk or matches command names.
- Why: one guardian runs per lane, so every periodic read multiplies across
  parallel lanes.
- Owner: `crates/supervisor/src/guardian.rs`

### Closed stop reasons; natural exit wins

Only `ProviderGroup::stop(StopReason)` signals a provider, and `StopReason` is
closed: cancellation, steering, the wall-clock deadline, a writer's token
budget or context handoff threshold, protocol rejection, a supervisor signal
and post-terminal cleanup. Silence, inactivity, capture failure, ENOSPC and
failed publication are never stop reasons, and there is no startup deadline. A
natural exit wins over a late cancel or deadline, and host sleep cannot extend
the deadline.
- Why: a bookkeeping fault or slow start must never kill a healthy provider.
- Owner: `crates/supervisor/src/machine.rs`

### Signals need proven identity and scope

The PID stays reserved until the handle is reaped last. Only these may be
signalled: the unreaped provider, its group while the leader is unreaped,
descendants with retained proof of provider ancestry, and escapees proven by the
attempt's sandbox mark. Every signal revalidates the full native identity. A
census listing or cwd observation never grants signal authority.

On macOS, however the provider ends (natural exit included), cleanup ends the
lane's own tree with TERM, then KILL after the descendant grace, so background
services an unsandboxed lane starts end with it. A live graph child is a
separate attempt, never a cleanup candidate, and cannot withhold a lane's
signals or custody release. Recovery after guardian loss signals only recorded
descendants whose identity still matches and live processes carrying the
sandbox mark.

A launch is named by its attempt's `WorkKey`; the launch summary records the key
and the launching build's version and source digest. A supervisor restart finds
a live writer by the key through the attempt row.
- Why: signalling a recycled PID or a sibling's group kills unrelated work, and
  a lane's backgrounded loops must not outlive it.
- Owner: `crates/supervisor/src/process_os/`, `crates/supervisor/src/custody.rs`

### Read-only writer presence

Status reports a writer live only when the guardian's or provider's exact
recorded identity (boot, PID, native start) is present and non-zombie. A changed
boot proves the old tree gone. Same-boot absence, missing or malformed
checkpoints, reused PIDs, inaccessible processes and incomplete censuses are
unknown, because the checkpoint can predate a fork the census cannot identify
after reparenting. A sealed process exit reports settlement pending. Status only
projects these facts; scoped reconciliation rechecks custody before changing
attempt state.
- Why: a dead guardian or recycled PID cannot hide a surviving provider child,
  and uncertainty must not become a running writer or an unsafe retry.
- Owner: `crates/supervisor/src/custody.rs`, `crates/run/src/controls.rs`

### Tick-driven settlement

`provenant tick` queues recovery for settling and unattended attempts in every
registered project from one bounded process census per pass; a binding whose
common directory is gone is skipped, and a failed project never blocks the
others. A sealed exit stays settlement evidence when the census
is unavailable. Unknown attempts get proof-only recovery: the original guardian,
custody and attempt holder locks and a fresh empty worktree census must prove
absence before same-boot settlement, and unknown evidence never clears custody.
Repeated ticks reuse the recovery backoff, worker marker and native locks. The
guardian persists a failed terminal publication in the scoped settlement
journal, so status explains the pending settlement without stderr.
- Why: losing the guardian must not lose the automatic path to a terminal
  attempt.
- Owner: `src/tick.rs`, `crates/run/src/controls.rs`,
  `crates/supervisor/src/settlement_progress.rs`

### Explicit abandonment

`provenant reconcile --abandon` lets a chair abandon a nonterminal writer with a
reason only when its presence is not live, its native writer lease is free and
the worktree process census reports `Free`. Same-boot unknown presence needs
both exclusion and positive non-occupancy evidence. Live presence, held
exclusion and any non-free or incomplete occupancy refuse with typed blocking
evidence. This does not change the proof automatic reconciliation needs.

The loss intent is durable before the attempt is committed `lost`; repeating an
interrupted abandonment with the same reason finishes it, and an incomplete
intent stays visibly settling. Abandonment warns and retains all attempt
evidence, scratch, worktree contents and private Git. A never-dispatched queued
attempt keeps its evidence-preserving cancellation.
- Why: an incomplete process census must not become permission to reuse a
  worktree.
- Owner: `crates/run/src/settlement.rs`, `crates/cli/src/run/reconcile.rs`

### Guardian holds custody past its owner

The guardian keeps the provider, attempt lock and writer lease after its owner
dies. Secrets travel in memory and pipes, never argv or disk. A missing launch
acknowledgement is an unknown launch, never permission to launch again. Writer,
custody, port and directory descriptors close on provider exec; only explicit
executable inputs (secrets, confinement profiles) cross, so a service a provider
detaches cannot retain the lane's writer lease.
- Why: losing the caller must not lose custody or double-launch a writer.
- Owner: `crates/supervisor/src/guardian/`

### Quiescence before release

A writer stays fenced until the provider is reaped, two complete censuses show
no surviving member or escapee, and a fresh lock on the original holder inode
succeeds. Failed, racing or incomplete reads keep the fence; only a recorded
decision with matching authority clears one, and that decision permits no
signal. Guardian loss leaves the durable unresolved reservation fenced against
every new attempt even when the kernel lease is free.

At the first cleanup deadline one diagnostic census per guardian names
observable PIDs, command names and open writer-lock paths with a reconcile
action. An open path is not proof of lock ownership; unknown holders stay
explicit uncertainty; a timeout alone never authorises settlement or an
unrelated signal.
- Why: a leftover descendant could still write the worktree.

### Retention is bounded and event-driven

While retaining unresolved custody the guardian blocks on process, cancellation
and signal events and costs no CPU between them. Its reactor latches every
kqueue edge until the loop that acts on it takes it, and a supervisor signal
stays visible to every later loop. A supervisor signal during cleanup or
retention gets one more bounded stop (TERM, grace, KILL); then the guardian ends
retention and exits. Retrying an undurable reservation or exit publication
stops after a bounded time once a supervisor signal has latched; the previous
reservation or the exit's `publication_pending` marker stays the fence.
- Why: kqueue reports each edge once, and retained custody can last hours with
  one guardian per lane.

`domains.run.retained_custody_seconds` bounds retention. The launcher resolves it
because the guardian cannot read config; unreadable run settings warn
(`LAUNCH_RETAINED_CUSTODY_LIMIT_OR_WARN_DEFAULT`) and keep the default. The
unresolved exit carries a `retention` record (guardian identity, start, limit)
and `provenant status` names the guardian only while its exact identity is
alive; otherwise it points to `provenant reconcile`. When retention ends with the
exit unresolved, the `retention` record adds an `ended` object with the reason, time
retained and a diagnostic blocker (`RetentionBlocker`). Past the limit the
guardian exits `timeout` with code `RETENTION_LIMIT`; other guardian faults use
`SUPERVISOR`. Ending retention signals and removes nothing: the unresolved exit
and reservation keep the writer fenced, and `provenant reconcile` ends only
proven orphans.
- Why: an orphaned guardian must not hold a lane indefinitely, and the fence,
  not the guardian, protects the worktree.

### Durable exit publication

The initial exit record is published without replacing an existing one, after
what it references is durable. Run pins an immutable provider-exit snapshot
before committing its outcome; later cleanup may replace only the process-exit
observation. A failed publication keeps custody and retries; an unconfirmed sync
is `durability_unconfirmed`, never absent. Captures and frames are bounded; an
oversized frame is decoder loss, never a fabricated terminal event.
- Why: a lost or overwritten exit record loses the only proof of outcome.

### Writer reservation and settlement

The writer reservation is published before spawn and cleared only after
quiescence and run's terminal commit, or an authorised reconcile; the worktree
flock stays held through import and [publication](run.md#publication-order). A
missing or malformed reservation refuses automatic recovery of that attempt;
[explicit abandonment](#explicit-abandonment) instead requires native exclusion
and positive worktree non-occupancy. Scratch deletion needs a verified
settlement commit.
- Why: a writer must never be freed on a partial or forged settlement.

### Stable locks and atomic writes

Lock inodes are never unlinked while held, and acquisition never revokes or
steals a lease. Authoritative writes are private temporaries, synced, renamed
atomically and followed by a parent sync. Owner records carry no PID-derived or
replayable authority.
- Why: a recreated lock inode lets two owners hold one lock, and a crash must
  leave the old or new file whole.
- Owner: `crates/os/src/fs.rs`

### Build context

Lanes have no Cargo shims and receive no host build context: they never
compile, seed or admit a build. The chair's host-owned warm check and
`provenant verify` tiers use the one `slots` strategy, bounded by
`[admission.build].slots`; layered `build.*` ports are in
[state](state.md#configuration). `.worktrees/.cargo/config.toml` is the single
mechanism that disables lane cargo.
- Why: one build model keeps lane custody independent of host compilation.
- Owner: `src/admit.rs`; `src/verify.rs`; `crates/run/src/build/`

## Permission policy

Lane profiles start open and deny the boundaries
([ADR 0040](../adr/0040-open-lane-policy-with-explicit-boundaries.md)).
`ResolvedConfinementPolicy` composes the host's optional restrictions, the
lane's network selection and the provider's authentication and offline
preferences; admission and rendering consume the same resolution and retain its
sources. An absent optional setting leaves files, tools, local services and
network open. Capacity settings warn by default; a configured stop stays
distinct from a mandatory boundary refusal.

`lanes.access` ([ADR 0042](../adr/0042-lane-access-host-by-default.md)) selects
credential treatment. `host` (the default) uses native provider homes, skill
discovery and login state, and a credential overlap warns with path, access and
source, then continues. `isolated` uses private per-attempt state and refuses
overlaps. Under isolated access every credential declaration is enforced
independently: an exception for an enclosing runtime, package or authentication
root cannot open a nested declared store, and a lane cannot hard-link a file in a
credential root, the shared Git control directory or another lane's worktree.

OpenCode under host access attaches to the host service when available and
resumes by recorded session ID in the host store; an unavailable prior session
warns and starts fresh, and without a service it warns and runs standalone.
Attached tools run in the host service process, outside the lane's confinement;
standalone tools stay under the lane profile. The client needs loopback access.

### Host inventory problems stay local

Host confinement inventories registered projects, worktree allocations, owned
worktrees and writer custody. A binding whose common directory is gone protects
nothing and is skipped by launch confinement, settlement and forge polling. A
present project whose layout cannot be read warns through launch preparation and
protects that project's root, so it never refuses an unrelated launch. An
unusable sibling entry is protected as a denied root and reported through the
host warning channel with its path and reason; it never refuses an unrelated
launch. If the entry's directory cannot be named, inventory refuses with that
path and stage. The selected writer's own slot stays fail-closed, including
unresolved guardian evidence, until reconciled.
- Why: retained sibling evidence must not become a host-wide launch outage.
- Owner: `crates/confinement/src/permissions/policy/host.rs`,
  `crates/supervisor/src/custody.rs`

### Plans refuse unsafe roots

Confinement refuses traversal, symlinked state or lock leaves, grants that
enclose a declared credential root, broad home or tmp grants and Git-internal
roots. Credential declarations name concrete resolved paths and sources:
built-in secret stores, provider configuration and data stores, every
registered account's stores and the resolved Cargo home's credential files. For
HOME-rooted accounts only the adapter's client state and declared credential
files are stores, never the whole home. `CredentialRoots::select_refusal` names
the most specific declaration (exact match, then deepest enclosing or enclosed
store, independent of declaration order). Every declaration is kept whatever its
order or spelling.

Under isolated access:
- A store, or a declared credential file from an adapter or host inventory
  (including per-account tokens and refresh locks), equal to or inside a lane
  root refuses the launch, naming origin and path. The declared path and its
  resolved target are both checked, so neither symlink direction evades it.
- A store holding no lane root keeps whole-subtree denial. A store enclosing
  lane roots keeps it minus validated carve-outs for lane, executable,
  runtime-read and tool-execution grants. Each carve-out is a strict descendant
  of the store, encloses no other store, and confers read and execution only;
  writes stay limited to lane roots and selected authentication refresh.
- An enclosed exact credential endpoint keeps its denial and a grant at the
  endpoint refuses. Read and execution grants may enclose an exact credential
  file; writes and lane authority enclosing one refuse.
- Selected authentication keeps exact-file access. Adapters declare the selected
  identity's exact endpoints (database sidecars included) and stage only those
  into private authentication state; a broad configuration grant cannot reopen
  a shared store. Input materialisation refuses enclosing endpoints, since
  staged copies lose the sandbox denial.
- A directory link under a store also denies its resolved target outside HOME;
  an overlap with lane authority refuses.
- Skill snapshots use `CredentialRoots::skill_entry_refusal`: every declared
  nested store and exact endpoint stays out of staged setup bytes, including
  unselected accounts' declarations and their aliases.
- Mandatory denials render from `DenialRoot` independently of grant validation
  and may cover HOME without relaxing broad-grant validation. Ancestor metadata
  stays available for traversal; contents and extended attributes stay denied.

Under host access an overlapping or unresolved credential root, and an optional
project path inside one, warns with path, access and source and continues; host
profiles omit credential, keychain and process-inspection denials. A component
named `.claude`, `.codex`, `.cursor` or `.gemini` is no credential boundary, and
overlap with project-local agent configuration warns only when
`policy.CONFINEMENT_SOURCE_AGENT_CONFIG_NONE` is enabled. A verified linked
worktree is admitted at its real path wherever it lives. Git markers, common
directories and private worktree Git directories keep their hard hold. Declared
worktree allocation roots stay protected from extra host write grants, even
before they exist.

Unsupported confinement fails honestly and never widens a selected profile. An
inherited OS sandbox does not prove equivalent protection; a route chosen by
host default or explicit off may keep one only when no explicit restriction or
parent floor requires a profile, and its receipt reports inherited enforcement.
`run --explain` shares these checks and reports credential admission as
`pending_route` until launch preparation resolves the route's access.
- Why: one broad grant silently voids the boundary.
- Owner: `crates/core/src/roots.rs`, `crates/confinement/src/permissions.rs`,
  `crates/cli/src/run/provider/confinement.rs`, `crates/providers/src/skill_stage.rs`

### Shared Git metadata is read-only

Each writer owns an attempt-private Git view with separate refs, index, objects,
configuration and credentials, using the repository object store as a read-only
alternate. The repository's refs, config, alternates, hooks and admin metadata
stay read-only to lanes. The host imports verified objects and moves the branch
by compare-and-swap ([launch custody](run-launch.md#admission-and-custody)). The
lane Git wrapper binds commands that implicitly target its worktree; commands
with an explicit repository selector, other repositories and transport endpoints
pass through unchanged.
- Why: ordinary Git works in the lane while other lanes' refs and the host's Git
  behaviour stay protected.

### Native environment and sandbox tool stores

`base_env` inherits the host environment under host access, sandboxed or not;
isolated sandboxed lanes get a minimal reconstructed one and
`strip_hosted_credentials` filters hosted-cloud variables. Sandboxed lanes use a
private TMPDIR. `FilteredLanePath` owns the lane PATH: the retained requester
PATH, else the captured host search path, else the process PATH only without a
host handoff, minus directories and executable symlink targets under the primary
project and selected worktree. Account and model probes, bare commands and
native launches share it, and PATH adds no filesystem or executable grant. Graph
retries capture PATH before changing state and save it with the retry patch under
one journal lock; a capture failure leaves the previous binding intact. Remote
peers record their own PATH at ingress.
`HostLaunchContext::provider_path_environment` captures the SSH agent selector
across custody; host access under a sandbox grants the exact resolved socket.

Provider preparation may select an account-specific home or redirect state and
cache paths in either mode; credential values stay in the live launch, never in
prompts or receipts. Native launches skip skill copies, tool dependency
discovery and credential symlink inventory. Isolated sandboxed lanes stage
skills, discover tool dependencies and credential aliases, and report omissions
in one warning. Cargo registry and Git sources are writable unless
`domains.fabric.lanes.cargo_source_writes` is off; Cargo credentials are denied
under isolated access. Package caches the host later runs unconfined (pnpm, npm,
Node) stay read-only; build output uses private lane caches.
- Why: native lanes use ordinary tools and login state; sandboxed lanes keep
  source sharing explicit and build output isolated.

### Network, IPC and process information

No lane reaches the peer agent's socket. Ordinary system and process metadata
queries stay open; arguments, task information and memory from outside the
lane's sandbox stay private. Same-sandbox process queries and signals remain
available. SysV IPC is host-global on macOS, so same-user IPC isolation needs a
separate OS user or VM.
- Why: the peer agent relays to unconfined children, and Seatbelt can enforce no
  more.

### Lane process-tree memory interface

The interface reads a host-sampled snapshot of the lane's own descendants
without process-listing authority. A missing, stale, incomplete or unsupported
sample is unavailable, never a partial tree or zero memory; the sample is
periodic observation, not an atomic census or a memory limit. Sampling runs only
when `domains.run.resource_sample_interval_ms` is set or an admission class has
`memory_per_slot`; otherwise `PROVENANT_PROCESS_TREE` is unset. The launcher
resolves the interval for the guardian, which reads no config; unreadable run
settings warn (`LAUNCH_PROCESS_TREE_INTERVAL_OR_WARN_NONE`) and launch without a
sampler.
- Why: a memory guard that reads lost observation as zero would approve a
  runaway build.
- Owner: `crates/supervisor/src/process_tree.rs`, `src/process_tree.rs`

### Disclosure and safe Git evidence

`redaction` is one sourced setting, `off` by default
([ADR 0035](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md)).
An unrecognised value warns and falls back to `secrets`, naming the restrictive
value; only an absent value takes `off`.
Under `off` nothing is masked and no per-field warning appears; `config show`
reports the value and source. `secrets` masks credential values and token
shapes in text leaving the host: GitHub publication, cross-host peer and relay
projection, fixture export and configuration history. Local output and
diagnostics stay raw under both modes, and model, effort, adapter, alias, name,
tier, account label, source, pool, path, worktree, stage, graph and node names
are never hidden. Raw and exported bytes carry separate lengths and digests,
and a failed projection withholds the affected publication. In-memory Debug
masking, fence-token argv masking, the GitHub token receipt and chair checkpoint
scoping are unconditional and are not output policy.

Provider stdout and stderr are retained losslessly in bounded host-owned private
capture, and JSONL frames are decoded without rewriting their bytes; malformed
retained frames are evidence defects. Capture and local settlement never modify
provider continuation state.

Host Git runs with hooks, filters, fsmonitor and external programs off and
refuses configuration from a lane-writable include. Host delivery pushes once,
from the host; under isolated access a remote URL with userinfo, query or
fragment is refused, and under host access it warns.
- Why: repository content and lane output are attacker-controlled input to the
  host.
- Owner: `src/lib.rs`, `crates/run/src/holder.rs`, `crates/redact/src/lib.rs`,
  `crates/confinement/src/publication.rs`, `crates/confinement/src/utility.rs`

## Worktrees

### Managed writer path owner

`run --branch` resolves root and ports from the primary checkout whatever the
cwd; a relative `--worktree` stays cwd-relative.
- Why: the two roots differ.
- Owner: `Context::open`, `bind_launch_controls`

### Shared occupancy; removal never forces

Git's own inventory is the worktree registry, and occupancy is a shared lease:
any number of lanes may hold one worktree, path claims are advisory, and a live
conflicting writer is not excluded by Provenant. Attempts own separate Git refs,
indexes and object stores, so a commit cannot take a sibling's staged snapshot.
Host publication uses expected-old-tip compare-and-swap after verification and
retains divergent private commits. Each attempt holds its own holder lease;
proof that a sibling settled never releases another attempt's custody. An
unscoped budget commit needs durable proof that no sibling shared the worktree;
shared attempts report typed leftover paths instead.

Removal needs exclusive occupancy, zero live holders and no unresolved
reservation, and terminal outcome publication alone does not release custody.
Under exclusion removal checks native cleanliness and every recorded attempt
index; staged work or uncertain index identity retains the checkout.
`worktree remove` refuses the primary, a live writer or an observed process, and
has no force. Dirty work needs explicit discard with its recoverable archive
captured first; a branch is deleted only with current landing evidence or
explicit discard. A persisted writer reservation stays a custody fence while its
kernel lock is absent, until run-owned reconciliation settles the attempt. Named
removal without current landing or subsumption proof returns
`WorktreeError::UnprovenLanding`, naming the branch and missing proof, with the
explicit-discard command; `IdentityChanged` means a changed identity or
observation, never missing landing proof.
- Why: removing a tree under a live writer destroys its work.
- Owner: `crates/confinement/src/worktrees.rs`

### Unreadable processes need positive non-occupancy evidence

A root is `Free` only when every possible writer was observed completely with
stable identity; a placed occupant makes it `Live`, and a failed or partial
query leaves it `Unknown`, never empty. A process whose real, effective and
saved uids are all stable system uids other than the root owner's is outside the
writer cohort
([exceptions](../runbooks/configuration.md#unreadable-process-census-exceptions)).
Landing and collection require `Free`; the census grants no signal authority.
- Why: a retained cwd or descriptor survives permission changes, so current
  permissions cannot prove absence.
- Owner: `crates/confinement/src/worktrees.rs`, `crates/os/src/sys/pid.rs`

### Settled dwellers are reported, never signalled

`gc` (preview and apply), `hosts doctor` and `worktree doctor` name every
process one bounded census finds inside a terminal lane worktree or settled
attempt directory, with pid, command and whether a cwd, executable image or open
descriptor placed it there. An attempt contributes roots only while its run is
`Terminal` and its custody released, through one shared filter. The three
commands share one census budget and exclude the caller's session and verified
ancestors. A bounded census cut short, or with an incomplete listing, warns that
results may be missing instead of reading as no dwellers, and never refuses the
command; a local report caps its held rows, says how many it omitted and still
passes peer validation. `worktree doctor` keeps unbounded live-writer and
`census_unknown` classification, adding held rows only for terminal worktrees.
- Why: an orphan must be visible without turning a cwd or descriptor hint into
  permission to kill an unrelated process, and a partial census must never look
  clean.
- Owner: `crates/confinement/src/worktrees/census.rs`,
  `crates/confinement/src/worktrees/doctor.rs`, `src/worktrees.rs`,
  `src/gc_support.rs`, `src/hosts.rs`

### Pruning and GC need proof

Pruning proves subsumption or landing from declared retained-ref,
tree-equivalent, integration or forge evidence, and revalidates it at mutation.
Reference deletion needs landed evidence and the expected old OID; worktree-only
retirement preserves the branch, and retained-ref subsumption always chooses it.
One retirement scope names the default branch from the fetched upstream,
`origin/HEAD` or `main`, and every checked-out branch. No GC path retires or
modifies the default branch: the scope protects local and remote ledger records
even when no target was fetched, and a linked worktree checked out on the
default branch is never retired. Excluded branches report as Retained with a
reason instead of dropping out of the preview.
GC keeps anything in doubt: a held writer lock keeps a cache, and foreign,
detached or idle worktree collection needs the declared retention policy and
complete current custody, run and process evidence. Shallow history, conflicted
comparisons, moved tips and unavailable evidence keep the candidate. Removal
previews use the apply checks and never report dirty user data as removable.

A pruning receipt is a locator, not removal authority. When the checkout is
absent and no durable retirement owns the receipt's reference, `provenant gc
--apply` retires the orphaned receipt with cause `missing_retirement_owner` and
custody-unknown retention evidence; it never certifies deletion from path
absence, and existing checkouts are reconsidered under fresh proof.
- Why: GC runs unattended; pruning on a guess deletes the user's work.
- Owner: `crates/confinement/src/worktrees.rs`, `crates/confinement/src/cache.rs`
