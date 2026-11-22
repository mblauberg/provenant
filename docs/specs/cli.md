# Command surface

Owner: `crates/cli` (grammar and rendering), `src/` (composition and hooks)
Anchor: `crates/cli/src/lib.rs`, `crates/cli/src/render.rs`,
`crates/cli/src/wait.rs`, `src/hooks.rs`

`provenant help --all` is the reference for commands and flags, and
`provenant config schema --all` for configuration. `crates/cli/src/external_contract.rs`
declares the outward contract and `scripts/generate-reference` renders it; the
[consumer contract](consumer-contract.md) owns the stability tiers. Command
names may change freely, and direct readers of internal state are unsupported.
Repository CI selection, its gate and quarantine are specified in [CI](ci.md).

## Configuration schema completeness

Every setting a registered domain deserialises has field metadata and a
`config/harness-knobs.json` entry. A probe over each domain's serde types names
every declared field, whether or not a value can be built, and a test fails
when the metadata or the registry omits one.

- Why: three independently kept lists drift silently unless each is checked
  against what the runtime accepts.

## Targets

Target verbs share one grammar and resolve only the named graph. Ambiguity is
reported, never resolved by guessing; conflicting manifest settings are refused.
`TargetGrammar` declares each target argument, and its parser must accept and
refuse exactly the values of its declared syntax.

- Why: an action must address the intended work.

## Parsing

Help and version inspection need no project state. Paths keep their native
bytes, and invalid or conflicting arguments are rejected. The catalogue derives
flag relationships and child commands from the parser and declares each
command's configuration domains; every catalogued command resolves to scoped
help carrying that metadata. Clap parse errors never echo a fence token.

- Why: inspection must be safe outside a checkout and paths must not change in
  transit.

## Build commands

`provenant build` inspects and resets the host's build slots without reading
their files, and `provenant verify` runs a tier through the chair's one
host-owned warm check. Lanes never build. A skipped `build maintain` reports why
as a warning, never a failure, and a `build report` row whose affected-package
selection cannot be computed stays unknown.

- Why: the chair's warm check and CI are the only places that compile.

## Console catalogue and peer coverage

Console reads allocate no product state. Peer reads share one deadline and keep
the configured host order; a peer that fails or times out leaves local results
in place and is reported as incomplete coverage, never as complete.

- Why: a partial observation must not hide available work or imply complete
  coverage.

### Nested tree

The tree distinguishes dispatch, continuation and retry relationships, and
reports missing parents, gaps and incomplete usage as observations, not facts
about unseen work.

- Why: nested work must remain visible without inventing edges or totals.

### Board snapshot and cursor contract

A board cursor binds the selection and a high-water ceiling; replay reads
current facts within that ceiling. Damaged ancillary observations cannot hide
valid indexed rows, and missing usage stays distinct from zero.

- Why: an active board must be pageable without inventing observations.

## Results

### One bounded envelope, clipped with a warning

A command emits one terminal envelope whose `status` is the code the process
exits with. A result over the cap is bounded, not refused: the human view keeps
both ends with an omitted-bytes marker and a warning naming the cap, its source
and the repair. A result the owner can page is published once and the envelope
carries a pointer to it, never a truncated object; commands that warn and
proceed keep the result's status. A surface that keeps the refusal contract
instead emits a typed `RESULT_TOO_LARGE` Error envelope and exits 3. A surface
that cannot publish a result carries a bounded marker in `data` (`truncated`,
`omitted_bytes`, `source`, `repair`) with a warning, beside the result's typed
error. Paged reads bind the cursor to the input, terminal text escapes control
sequences, unknown facts stay unknown, and `--json` always preserves the envelope.

- Why: a script branches on the exit code, and an operator must see that output
  was bounded rather than mistake a clipped view for the whole result.

### Each refusal has a stable code

A refusal carries a stable typed code independent of message wording; the
command catalogue (`provenant help --all`) owns the codes. An output or trace
read outside a project returns `NOT_FOUND` and names running from a checkout or
passing `--project PATH` as the repair.

- Why: consumers can act on a refusal without depending on prose.

### Run refusals keep their own code at the facade

