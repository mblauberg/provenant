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
`ResolvedConfinementPolicy` composes the host's optional restrictions, the lane's
network selection and the provider's authentication and offline preferences.
Admission and rendering consume this same resolution and retain restriction
sources. An absent optional setting leaves ordinary files, tools, local
services and network access open. Capacity settings warn by default; a sourced
configured stop remains distinct from a mandatory boundary refusal.

Every credential declaration is enforced independently. An exception for an
enclosing runtime, package or selected authentication root cannot open a nested
declared store. Selected authentication and its exact aliases retain only their
declared access. Link operations remain subject to credential, state, source
and Git boundaries without a blanket denial of ordinary hard links.

### Plans refuse unsafe roots

Confinement refuses traversal, symlinked state or lock leaves, declared
credential roots and grants enclosing them, broad home or tmp grants and
Git-internal roots. Credential declarations name concrete resolved paths and
their source: built-in secret stores, provider configuration and data stores,
every registered account's stores and the resolved Cargo home's credential
files. Roots carry declarations; launch preparation classifies built-in,
provider, host-variable and account declarations once, using the actual launch
source and cwd, scratch, and the derived writer cache, without unioning an
earlier project classification. Scratch and the writer cache are lane roots
just as the source and cwd are. Host state admission also consumes raw host
declarations and refuses state roots inside dedicated stores. A declaration
exactly at HOME does not displace Provenant's normal home-relative state tree;
exact credential declarations there retain their hold. This host-state rule
does not classify or narrow the later launch boundary.
Generic and sandbox credential refusals use `CredentialRoots::select_refusal`
to name the most specific applicable declaration and its origin: an exact path
match first, then the deepest enclosing or enclosed store, independent of
declaration order. Equal-depth ties use stable declaration fields.

Mandatory credential denials use `DenialRoot`, independently of grant
validation: denial paths must be absolute, normalised and below the filesystem
root. A denial may cover HOME; this never relaxes broad-grant validation.

A store containing no lane root retains whole-subtree protection. A store
strictly containing lane roots retains whole-subtree protection minus validated
lane, process-executable, runtime-read and tool-execution grants. Each carve-out
must be a strict descendant of the store and enclose no dedicated store.
An enclosed exact credential endpoint retains its independent denial; a grant
at the endpoint itself refuses. Runtime carve-outs confer read and execution only;
writes remain limited to lane roots and selected authentication refresh. In the
open profile, the renderer denies the store subpath with `require-all` and a
`require-not` carve-out for each admitted grant at its validated scope. Under an
allowlist profile, default deny protects the store outside those grants. The current production
renderer is open; this rule also defines the boundary for an allowlist profile.
Metadata on ancestors of admitted paths remains available for executable and
dynamic-loader traversal; directory contents and extended attributes stay denied.
These metadata exceptions do not add read or execution authority over siblings.
Undeclared store files outside validated carve-outs remain denied in both profiles;
files inside a lane root are accessible by design. A store equal to or inside
a lane root refuses the launch. A provider can use its admitted runtime files
and lane-private writable state while unlisted store files remain inaccessible.

Adapter and host file inventories, including Provenant-owned per-account lane
tokens and refresh locks, serve only to check that no declared credential file
lies inside any lane root. Such a file refuses the launch, naming its origin
and path. Both the declared path and its resolved target are checked against
every declared and resolved lane root, so neither direction of a symlink can
evade the boundary. An inventory does not replace or narrow the store denial. Stores without an inventory retain the
same subtree boundary. A HOME-rooted store may enclose default state: the store
is a protection rather than an authentication grant, and state retains its
independent mandatory protection. Independent sibling profiles and file
declarations keep exact scope. No grant may enclose a store or open
a store outside its validated carve-outs. Read and execution grants may enclose
exact credential files because their independent denials still apply; writes
and lane authority enclosing a declared credential file refuse. Input
materialisation also refuses enclosing endpoints: staged copies cannot inherit
the original path's sandbox denial. Selected authentication retains its
explicit exact-file access; a broad authentication or configuration grant
cannot reopen a shared store. Adapters declare the selected identity's exact
authentication endpoints, including database sidecars when required, and stage
only those endpoints into private authentication state. Credential read aliases
are derived from exact endpoint declarations after shared-store filtering;
a discarded subtree read cannot confer alias access. Directory links under a
store also deny the resolved target subtree when it is outside HOME. A target
overlapping lane authority refuses inventory; runtime authority overlapping the
target refuses compilation. Refusals identify the link origin and target path.
Declared credential denials follow grants in the rendered profile. The
standalone Codex release package read exception belongs only to the built-in
default Codex home; dedicated relocated and registered stores retain whole-store
protection. Shared stores use the validated carve-out rule above. The exception confers no writes and never exempts another
overlapping credential denial.

