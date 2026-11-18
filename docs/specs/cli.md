# Command surface

Owner: `crates/cli` (grammar and rendering), `src/` (composition and hooks)
Anchor: `crates/cli/src/lib.rs`, `crates/cli/src/render.rs`,
`crates/cli/src/wait.rs`, `src/hooks.rs`

Use `provenant help --all` for commands and `provenant config schema` for configuration.
The outward contract is declared in `crates/cli/src/external_contract.rs`, covering commands, lane environment and JSON schemas.
`scripts/generate-reference` renders `external-contract.md` from that declaration through `provenant-cli print-reference-data`; the same data derives threshold behaviour from the policy table. Every enumeration an owner declares for the editor, such as `gc.projects.{project}.foreign_worktrees`, publishes its values and runtime default through the same data; the configuration reference fills them in and refuses an entry that restates them.
The [consumer contract](consumer-contract.md) gives each command, output and variable its tier: the few stable ones hold across 2.x, and the rest evolve with release notes; direct internal-state readers are unsupported.
Conditional PR wait observations report `review=unknown` because the REST response has no GraphQL review decision; CI settlement still determines the wait verdict. One-shot `pr status` keeps its detailed review observation.
Repository CI reads its tier declaration from `config/ports.json`; the optional `.provenant/ports.json` entry links that same table, so running repository checks requires neither an installed Provenant nor a `.provenant` directory.

## Targets

Target verbs share one grammar and resolve only the named graph. Ambiguity is
reported, never resolved by guessing; conflicting manifest settings are refused.

- Why: an action must address the intended work.

## Parsing

Help and version inspection require no project state. Paths retain their native
bytes, and invalid or conflicting arguments are rejected.
The catalogue derives flag relationships and child commands from the parser, and declares each command's configuration domains.

- Why: inspection must be safe outside a checkout and paths must not change in transit.

## Console catalogue and peer coverage

Console reads do not allocate product state. Peer observations retain local
results and identify incomplete coverage rather than presenting it as complete.
At most eight peer reads run concurrently under one shared deadline, retaining configured host order.

- Why: a partial observation must not hide available work or imply complete coverage.

### Nested tree

The tree distinguishes dispatch, continuation and retry relationships, and
reports missing parents, gaps and incomplete usage as observations, not facts
about unseen work.

- Why: nested work must remain visible without inventing edges or totals.

### Board snapshot and cursor contract

A board cursor binds the selection and a high-water ceiling; replay reads
current facts within that ceiling. Damaged ancillary observations cannot hide
valid indexed rows, and missing usage remains distinct from zero.

- Why: an active board must be pageable without inventing observations.

## Results

### One bounded envelope, never truncated

A result is whole or refused before output. Paged reads bind the cursor to the
input, and terminal text escapes control sequences. Unknown facts stay unknown.
Compact output uses semantic owner summaries and a detail command; JSON preserves the complete envelope.

- Why: consumers must not mistake partial or forged output for a complete result.

### Error codes

### Each refusal has a stable code

A refusal carries a stable typed code independent of message wording. The
command catalogue owns the list of codes and their meanings.
An output or trace target read outside a project uses one `NOT_FOUND` code
prefix, explains that no project was found, and names running from a checkout
or passing `--project PATH` as the repair.

- Why: consumers can act on a refusal without depending on prose.

### Template name errors

An unknown template or flavour names the supplied template and lists known
matching template names or flavours. Only validated template identifiers are
disclosed; unclassified graph input errors keep their generic message.

- Why: a mistyped flavour should be repairable from the refusal without exposing
  arbitrary graph input.

### Waits are readers

Waiting observes work; timeout or cancellation does not stop it. `wait` defaults
to `--all`, which waits for every target and returns the worst final result.
`--any` returns at the first target that settles or needs input.
`--fail-fast` returns at the first unsuccessful target, or after every target
settles successfully or needs input. Human output names the selected mode and
the terminal JSON envelope includes it. A target
settles by its own state, so an older settlement cannot satisfy a wait for new
work. Local graph waiters share one filesystem event source (inotify or kqueue)
for settlement logs and store changes. When that source is unavailable, the result reports why
and rereads at `domains.wait.local_interval_ms`. Peer or relayed rows also use
paced rereads at that setting because the relay socket has no remote event
subscription; the result identifies that source. Pull requests use forge
observation. Settlement events are emitted as they are observed. Expiry ends blocking observation. Local stores take one final snapshot; remote reads use only the original wait budget. Results identify unsatisfied waits as timed out.
Every blocking forge operation inside a wait (repository discovery, pull
request metadata, each check page and the checks read) shares one request
context: the wait's absolute deadline, capped per read by
`domains.wait.pr_read_timeout_seconds`, and the waiter's cancellation. Each
operation derives its bound from the time that context has left, and a wait
that settles cancels the forge transport of its other targets.
Wait settings resolve in one place for every waiter (`wait`, owner-side run
waits, relay child waits and CI cadence). A configuration file that cannot be
read leaves the compiled defaults in force with a warning, and every reported
source names the reason; a file that is present but malformed is a typed
error. Reports name each setting with its source.
- Why: a wait must not outlive its budget in a transport it can no longer
  interrupt, and one setting must not fail three different ways.