A run refusal that reaches the CLI or Fabric facade without a richer typed
projection keeps the run's own code (for example `DISK_FLOOR` or `LANE_CAP`) and
pairs it with a repair action in `next`. `confinement_refused` is reserved for a
real confinement boundary with a named resource. `writer_lease_held` is its own
code: locally it names the holding attempt or graph, branch, worktree,
reservation age and liveness, while peer projection keeps only the code and
repair action. A kind-only failure never loses its local diagnostic detail.

- Why: the operator can see and fix the cause instead of chasing an opaque
  confinement refusal.

### Template name errors

An unknown template or flavour names the supplied value and lists the matching
known names. Only validated template identifiers are disclosed; other graph
input errors keep their generic message.

- Why: a mistyped flavour should be repairable from the refusal without exposing
  arbitrary graph input.

### Waits are readers

Waiting observes work; a timeout or cancellation does not stop it. `wait`
defaults to `--all` (worst final result of every target); `--any` and
`--fail-fast` return early, and output names the mode. A target settles by its
own state, so an older settlement cannot satisfy a wait for new work.

Local graph waiters share one filesystem event source; when it is unavailable
the result says why and rereads at `domains.wait.local_interval_ms`. Peer and
relayed rows use the same paced reread because the relay has no remote
subscription. Every blocking forge operation inside a wait shares one request
context (the wait's absolute deadline, capped per read by
`domains.wait.pr_read_timeout_seconds`, and its cancellation), and a wait that
settles cancels the forge transport of its other targets.

Wait settings resolve in one place for every waiter. Configuration a waiter
cannot use warns and leaves the compiled defaults in force, and reports name
each setting with its source. Budgets clamp to `domains.wait.max_timeout_seconds`
with one warning, and a larger request stays a blocking wait. `wait --pr --until
complete` has its own budget (`domains.wait.pr_complete_timeout_seconds`) apart
from `domains.wait.timeout_seconds`; the landing gate and graph waits use
`domains.pr.land.gate.timeout_seconds` and
`domains.fabric.graph.launch_wait_seconds`. With `--json`, wait emits JSONL events
then a terminal envelope, and a lane wait that names no targets waits on the
lane's children through the owner relay.

- Why: a wait must not outlive its budget in a transport it can no longer
  interrupt, and one setting must not fail three different ways.

### Status selection

Status lists all active graph work and counts omitted settled work; a settled
graph with a live attempt is still listed. The headline leads with the live-lane
count (`live_lanes` in JSON). One state vocabulary serves the summary, rows and
`status NAME`: a live attempt is `running`, work waiting for admission or launch
is `pending`, and a non-live writer keeps its presence label. Human and
structured output share the selection.

`status --mine` reads active custody across projects (or one `--project`),
including attempts nested under the current chair and unattended items open to
adoption. Settled custody leaves the view, and the session-start digest reads
this view instead of extracting PRs from prose.

Writer presence is `live`, `unattended`, `settling` or `unknown`. `running` shows
only while the recorded guardian or provider identity is present. A changed boot
proves the old process tree is gone; same-boot absence, a reused PID, an
unobservable host, incomplete evidence and unreachable peers stay `unknown`,
because a child may have forked into another session and been reparented. Status
never mutates or schedules recovery; unattended and settling rows name the scoped
`provenant reconcile` action, which rechecks custody before settlement.

`status NAME --full` gives each unsettled attempt its newest event time and
names the source (`output`, `started` or `queued`), so old output is not mistaken
for a heartbeat. `child_command` is set only for a command that went through the
admission shim, and a lane waiting on a heavy-job lock reports the command it
waits to run. The attempt directory and child command stay host-local and never
cross a peer boundary.

`status` and `worktree list` report `stranded work` (`stranded_work` in
`status --json`): a worktree with uncommitted edits or an interrupted Git
operation and no live holder, with the `run implement --worktree` command that
adopts it. A clean worktree, one with a live lane and one whose holder cannot be
determined are not stranded. The section is advisory and never refuses a command
or signals a process.

`adopt`, `release` and `fabric claim` share one subject spelling. Run launch,
worktree creation, PR binding and landing, and cloud launch take custody
automatically and settlement releases it. A custody failure after a successful
external action warns and can be retried by adoption.