Every declaration is kept, whatever its order or kind; each spelling of a
declared location carries every declaration made at or beneath any spelling of
it, so a store registered at a Codex home or its alias stays refused.

Skill snapshots use `CredentialRoots::skill_entry_refusal` for each entry and
resolved link target. The selected skills directory may sit beneath an enclosing
provider store; that permits ordinary skills only within the selected source.
Every independently declared nested store and exact endpoint remains excluded,
including declarations for unselected accounts and their resolved aliases.
Directories containing declared endpoints may supply ordinary siblings, but
neither the endpoints nor links to them may enter staged setup bytes. Selecting
a declared store itself does not exempt it. Provider-declared sibling profile
files retain exact scope for registered and default account directories.

A component named `.claude`, `.codex`, `.cursor` or `.gemini` does not establish
a credential boundary. Project-local agent configuration overlaps warn with
their source and proceed. A verified linked worktree is admitted at its real
path, including beneath project-local agent configuration or outside the
project. A declared store may strictly contain that root under the carve-out
rule above; equality, enclosed stores and enclosed credential files refuse.
Git markers, common
directories and private worktree Git directories keep their hard hold.
`run --explain` and launch preparation use one source admission decision and
report its refusal code and reason or sourced warnings.

Unsupported confinement fails honestly and never widens a selected profile's
access. The mere presence of an inherited OS sandbox does not prove equivalent
credential, Git or read-only protection. A trusted route selected by the host's
default or an explicit off setting can retain an existing OS sandbox without a
new profile when neither explicit restrictions nor an active parent floor
require one; its receipt reports inherited enforcement.
Failure to enforce a selected profile cannot choose that route implicitly.
An optional root that resolves into a declared
credential root is omitted with a warning.

Registered Git projects' declared worktree allocation roots remain protected
from extra host write grants, including before those roots exist. Resolve the
project port table from the verified primary checkout in Git's worktree
inventory; the common Git directory's parent does not establish that owner.
- Why: one broad grant silently voids the boundary.
- Owner: `crates/core/src/roots.rs`, `crates/confinement/src/permissions.rs`,
  `crates/cli/src/run/provider/confinement.rs`, `crates/providers/src/skill_stage.rs`

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

No lane reaches the peer agent's socket. Ordinary system and process metadata
queries remain open. Arguments, task information and memory from outside the
lane's sandbox stay private; the cross-process pidinfo guard is required to
enforce the argument boundary. Same-sandbox process queries and signals remain
available. SysV IPC is host-global on macOS, so same-user IPC isolation needs a
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
available from `provenant config show`.
- Why: a memory guard that reads lost observation as zero would approve a
  runaway build.
- Owner: `crates/supervisor/src/process_tree.rs`, `src/process_tree.rs`

### Disclosure and safe Git evidence

Disclosure is selected by destination, not transport. Authorised execution
context remains lossless; external publication projects known credential values
and typed secret fields, and withholds publication if projection fails. Raw and
exported bytes have separate lengths and digests.

Provider stdout and stderr are retained losslessly within bounded, host-owned
private capture. Provider JSONL frames are decoded without rewriting their bytes;
malformed retained frames remain evidence defects. Capture and local settlement
do not modify provider continuation state.
- Owner: `src/lib.rs`, `crates/run/src/holder.rs`

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