Wait budgets clamp to `domains.wait.max_timeout_seconds`, warning once with
the effective limit and source when exceeded. A larger request remains a
blocking wait. Parsing retains the request so configured limits, including
maxima above the default, take effect in one place.
- Why: an oversized budget must not turn a blocking call into a retry loop.

With `--json`, wait emits JSONL events followed by a terminal command-result
envelope, including without `--follow`.
- Why: a reader must neither terminate healthy work nor wake on stale evidence.

### Status selection

Status includes all active graph work; omitted settled work is counted. Human
and structured output share the selection. `status --mine` reads active custody
across projects, or one explicit `--project`, including attempts nested under
the current chair and unattended items available for adoption. It is short by
default, expands with `--full`, and exposes typed records and native presence
with `--json`. Pagination uses `--limit` and `--cursor`; `--session` selects the
exact chair session for the session-start digest. Settled custody leaves the
active view. The digest reads this view instead of extracting PRs from prose.
Graph status rows share the same keys across graph, node and attempt kinds.
Node warnings are present on node rows and null on graph and attempt rows.
The graph-listing AGE column measures elapsed time since graph creation, even
when the graph's state changed more recently.
For graph nodes with a typed review result, the digest shows execution state
separately from verdict, findings count and findings availability.

For a durable `running` attempt, host status reports native writer presence as
`live`, `unattended`, `settling` or `unknown`. `running` is shown only while the
recorded guardian or provider identity is present. A changed boot proves the
old process tree is gone. Same-boot absence remains `unknown`, even when the
last process checkpoint and current bounded census are complete: a child may
have forked into another session or group after the checkpoint and then been
reparented. A sealed process exit reports `settling`. A PID or its reuse alone,
a host that cannot observe the writer, missing checkpoints, permission errors,
incomplete evidence and unreachable peers remain `unknown`; status does not
mutate or schedule recovery.
Unattended and settling rows name the scoped `provenant reconcile` action,
which rechecks custody before settlement.

`adopt KIND:ID` takes custody of an external item; `release KIND:ID` records an
explicit release. `fabric claim acquire|renew|verify|release` takes the same
subject spelling; renew records activity without a lease or secret token.
Run launch, worktree creation, PR binding/landing and cloud launch acquire
custody automatically. Attempt/graph settlement releases those subjects; landing
records typed evidence on PR and associated branch/worktree subjects. Custody
failures after a successful external action warn and can be retried by adoption.
Transfer to a named session is deferred.

- Why: a chair acting on a listing must see work still running.

### Output references and bounded tails

For a graph with one non-composite top-level node, `output GRAPH` reads that
node's newest settled attempt. For a composite root it reads the declared bounded result and
returns its typed exports, without inventing an attempt identity. A bare graph
with multiple top-level nodes remains ambiguous and lists only those nodes;
`output GRAPH/NODE` selects a composite explicitly. Prompt, stderr, transcript,
events, receipt and artifact-path reads require an attempt and are unavailable
for a composite. If a composite has typed exports but no result text, output
returns those exports with an empty byte chunk. Offsets and limits count bytes;
a page that splits a UTF-8 character uses lossless base64 bytes and advances by
exactly the returned byte count. A bounded composite excerpt reports
`complete: false` when the checkpoint says its source result was truncated,
with an unknown source length.

For attempt targets, output and steering resolve the same numbered attempt.
Output reads are bounded and a clamped tail reports its effective bound.
By default, `output` shows result text whole up to 12 KiB, then marked head and tail; `--full` shows complete text.

- Why: a reader must be able to inspect the intended attempt safely.

### Settled attempt provenance receipts

A receipt identifies a settled local attempt and reports only supported route
and reviewer attribution. Ambiguous, running or peer-owned targets are refused;
missing evidence stays unknown.

- Why: provenance must not mistake a writer for a reviewer or infer a route.

## Guards

### Merge guard

Landing verifies and merges the same head, and refuses changed heads or
incomplete required check evidence.

- Why: a merge must land the head that CI verified.
- A refused merge keeps the unified `LAND_REFUSED` code, plus a typed `cause`
  (`HEAD_MOVED`, `MERGE_NOT_ALLOWED`, `PR_DRAFT`) and a non-empty `repair` for the next
  step, so callers branch on the cause rather than on message text.

### Landing adds no merge authority