- Why: a chair acting on a listing must see work still running.

### Output references and bounded tails

`result NAME[/NODE[#N]]` reads current lane evidence in one call, and for a
review node projects the settled `verdict` and typed `findings[]` through the
projection `status` and `wait` read, not by parsing the result artifact again.
Missing or unavailable evidence is not proof of a check.

`output GRAPH` reads the newest settled attempt of a graph's one non-composite
top-level node. A composite root returns its declared bounded result and typed
exports without inventing an attempt identity, `output GRAPH/NODE` selects a
composite explicitly, and a bare graph with several top-level nodes is
ambiguous and lists them. Offsets and limits count bytes; a page that splits a
UTF-8 character returns lossless base64 and advances by the bytes returned.
`output` and `steer` resolve the same numbered attempt, and a clamped tail
reports its effective bound.

- Why: a reader must be able to inspect the intended attempt safely.

### Settled attempt provenance receipts

A receipt identifies a settled local attempt and reports only supported route
and reviewer attribution. Ambiguous, running or peer-owned targets are refused;
missing evidence stays unknown.

- Why: provenance must not mistake a writer for a reviewer or infer a route.

## Guards

### Merge guard

Landing verifies and merges the same head, and refuses changed heads or
incomplete required check evidence. A refused merge keeps the unified
`LAND_REFUSED` code with a typed `cause` (`HEAD_MOVED`, `MERGE_NOT_ALLOWED`,
`PR_DRAFT`) and a non-empty `repair`, so callers branch on the cause, not the
message.

- Why: a merge must land the head that CI verified.

### Landing adds no merge authority

Approval must cover the exact head and come from an authorised independent
source or an explicit owner override. Approval evidence is the union of the
configured sources; a new passing review on the pinned head cannot invalidate an
existing forge approval, and losing all current approvals still excludes the
member. Carried approval (`domains.pr.land.approval_carry`) survives a `pr sync`
merge only when the resulting tree equals Git's merge tree, and a formatter
carry only when formatting both heads gives the same tree; any other tree
change is refused, naming both heads and the changed paths. Graph landing uses
the same policy. The publication lock fences landing identity, generation and
snapshot revision before dispatch, so a superseded generation cannot publish,
including during recovery.

Repository, host, project, target-rule, risk-tier and invocation settings
resolve into one saved plan with sources (`land --explain --json`). An
unavailable execution returns its landing record ID and typed alternatives and
never silently selects another strategy. An explicit `poll_seconds` or
`land --poll` fixes the wait cadence even when it equals the bundled default;
source provenance decides, not the value.

The effective `PortTable.land` declaration alone selects the publication
backend. `none` returns typed unavailability before publication effects.
`command` receives the pending intent as JSON on stdin; exit 0 attests the
candidate landed, the declared `unknown_exit_code`, timeouts and signals are
unknown, and other exits refuse. An unknown outcome is observed through
candidate ancestry and exact-head forge evidence, never replayed or replaced by
a builtin publisher while it stays uncertain.

Cleanup after merge preserves moved branches, dirty worktrees and work not
proven merged, and reports incomplete cleanup.

- Why: routine landing must not widen approval or delete unmerged work.

`pr sync` merges each PR's base into its branch worktree and pushes. A conflict
is aborted and the worktree left clean; an `implement` repair lane is launched
unless `--no-lane`.

### Pull request bodies come from receipts

Pull request creation derives its claims from the settled attempt and delivery
receipt, and states absent evidence rather than inventing it.

- Why: a hand-built body can claim a head or route the receipts do not support.

### Shell commands use confinement