Approval must cover the exact head and come from an authorised independent
source or an explicit owner override. Graph landing uses the same policy.
Approval evidence is the union of configured sources. A new passing independent
review on the pinned head cannot invalidate an existing forge approval; loss of
all current approvals still excludes the member. The publication lock fences
landing identity, generation and snapshot revision before dispatch: a superseded
generation cannot publish, including during recovery.

`land --strategy` choices come from the records strategy declaration; `--target`
selects the root branch, defaulting to the forge repository's default branch.
Repository `.provenant/config.toml` declarations, host settings, project overrides,
target rules, risk tier and invocation resolve into one saved plan with sources.
`--explain --json` reports repository, target, effective plan, capability decisions
and unavailable alternatives. An unavailable execution returns its landing record
ID and typed alternatives without silently selecting another strategy.

Landing CI, candidate gate, and post-merge waits use the forge observer's adaptive
cadence when `poll_seconds` comes from the bundled default. Supplying `poll_seconds`
at any supported configuration layer or using `land --poll` selects that fixed interval for
all three waits. Source provenance decides this, so an explicit value is fixed even when it
matches the bundled default.

While a candidate gate waits, `land` prints the candidate ref, the checks URL and the state
to standard error, then repeats the elapsed time and state every `progress_seconds`
(default 60). When a candidate's tree equals the tree of a member head whose required
checks already passed, the gate is skipped and the receipt's gate evidence records the head
as `reused_from_commit`; `reuse_tested_tree = false` forces a gate run. Each land also
removes pending records and `land/*` candidate refs whose pull requests are merged or
closed.

The effective `PortTable.land` declaration alone selects the publication backend.
`none` returns typed unavailability before publication effects; `command` invokes
its declared argv in the repository checkout. Candidate and ordering strategies
remain independent of that backend. The command receives
the full pending intent as JSON on stdin. Its required declaration includes
`unknown_exit_code` (1..255) and `timeout_seconds` (>0). Exit 0 attests that the
supplied candidate landed; other exits refuse, except the declared unknown code.
Timeouts and signals are also unknown. The same pending markers, gate, approvals,
custody and settlement evidence apply. Unknown commands are observed through
candidate ancestry and exact-head forge evidence, never replayed or replaced by
a builtin publisher while their outcome remains uncertain.

Cleanup after merge preserves moved branches, dirty worktrees and work that is
not proven merged; incomplete cleanup is reported.
Approval sources default to Provenant review then forge and are overridable by project and tier; the first usable proof still covers the exact head.

- Why: routine landing must not widen approval or delete unmerged work.

### Pull request bodies come from receipts

Pull request creation derives its claims from the settled attempt and delivery
receipt, and states absent evidence rather than inventing it.

- Why: a hand-built body can claim a head or route the receipts do not support.

### Shell commands use confinement

The hook validates its event envelope but does not decide authority from shell
syntax. Writer isolation and authority are enforced at the boundaries described
in [supervision](supervisor.md#permission-policy).

- Why: shell syntax rules cannot enforce those boundaries.

### Pre-push refuses data loss

The pre-push guard protects shared default-branch history and reports advisory
conflict markers.

- Why: shared history must not be deleted or rewritten accidentally.

### Worktree hooks respect ownership

Worktree hooks use the worktree owner and cannot remove a worktree they did not
create or override the owner's refusal.

- Why: a hook must not widen removal authority.

### Checks report, never repair

Checks report findings and unavailable evidence without repairing the subject.
Release checks inspect the artifact to be distributed as well as Git evidence.

- Why: an inspection must not change its subject or certify an uninspected artifact.

### Release symlinks

A release symlink is portable only when its relative target stays inside the
repository and names a tracked regular file in that same tree. Absolute,
escaping, malformed, missing or untracked targets and links to another
symlink remain findings.
The check resolves Git targets from each tree's entries and reads current
worktree links without following them through the host filesystem.

- Why: every published link must have one inspectable target in the tree being
  checked, independent of host files or link-chain behaviour.
- Owner: `crates/cli/src/check/release.rs`

## CI observations

`pr status [NUMBER ...]` reports each pull request's head, check rollup and failed check names in one batched GraphQL read when available; omitting numbers selects this account's open pull requests. The REST observer shares ETag-backed check pages by repository and head SHA. Batched results have a short per-pull-request freshness window and fall back to REST when GraphQL cannot answer; a REST rate limit falls back to GraphQL when available.

`ci failures pr NUMBER` and `ci failures run ID` report failing test names, panic locations and messages, and error lines from failed job logs. The parser handles nextest and legacy `cargo test` output. Human output is compact by default; `--json` returns structured evidence. Failed logs share the CI observation cache for a short freshness window. Unreadable logs and bounded output remain visible in the result.

- Owner: `src/pr/status.rs`, `src/pr/status/batch.rs`, `src/ci.rs`
- Why: a chair needs actionable CI evidence without mistaking missing observations for success.