The hook validates its event envelope but does not decide authority from shell
syntax. The [hard boundaries](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md)
are enforced by the [permission policy](supervisor.md#permission-policy).

- Why: shell syntax rules cannot enforce those boundaries.

### Pre-push refuses data loss

The pre-push guard protects shared default-branch history and reports advisory
conflict markers.

- Why: shared history must not be deleted or rewritten accidentally.

### Worktree hooks respect ownership

Worktree hooks act through the worktree owner and cannot remove a worktree they
did not create or override the owner's refusal.

- Why: a hook must not widen removal authority.

### Checks report, never repair

Checks report findings and unavailable evidence without repairing the subject.
Release checks inspect the artifact to be distributed as well as Git evidence.

- Why: an inspection must not change its subject or certify an uninspected
  artifact.

### Release symlinks

A release symlink is portable only when its relative target stays inside the
repository and names a tracked regular file in the same tree. Absolute,
escaping, malformed, missing or untracked targets, and links to another symlink,
remain findings. Git targets resolve from each tree's entries, and current
worktree links are read without following them through the host filesystem.

- Why: every published link must have one inspectable target in the tree being
  checked, independent of host files or link-chain behaviour.
- Owner: `crates/cli/src/check/release.rs`

## CI observations

`pr status` reports each pull request's head, check rollup and failed check
names from one batched GraphQL read when available, and falls back between
GraphQL and REST when either is unavailable or rate limited. A wait's
conditional observation reports `review=unknown` because the REST response has no
review decision; CI settlement alone decides the verdict.

### Integration batch membership

`pr status` infers integration batches from bounded Git history: a member whose
head is contained in an open batch pull request's head, or in the local `land/`
candidate branch that `land --target` gates, reads `in batch #M (head merged); own
CI superseded` with the batch's CI state, and its cancelled checks are not
failures. A member whose head moved after batching is flagged `head moved since
batching`. `pr status M` on a batch lists its members and attributes each changed
file to the member whose commit last touched it. Membership is a report from
observed history: it adds no refusal and changes no landing decision.

- Why: a cancelled member run must not hide the batch CI that superseded it, and
  a review finding on the batch must be assignable to a member.

`wait --pr NUMBER` is the one way to block on a pull request. A failed check
settles it at once; `--until complete` waits for every observed check and every
active Actions run of the head. A head with no Actions run yet cannot settle a
wait, because a push's workflows start after the push: a reading that would
settle stays pending for `domains.wait.actions_grace_seconds`, though a failure
still settles a default wait at once. `--head SHA` pins the wait to one head:
until GitHub reports that SHA the reading stays pending, so a push's predecessor
never produces a verdict. Without `--head`, when the local tip differs from
GitHub's head, the wait warns and watches the local tip. The last line of output
names every target's settle state (`settled ok`, `failed`, `needs input` or
`still running at timeout`) and the one next command; the observation method
shows only under `--full` and in JSON.

`ci failures pr NUMBER` and `ci failures run ID` read each ended job's log from
the Actions jobs endpoint, so evidence is available when that job ends, not when
the slowest job does. An aggregate gate whose only bad inputs were cancelled is
a cancellation, never a failed job; when its inputs cannot be resolved it is a
failure with uncertain cause. A failure in a file the pull request did not touch
carries `possibly unrelated: PATH untouched by this PR`, and an unreadable file
list suppresses that advisory. A rerun is suggested, never executed, and an
unreadable log is reported as unreadable evidence, never as zero failures.
`pr status` and `wait --pr` carry the same digest.

### Wait/pr causes

A `wait --pr` reading of a head with no reported checks names why before it
suggests an action. The observer reads `mergeable` and `mergeStateStatus` from
the PR resource first and rereads an UNKNOWN mergeability a bounded number of
times (`MERGEABLE_UNKNOWN_RETRIES` in `src/pr/observer.rs`) before choosing:

- `merge_conflict`: GitHub reports the head as conflicting. It starts no
  `pull_request` workflows for a conflicting head, so the next step is to merge
  the base into the branch and push, not to start CI.
- `not_scheduled`: the head is mergeable, or mergeability stayed UNKNOWN. The
  next step is to check the workflow triggers or push an empty commit.

The cause and next step appear in the `wait` reading line and in `pr status`.
They are hints, not a settlement: a conflicting head stays pending until it is
merged with its base and its checks run.

- Owner: `src/pr/observer.rs`, `src/pr/status.rs`, `src/pr/status/batch.rs`, `src/status_wait.rs`, `src/ci.rs`
- Why: a chair needs actionable CI evidence without mistaking missing
  observations for success, and without blaming workflow triggers for a merge
  conflict.
