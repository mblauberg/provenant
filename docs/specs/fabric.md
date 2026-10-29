# Fabric

Owner: `crates/fabric-store` (SQLite), `crates/fabric-runtime` (identity,
coordination, peers, graphs, Git transport), `crates/fabric-mcp` (stdio MCP)
Anchor: `crates/fabric-store/src/tests.rs`, `crates/fabric-runtime/src/graph.rs`,
`crates/fabric-mcp/src/stdio_tests.rs`, `tests/fabric.rs`, `tests/fabric_transport.rs`

Fabric is the daemonless coordination layer for agents on one user's hosts:
mail, tasks, an event log, advisory claims, fenced landing, bounded graphs,
peer hosts over SSH and the stdio MCP server. It grants no authority to
install, land or run a live provider, and keeps no second run status
([run](run.md)). ADRs [0020](../adr/0020-retire-the-daemon-fabric.md),
[0022](../adr/0022-thin-fabric-mcp-execution-facade.md) and
[0026](../adr/0026-federate-fabric-across-own-hosts-over-ssh.md) record the
decisions; setup is in the [peer host runbook](../runbooks/fabric-hosts.md)
and the [install runbook](../runbooks/install.md#fabric-mcp-registration).
Paths are relative to `tools/front-door/`.

## Identity and store

### Projects never merge by guess

`ProjectId` is host-local, derived from the `HostId` and canonical Git common
directory, so linked worktrees share it. The cross-host `ProjectKey` is
relative to the user's home directory and verified at each peer; outside it,
remote placement is refused. No scan or URL heuristic joins projects. Labels address inboxes and
never prove a model family.
- Why: two checkouts must never share coordination state by accident.
- Test: `tests/fabric.rs::linked_worktrees_share_projects_and_ignore_git_redirects`;
  `crates/fabric-runtime/src/identity.rs::keys_are_home_relative_and_local_ids_are_separate`
- Owner: `crates/fabric-runtime/src/identity.rs`

### Lane store isolation

Lane observations open `Store::open_read_only`, with exact read grants for
`fabric/fabric.sqlite3` and its `-wal`, `-shm` and `-journal` companions.
No lane profile grants writes to these files, Fabric locks or the Fabric
directory. Long-lived lane readers release the initial snapshot so subsequent
observations see fresh WAL commits, without creating or updating companions.

The guardian starts an attempt-bound host MCP relay before provider spawn and
closes it on provider exit, failed spawn or guardian teardown. Each lane may
connect only to its own exact Unix socket under a private, short-lived host
directory. The host captures the project and `lane-<attempt>` actor from the
custody context; request fields and lane environment labels cannot select the
sender. Mail, tasks, event notes and advisory claims use the existing atomic
operation receipts. Observations of a configured remote home also use this
relay, preserving the sole owner rather than reading a local mirror. Landing
verification also uses the host because it requires an OS lock.

Relay frames obey configured input byte/depth limits, absolute I/O deadlines
and cancellation. After admission the caller confirms its remaining budget
before the host executes; an abandoned queued request cannot mutate later.
The relay excludes landing mutations, inbox writes, chair writes, graphs and
execution. CLI coordination mutations, including `fabric ... send`, still refuse inside lanes. Peer ingress
and `peer-agent` remain unavailable to lanes. The socket is removed on normal
teardown; on restart, a stale socket directory is removed only if it is a real directory owned by the current user with mode 0700.
- Why: a lane must not forge records that the host or another agent trusts.
- Test: `crates/providers/src/seatbelt_snapshots.rs::fabric_profiles_only_read_database_and_allow_only_the_attempt_socket`;
  `crates/fabric-mcp/src/coordination_tests.rs::lane_relay_binds_sender_replays_receipts_and_refreshes_read_only_wal`;
  `tests/fabric_lane.rs`.
- Owner: `crates/cli/src/run/provider.rs`, `crates/fabric-mcp/src/lane.rs`,
  `crates/fabric-store/src/db.rs`

### Private, versioned database

The store is one SQLite database (WAL, foreign keys, `synchronous=FULL`)
under a root with mode 0700 and files with mode 0600. Fabric refuses symlinked
components, and the database and its companion files must be private, single-link and owned by the
user. A future or unversioned schema is refused before any mutation. Limits
are ceilings that config may lower, never raise.
- Why: a shared or swapped database would let another user forge fences.
- Test: `crates/fabric-store/src/tests.rs::state_symlinks_and_non_private_components_are_refused`;
  `tests/fabric.rs::domain_config_accepts_typed_peers_and_only_lower_limits`
- Owner: `crates/fabric-store/src/db.rs`

### Atomic, replay-safe mutations

A mutation and its event commit in one transaction, and no transaction stays
open while waiting, spawning, probing Git or calling SSH. Retryable
mutations carry an operation ID: the same ID with a different digest is a
conflict, and a lost reply is answered from the stored receipt, never by
repeating the mutation.
- Why: agents retry after timeouts; a duplicate send or claim is a bug.
- Test: `crates/fabric-store/src/tests.rs::event_failure_rolls_back_task_and_mail_mutations`;
  `crates/fabric-store/src/tests/operations.rs::concurrent_duplicate_operations_apply_once_and_share_the_committed_receipt`
- Owner: `crates/fabric-store/src/`

### Fences and cursors

Generation counters survive release and expiry, and claims use the home
host's clock, never a caller's. Task `finish` needs the live fence. Path
claims compare canonical prefixes and are advisory and never authorise writing
to a worktree. Mail cursors advance monotonically and only for the caller. Run
events are pulled lazily and deduplicated in the same transaction that
advances the cursor; an unreachable peer is `Unknown` and partial results never
claim complete coverage.
- Why: a stale holder must never act, and a reader must never skip events.
- Test: `crates/fabric-store/src/tests.rs::task_fences_survive_expiry_and_release_and_cannot_be_bypassed`;
  `crates/fabric-store/src/federation.rs::run_event_rows_and_host_cursor_rollback_together_then_deduplicate`
- Owner: `crates/fabric-store/src/`

## Human requests

Viewer request presentation state is separate from resolution and blocking.
Viewer names are caller-supplied labels, like agent labels: they are not an
authentication boundary. A local caller that knows a label can change that
viewer's state; the direct SQLite access residual above also applies.
A lane may dismiss or snooze a request in any viewer, including `console`.
Snoozing never grants authority.
A lane may change only the bounded request presentation fields `seen_revision`,
`snoozed_until` and `dismissed`; it never changes resolution, answers or the
blocking state, and summary baselines remain outside a lane.
Quotas are per project and viewer, with a combined per-project cap. Writes
reclaim closed or expired request rows. A new summary baseline is refused at
capacity rather than evicting an existing one.
- Why: viewing and snoozing do not authorise an answer or unblock work.
- Test: `crates/fabric-store/src/tests/human_inbox.rs::viewer_isolation`;
  `tests/inbox.rs::viewer_cli_isolation_and_render_race`
- Owner: `crates/fabric-store/src/viewer.rs`, `src/chair.rs`

Human requests, immutable answers, revision history and the delivery outbox
belong to Fabric. CLI and MCP must use the same service. The service binds
project, host and actor from Fabric identity; request JSON cannot supply them.
The chair declares its exact session explicitly. This declaration is not
session discovery or evidence of a live provider connection.
- Why: a shared agent label cannot identify a particular chair session.
- Test: `tests/inbox.rs::inbox_cli_refuses_forged_ownership_and_closes_without_approval`;
  `crates/fabric-runtime/src/inbox/tests.rs::inbox_acknowledgement_requires_the_exact_host_project_session_and_actor`
- Owner: `crates/fabric-runtime/src/inbox.rs`, `crates/records/src/inbox.rs`

`request.work` is caller-authored claimed work, never a verified binding by itself.
Its optional repository names the canonical Git common directory; head names
launch HEAD. `work_coverage` is a host-only preparation snapshot. Consumers MUST
use both together: only `verified` permits navigation or task-label joins to work,
repository or head. `HumanRequest::verified_work()` enforces that rule. Other
coverage, missing coverage and legacy claims may be displayed only as unverified
claims, never promoted by a UI, summary or receipt replay. Even verified work
confers no execution, repository or approval authority.

Both edges resolve through Run at the configured host root and current project.
They reject `mismatch` with `work_mismatch`, including unqualified selectors;
partial, unavailable, unindexed and capped results remain explicit claims. Replay
validates the exact actor and input before lookup. New lookups finish before Fabric opens its write
transaction, so index locks, creation and backoff cannot stall other coordination.
Inside the transaction the exact actor/input receipt is checked again to handle
concurrent preparations. Membership is an observation, not a cross-store atomic
snapshot: a later host capture does not retroactively change saved coverage.
Ask receipts report the saved coverage. Missing resolvers fail closed; lookup
failures disclose their cause as `work_lookup_failed`, preserving busy/cancelled.
Repository limits count at most 4096 UTF-8 bytes of `relative_to_root.path`,
including hexadecimal expansion for `native_bytes`, excluding the root slash.

Unlinked requests omit absent correlation fields and retain the version-1 wire
shape. Requests using work, deadlines or replacement links declare version 2 and
require a version-2 reader. An older reader that rejects a record fails the entire
list page, so an inbox list or console overview can be unavailable even when other
records are readable.
Current readers accept both record versions, including retained history and the
bounded overview projection; the database schema is unchanged. Existing version-1
records and immutable receipts remain readable and retain their original shape.
- Why: unknown work must not appear verified; index contention must not hold
  Fabric's write lock; optional features must not rewrite unrelated version-1 data.
- Test: `crates/fabric-runtime/src/inbox/tests.rs::request_correlation_lookup_never_holds_fabric_write_lock`;
  `crates/fabric-runtime/src/inbox/tests.rs::request_correlation_legacy_records_keep_v1_wire_shape_and_answer`;
  `tests/inbox.rs::request_correlation_edges_verify_host_seals_and_refuse_mismatches`
- Owner: `crates/records/src/inbox.rs`, `crates/fabric-runtime/src/inbox.rs`,
  `crates/fabric-store/src/operations.rs`, `crates/cli/src/inbox.rs`

One operation must commit the revision comparison, answer, request state,
history, delivery intent, local Fabric response and replay receipt together.
Replays return the original receipt before checking the current revision.
Changed content or actors under the same operation ID conflict. A competing
answer cannot replace the winner. Closing an item preserves any earlier answer.
- Why: a timeout or competing UI must not produce a second authoritative answer.
- Test: `crates/fabric-store/src/tests/human_inbox.rs::human_answer_outbox_mail_and_history_roll_back_when_receipt_fails`;
  `crates/fabric-runtime/src/inbox/tests.rs::inbox_concurrent_answers_have_one_winner_and_one_mail`;
  `crates/fabric-runtime/src/inbox/tests.rs::inbox_request_and_answer_replay_survive_restart`
- Owner: `crates/fabric-store/src/human_inbox.rs`

Fabric mail is queued delivery, not live injection or evidence of consumption.
Only explicit acknowledgement from the exact target host/project/session/actor
marks an answer consumed. Mail cursors remain independent. Answers never start,
resume or cancel runs, approve reviews or broaden authority. Only decisions
may be marked blocking; assumptions still require existing authority.
- Why: transport success and human choice cannot substitute for workflow gates.
- Test: `crates/fabric-runtime/src/inbox/tests.rs::inbox_fabric_mail_is_correlated_and_read_cursor_does_not_acknowledge`;
  `tests/inbox.rs::inbox_cli_posts_lists_and_resolves_all_kinds_without_apps`
- Owner: `crates/fabric-runtime/src/inbox.rs`

Delivery offers must resolve the registered chair, validate the local owner and
lane before opening the store, and match host, project, session and actor.
Bounded offers repeat until explicit acknowledgement; oversized answers are
refused at submission and smaller batches skip entries that do not fit.
- Why: a failed delivery must not transfer an answer or stall other answers.
- Test: `crates/fabric-runtime/src/inbox/tests.rs::offer_until_ack_repeats_after_restart_stops_after_ack_and_preserves_overflow`;
  `crates/fabric-runtime/src/inbox/tests.rs::oversized_answer_is_refused_and_small_budget_skips_large_answers`;
  `crates/fabric-runtime/src/inbox/tests.rs::concurrent_offer_and_ack_never_resurrects_delivery`;
  `crates/fabric-runtime/src/inbox/tests.rs::offer_until_ack_rejects_unregistered_and_wrong_targets_without_mutation`
- Owner: `crates/fabric-runtime/src/inbox.rs`, `crates/fabric-store/src/human_inbox.rs`

Inbox ownership is host-local, independent of the configured project home.
CLI list, show and answer may address a configured peer by host label or ID;
a request ID may be qualified as `host:request`. The owner executes answers
with the normal revision and operation-ID replay rules. It records the source
host and peer actor with the answer; the answering host keeps no authoritative
copy. MCP `fabric_inbox` accepts a host selector for the same owner routing.
Close and acknowledgement are owner-local chair operations, retaining
exact chair and delivery target checks. Remote attempts return
`owner_local_only` before a write and direct the caller to the owner. An
unavailable owner is reported rather than replaced with a local mirror.
Paging binds project, host, session, state, page size and request snapshot;
changed filters or snapshots require an explicit restart. New observations
invoke no model, provider probe, polling loop or background process.
- Why: unknown peer/session coverage must remain unknown.
- Test: `tests/fabric_transport.rs::remote_inbox_owner`;
  `crates/fabric-runtime/src/inbox/tests.rs::inbox_paging_binds_filters_snapshot_and_limits`
- Owner: `crates/fabric-runtime/src/inbox.rs`, `crates/cli/src/inbox.rs`

## CLI and MCP surfaces

The tool catalogue is generated reference in
[`docs/reference/mcp-tools.md`](../reference/mcp-tools.md); CLI leaves are in
the [CLI reference](../reference/cli.md).

The default MCP catalogue exposes run and output operations, messaging and
identity, plus `fabric_discover`. Discovery returns the schemas for graph,
lease, task, event, inbox, console and chair operations, which remain callable
by their tool names.
Run submission can wait for a result in the same bounded call; a timeout
returns a receipt and continuation instructions. Run listings are bounded and
newest first, and compact status, wait and cancel responses can request detail.
Waiting is optional: every settlement passes one post-settlement seam that
runs its reactions in order, each isolated from the others and idempotent by
attempt ID. Graphs advance, then the dispatching chair's inbox receives one
note of at most 300 bytes (attempt, outcome, `(partial)`, model, staged
recovery, head, pull request), then the chair's session log gains a
`lane_settled` entry. Notices report outcomes and never instruct.

The run store owns the obligation. The terminal commit inserts a `reactions`
row (schema v6) in the same transaction, freezing the notice, the message
envelope, the pull request and issue IDs, and that a graph advance is due. The
row never carries host authority: it lives in lane-writable state, so graph
launch inputs come from the live guardian, or, in recovery, are derived afresh
from the trusted process and the host-owned graph journal. If they cannot be
derived the graph reaction stays pending. The first drain resolves the chair
and its session and persists them before any effect uses them, then saves
progress at every step boundary with a fresh call budget. Reactions are
at-least-once and exactly-once in effect: the normal path and crash recovery
drain pending rows under the run's `reactions.lock`, a busy lock means another
drain covers the run, and a failed reaction, or a busy host store, leaves only
that reaction pending without stopping the others. `run reconcile` and recovery
scheduling both see a closed run that still has pending rows. Attempts that
settled before v6 have no row, so nothing is backfilled. Finished rows are
pruned with `[retention] logs_days`.

The chair log goes to the session in `PROVENANT_SESSION_ID`, else the session
the chair saved, which must still be current and name the chair in its own
canonical record. A lane dispatched by a lane has no chair and stays quiet, and
read-only lanes notify only when `[domains.fabric.lanes.settlement]
notice_read_only` is set; `notice` and `chair_log` switch the other two
reactions.
Run operation IDs are generated when omitted and returned for retries;
explicit IDs preserve planned retry identity.
Chair log appends generate an operation ID when omitted and return it in the
receipt; a generated ID also encodes the original issuance time. A generated ID
protects only retries made after that receipt arrives. To survive a lost
response, choose the ID before the first call and send the same ID, content and
`issued_at` on every retry. Summary updates use revision compare-and-set and
have no operation ID yet (#2149).

### Strict tool input

Each tool takes a tagged `op` enum that rejects unknown fields, with no
coercion or synonym repair. Compact `tools/list` schemas accept every valid
strict probe, and runtime validation still rejects empty, NUL and oversized
text with the field path, before side effects. MCP submission rejects
`new_worktree`, and `resume` cannot change route, account or authority.
- Why: a lenient parser turns a model's typo into a different request.
- Test: `crates/fabric-mcp/src/schema_tests.rs::generated_schema_values_match_strict_serde_for_every_operation_and_field`;
  `::peer_worktree_requests_are_not_mcp_submit_inputs`
- Owner: `crates/fabric-mcp/src/`

### Lossless, bounded output

Output is UTF-8 when valid, otherwise padded base64; neither is lossy, and
a page never splits a character. Output reads default to the result part at
offset zero with a bounded size. Paged observations fit the byte budget before
issuing a continuation cursor; committed mutation receipts keep the transport
ceiling. `fetch` copies only run-exported regular files, refuses symlinks and
path traversal, and verifies a manifest digest before atomic publication.
- Why: agents must read every byte, never lose a receipt and never import a
  peer's link.
- Test: `crates/fabric-mcp/src/tools_tests.rs::result_limit_preserves_mutation_receipts_and_rejects_oversized_observations`;
  `::run_page_shrinks_before_cursor_is_issued`;
  `crates/fabric-runtime/src/fetch.rs::refuses_traversal_digest_mismatch_and_non_contiguous_chunks_before_publish`
- Owner: `crates/fabric-mcp/src/`, `crates/fabric-runtime/src/fetch.rs`

### MCP transport

The server is a bounded, tools-only stdio JSON-RPC transport with
protocol-only stdout, serving the 2025-11-25 and 2026-07-28 eras without a
switch mid-connection. Four workers share a queue of 32, and a full queue returns a
typed busy error. Cancellation suppresses the late reply and never cancels a
launched run; on EOF accepted work drains for at most 55 s.
- Why: a disconnected or cancelling client must never kill lanes.
- Test: `crates/fabric-mcp/src/stdio_tests.rs::cancellation_suppresses_late_observations_without_cancelling_runs`;
  `::four_workers_and_queue_capacity_return_busy_after_thirty_six_calls`
- Owner: `crates/fabric-mcp/src/stdio.rs`

### One error meaning at both edges

The CLI and MCP render one typed category per run error, and peer replies
keep their error code over the wire. MCP errors include a message and recovery
hint, and validation identifies the expected value. Uncertain mutations carry
their `operation_id`.
- Why: people and agents act on the code.
- Test: `crates/cli/src/fabric.rs::upgrade_and_peer_version_refusals_match_on_both_edges`;
  `src/fabric.rs::peer_run_errors_stay_typed_through_cli_rendering`
- Owner: `crates/fabric-runtime/src/coordination/facade.rs`

## Bounded graphs and route fallback

### Pipeline presets

`provenant run --preset NAME --prompt-file FILE` expands a preset into an
ordinary graph and submits it through the graph owner. `--wait DURATION`
waits for graph progress, `--concurrency N` bounds admission, `--worktree PATH`
supplies a writer binding, and repeatable `--read-root PATH` grants additional
read access. Writer worktrees must already exist and be registered; presets
do not provision branches or checkouts. The [preset runbook](../runbooks/presets.md)
contains starting points and batch preparation guidance.

`--explain` prints a compact tree of roles, tiers, scopes, worktrees, inputs,
concurrency, attempt bounds, acceptance and effective limits without writing a
graph or opening graph state. JSON output includes the exact submitted graph
and effective value origins, bounded by the 8 MiB graph document limit rather
than the ordinary 1 MiB result limit. Identical expanded graphs use the same
graph ID and a private, content-addressed document in project scratch, so
retrying the same submission observes the existing graph. Change the prompt or
parameters for a separate graph.

Every preset is a list of `stages` lowered by one path. `single`, `chain`,
`fan-out` and `fan-out-synthesise` are bundled stage presets with read-only
tasks, a `parameters.route` pool (default `senior`) and the supplied prompt;
the fan-out presets also accept `parameters.width` (1 to 32, default 2). A
chain passes the first result to the second; synthesis receives each branch
result. A stage can run any preset.

A preset declares ordered `stages`. A stage selects a role, another
preset or heterogeneous `parallel` entries. `count` repeats an entry, from 1
to 32. Role profiles supply node defaults, and an entry may override them:

```toml
stages = [
  { id = "scouts", parallel = [{ role = "scout", count = 3 }] },
  { id = "summary", role = "synthesiser" },
]
exports = [{ node = "summary", outputs = ["result", "findings"] }]

[roles.scout]
tier = "junior"
title = "Scout ${index}"
prompt = "Map assignment ${index}; cite call sites and uncertainty.\n${prompt}"

[roles.synthesiser]
tier = "senior"
kind = "synthesise"
prompt = "Reconcile evidence and identify the next slice.\n${prompt}"
```

Roles can configure `prompt`, `title`, `tier` or `route`, `effort`, `access`,
`worktree`, `write_scope`, `read_roots`, `acceptance_checks`, selected `inputs`,
`required_verdict`, `source` and existing node-kind configuration. Access
remains read-only unless explicitly changed. Tier names resolve through named
routing pools; profiles do not hard-code a model. A curated `high-junior`
pool is a chair-supplied configuration, not a bundled alias for high effort.
`tier` and `route` are alternatives; `route` accepts a pool name or graph route
table, and `effort` configures the chosen route.

`${prompt}` inserts the run brief and `${index}` is the one-based repeated
entry number. `[parameters]` defines editable values: an exact `${name}`
placeholder retains the parameter's TOML type, including arrays; placeholders
inside text insert scalar values. Unknown parameters and unresolved bindings
are rejected. Stage IDs form local dependency and export names; expansion
namespaces nested children. Child presets have their own parameters and roles;
an entry's `parameters` and `roles` tables override that child's defaults.

Composite nodes retain their children, expose declared outputs and report
one grouped status. Their internal attempts and evidence remain available in
the graph checkpoint. `exports` uses ordinary graph input selectors; by
default a preset exports result and findings from its final stage. Parallel
groups combine labelled, bounded results and findings. Scalar outputs such as
a commit or verdict require an unambiguous named export. Stage inputs select
the preceding result by default; an entry's `outputs` chooses other preceding
output kinds. Composites use no provider attempt of their own. An entry can
declare `loop = { stages = [...], until = "accept", max_rounds = 2 }` to reuse
roles and presets inside bounded repair. Existing `loop.body` configuration
also remains available. Planner fragments do not resolve named presets.

Bundled `scout-swarm`, `generator-evaluator`, `review-council`, `big-batch`,
`scope-then-run` and `deliver` express scouting, implementation with
evaluating lenses, combined review, a full batch, scouting and scoping before
a run, and delivery through CI to land respectively. Scouts use junior tiers and remain read-only;
implementation uses senior tiers; the end-of-batch refactor uses principal.
The implementation lens challenges redundancy, test quality, cleanup and
rigid over-engineering. Junior reviews on senior or principal work are
advisory. Counted acceptance reviews must meet the author's tier under
`HARNESS.md`; add principal coverage for high-risk areas and principal-authored
refactors. Synthesis reconciles evidence rather than taking a majority vote.

`deliver` runs a `write.implement` writer with `delivery = "pr"`, then a
composite `ci` holding a CI gate bound to the writer's pushed head and a
repair loop the gate routes to on failure. Each loop round runs a fresh
`repairer` that `continues` the writer's branch, whose inputs are the failed
gate's findings (check names with log excerpts), the writer's `result` and
`remaining` handoff items in every round and the previous repair's, never a
transcript,
followed by the CI gate again. The composite
succeeds on either path. A `review-council` then reviews a `head` source
binding to the writer, so it judges the newest repaired head; a verdict gate
and a `land` node merge that head under the host's tier policy. No model runs
while a gate is pending. `max_iterations` bounds the rounds; exhaustion raises
the loop ask when `decision_session` is set and otherwise pauses the graph.
The repairer's tier selector may supply ordered routes with a named
`host-native` entry. Each such route requires a later non-native fallback,
checked before graph execution.
Fallback retains the existing rule that a launched writer never switches routes.
A host-native repair's delivered commit passes through the same CI gate before
review or land; a host completion status cannot replace that observation.
See [host-native repairs](../runbooks/presets.md#host-native-repairs).
The same session is bound to the land node's `session`, so a nested `deliver`
still asks it, and the chair lands by hand; with none, a tier that asks fails
the land node with guidance to land by hand. A repository preset layer never
lowers a land tier: when repository layers take part, the expansion is
repeated without them, and each land node takes the stricter tier of the two.
A land node with no counterpart there, or any land node when the preset has
no non-repository layers, takes `terminal`. Command-line values take part in
both expansions, so `--set` and `--params-file` still set the tier. Preset leaves may set `continues`, `land` and
`kind = "land"`. A preset's top-level `decision_session` sets the graph's
decision session; an empty value, here, on a loop or as a land session, leaves
it unset.

Preset TOML files are named `NAME.toml` under
`$XDG_CONFIG_HOME/provenant/presets/` (or `~/.config/provenant/presets/`)
and `.provenant/presets/` in the selected project. Values merge in bundled,
user, repository and `--set PATH=TOML_VALUE` order. `preset list` lists names
with each pattern's level, summary and cost; `preset show NAME` displays merged
values and their origins; `preset explain NAME` renders the pattern, typed
parameters, stages and roles without creating state. `extends` selects
another preset's defaults. A same-name override omits `extends` and supplies
only its changes. Stage roles come from a role library (bundled `roles.toml`,
then user and repository `roles.toml`, then a preset's own `[roles.*]`), listed
by `preset show --roles`. A role may declare `card`, `family`, `extends` and
`skill:` `references`; the compiler records each reference's content digest on
the node, so a replayed graph names the exact guidance it ran with. See the
presets runbook. For example:

```toml
extends = "chain"
stages = [
  { id = "first", preset = "fan-out", parameters = { width = 3 } },
  { id = "second" },
]
```

A parameter is either a bare value or a typed table with `type` (`string`,
`integer`, `number`, `boolean`, `list` or `table`), `default`, `min`, `max`
(numeric value, or list or text length), `enum` and `doc`. A bare value that
overrides a typed parameter replaces its default and keeps the schema. A
refused value names the parameter and the layer it came from. A same-name
override of a bundled preset may add parameters of its own. An optional `[pattern]` table holds `level` (`default`,
`recommended` or `experimental`), `summary`, `citations`, `cost`,
`select_when` and `tags`. Parents pass `risk_tier` and `subject` to a child
that declares them unless the entry sets them; `prompt` is always available.

`--set PATH+=VALUE` appends to a list parameter and is refused on a scalar.
Composite `inputs` and `exports` are typed by output kind: lowering refuses a
selection its source node cannot supply (`commit` needs write access or a
pinned source, inherited from an enclosing composite; `branch` needs write
access; loops supply neither), with an error naming both nodes. Selections
resolve against the enclosing scopes, and `previous.X` against the loop body.
`--set` addresses a top-level stage by id (`sN` by position when it has none):
`--set first.width=3` sets a parameter of the preset that stage runs, and on a
role stage `--set task.tier=junior` sets that entry field. Stage `id` and
`preset` cannot be overridden.

A preset run announces every repository preset file used in resolution order,
including inherited files and files whose values are overridden. Each file is
listed once; bundled and user layers are excluded.

Named child presets use their own route defaults. Expansion is static,
limited to eight levels and 512 nodes including children, and refuses cycles
and invalid fields. Parameter values passed on the command line use TOML
syntax, for example `--set 'parameters.route="utility"'`; unquoted bare words
are rejected with a quoting hint. Invalid files appear in `preset list` and do
not prevent use of unrelated presets. Runtime limits and host ceilings still
apply.

- Why: editable roles and nested compositions reuse graph admission,
  settlement, writer exclusion and delivery.
- Test: `src/preset_tests.rs::bundled_expansions_match_golden_graphs`,
  `::layering_composition_and_refusals`, `::named_presets_inherit_and_cycles_are_refused`,
  `src/preset_deliver_tests.rs::a_red_gate_repairs_from_the_failure_excerpt_and_the_repaired_head_is_reviewed_and_landed`,
  `::an_exhausted_repair_loop_asks_the_decision_session_or_pauses`,
  `::pending_ci_runs_no_model_and_status_shows_the_waiting_gate`,
  `::remaining_work_at_the_end_of_a_long_report_reaches_every_repair_round`,
  `::reviewers_route_away_from_the_writer_and_every_repair_author`,
  `::a_council_pinned_to_the_writer_commit_never_lands_the_repaired_head`,
  `::deliver_nests_inside_another_preset`,
  `crates/cli/src/lib.rs::tests::preset_runs_accept_graph_controls_without_dispatch_writer_mode`.
- Owner: `src/preset.rs`, `src/fabric.rs`

A graph must obey the configured structural limits and budgets. Each attempt
is also bounded by `limits.graph_timeout_seconds`. The [host runbook](../runbooks/fabric-hosts.md#configure-hosts)
describes defaults, ceilings, budget telemetry and resuming a paused graph.
The ordinary dependency-depth limit remains four; the default host ceiling is
64 so a preset can request an explicit compatible depth. `big-batch` requests
64. A lower configured ceiling still caps that request and appears in explain.

Each node has an optional task `title`, defaulting to its ID; titles use the
shared task title validation and appear in lanes and graph status. A node may
specify a single `route` or an ordered `routes` list for agent work. A route can name a pool
with just `alias`, resolved as for `dispatch --route`, or supply an explicit
adapter. Routes require an adapter or alias and may mix these forms in a list.

### Sources and read roots

Agent nodes may declare `read_roots`, a list of additional directories passed
through ordinary run confinement. A read-only scout can inspect another
worktree's uncommitted files without acquiring a writer lease. Read roots grant
no write authority and remain subject to protected-path checks. Presets
canonicalise each root and accept only the project, its linked worktrees and
the host's `[domains.fabric.graph] allowed_read_roots` list. A planner
fragment may not add `read_roots`, change `source` or change `role` relative
to its planner node.

A graph document may set a top-level `network` of `"on"` or `"off"`. Every
node lane the graph launches receives that network reach; without it, lanes
default to open network. `run --network` binds it into a preset graph, with an
explicit `--preset` or under automatic selection. Automatic selection refuses
single-lane flags a graph cannot apply, such as `--account`, `--effort`,
`--write-root` and `--host`, before it selects a preset, and its `next:` names
`--model` or `--route`.

An optional `source` pins a node's subject before its first attempt:

```json
{"type":"commit","commit":"0123456789abcdef0123456789abcdef01234567"}
{"type":"node","node":"implementation"}
{"type":"integration"}
{"type":"head","node":"write"}
```

The explicit commit must be a full object ID. A node binding selects the
successful direct dependency's typed `commit` output. An integration binding
requires the graph's `integration_branch` and resolves its combined head when
the node becomes ready. The checkpoint retains the pinned source for replay
and route fallback. A read-only review receives this exact subject as its
attempt head; its own lane launch head is not a combined integration commit.
Missing subjects fail closed rather than falling back to the current checkout.
The checkpoint also pins the first resolved integration head. A synthesise
node, or a writer whose source is that pinned commit, that has not yet
attempted fails if the integration branch has moved, and a rerun starts a new
graph. A retry writer whose pin is an ancestor of its HEAD continues; a
diverged HEAD is refused.

A head binding names an upstream writer that delivers `pr` or `push` and does
not itself continue another writer. When the node becomes ready it pins the
newest commit delivered by that writer or by any upstream writer that
`continues` it, such as the repair rounds of a CI loop. It is the single-chain
counterpart of an integration binding: a review that follows a variable number
of repairs reviews the head the land node will merge, which `acceptance_error`
checks. Inside a composite or loop body the writer name resolves through the
enclosing scopes. Validation refuses a head binding to any other node.

A node may set `distinct_from = "writer"` or `"dependencies"`. For `writer` the
runtime walks the source chain to the nearest writer node. When that writer is
reached through a head binding, the attempts of every upstream writer that
continues it count too, since each authored the pinned head; for `dependencies`
it takes every transitive dependency's attempt, composite members included. It gives the owner those
attempt refs, and the owner excludes their model families from route
selection. `distinct_strength` is `prefer` (default) or `require`. Under
prefer, when no route outside the families is ready the owner substitutes one;
under require the attempt is refused before launch. An exact `model` route
has no substitute: under require, a model in an excluded family, or of unknown
family, is refused before submission as `adapter_unavailable`, so a later
route in the node's list may still run. After each terminal
attempt the scheduler resolves the applied family and stores a `separation`
record on the node (`strength`, `excluded`, `applied`, `outcome` of
`separated`, `unavailable` or `unconstrained`). With no known upstream family
the constraint is a no-op.

A node may declare `dispositions`, a list of allowed values. At settlement its
findings block must parse and every finding must carry a `disposition` from
the list and a non-empty `reason`, and every finding ID selected from an input's
`findings` must appear exactly once; otherwise the attempt settles as
`task_failed` and `advance_error` names the first offending finding.

A writer source requires an existing registered worktree. Under writer
exclusion, a clean ancestor worktree advances to the pinned commit with a
fast-forward only merge. Ignored output is preserved; ignored files that would
become tracked refuse the advance. Dirty, diverged, reserved and live writer
contexts are refused. The runtime never resets or creates a worktree to
satisfy a binding. The chair prepares writer contexts and integration
explicitly. A review after repair must depend on the repair
node and select its new commit, so it reviews the changed head. In a loop,
`previous.NODE` names the prior round's commit using the ordinary cross-round
namespace rules.

- Why: scouting can share read access while combined review and repair retain
  one immutable Git subject.
- Test: `crates/fabric-runtime/src/graph/land_tests.rs::a_head_source_reviews_the_newest_continuation_and_lands_it`;
  `::a_head_source_inside_a_composite_names_an_outer_writer`;
  `::a_head_source_must_name_an_upstream_writer`.
- Owner: `crates/fabric-runtime/src/graph.rs`, `src/fabric.rs`,
  `crates/confinement/src/worktrees.rs`

### Writer continuation

A writer may set `continues` to an upstream writer whose `delivery` is `pr`
or `push`. The continuing writer is a repair: it reuses that writer's
worktree and delivery branch, so its commits update the same pull request.
It must declare the same `worktree` explicitly, must have the target as an
ancestor, takes no `delivery` of its own and cannot itself be continued.
Writers sharing a worktree already run one at a time. At launch the runtime
reads the branch the target delivered and hands it to the host with `push`
delivery. A target that recorded no delivery branch fails the continuing
node before it launches. The host refuses a worktree whose branch differs
from the bound branch, then pushes the lane head to that branch without
force, so the remote refuses a non-fast-forward update and the delivery
fails at the push stage with the branch unchanged. The lane boundary is
unchanged: lanes never push, and only the host writes branch configuration.

The repair brief is ordinary `inputs`: the upstream gate's `findings` and the
writer's `result`, so failed checks reach the repair as generic findings.
Inside a loop body a continuing writer pushes every round, unlike other body
writers. In a composite, `continues` follows the same local-name rules as
`source`.

- Why: a repair must land on the reviewed PR, not open a second branch, and
  the host must never rewrite a branch someone else moved.
- Test: `crates/fabric-runtime/src/graph/land_tests.rs::repair_reuses_the_writer_worktree_and_pushes_only_to_its_branch`;
  `::a_repair_without_a_delivered_branch_fails_before_launch`;
  `::continuation_and_land_wiring_is_checked_before_launch`;
  `crates/run/src/delivery_tests.rs::host_push_delivery_refuses_a_non_fast_forward_and_leaves_the_branch`.
- Owner: `crates/fabric-runtime/src/graph/land.rs`, `src/fabric.rs`,
  `crates/run/src/delivery.rs`

### Synthesise agents

An agent node may set `kind` to `synthesise` and must select one or more direct
dependencies with `inputs`. Its configured prompt states the synthesis purpose.
The runtime uses the same bounded, untrusted-data handoff and typed settlement
as a task. Omitting `kind`, or setting it to `task`, preserves task behaviour.

- Why: one configurable agent can combine results for different purposes without
  adding a node kind for each purpose.
- Test: `crates/fabric-runtime/src/graph.rs::synthesise_receives_selected_reviewer_findings_and_emits_typed_output`;
  `crates/fabric-runtime/src/graph.rs::synthesise_requires_selected_inputs`;
  `crates/fabric-runtime/src/graph.rs::synthesise_json_preserves_purpose_and_rejects_unknown_configuration`.
- Owner: `crates/fabric-runtime/src/graph.rs`

### Deterministic gates

A `gate` must evaluate its condition without a model or attempt. Verdict gates
must compare selected direct dependencies' verdicts exactly, with missing
verdicts counting as non-matches. Malformed, repeated or non-final verdicts
must fail the gate regardless of its threshold. Majority requires strictly more than half;
count requires at least the configured positive count. Budget gates must check
settled attempt usage at evaluation time against the effective host budgets, including saved
extensions, and fail on unknown or partial usage for a configured metric.
CI gates check the configured pull request or branch through the host's check
status interface. A string `condition.target` names that pull request or branch.
`condition.target = { node = "write" }` instead resolves the upstream node's
delivered branch and pins its current head SHA. The target must be an upstream
dependency exposing a delivered branch. The host queries checks for that exact
SHA and checks the branch again before accepting a result. A newer push supersedes the
pin, discards stale check evidence and restarts the timeout for the new head;
a result for the previous head must not pass the gate. An observation without
a head SHA keeps the pinned head and restarts nothing; only a different known
SHA supersedes it. Explicitly named checks
must pass. Without named checks, the
host uses required checks from branch protection or rulesets if resolvable;
otherwise every reported check must pass or be skipped, with at least one pass.
The evidence records the applied policy. Failed checks fail. Checks still
registering, including explicitly named checks that have not appeared, leave
the gate waiting; an unavailable host or API, or an invalid check policy, fails
with reason `unavailable`. Pending checks leave the gate waiting,
and fail with reason `timeout` after the configured timeout (two hours by
default). The checkpoint retains the first observation time, last state,
`head_sha`, check names, elapsed seconds and check summary. The compact and full
graph status show a running CI gate's check names, head SHA and elapsed time.
Status refreshes elapsed time from the same host boot's monotonic clock without
querying CI or saving a new checkpoint. Timeout uses the host's monotonic clock,
so wall clock changes do not affect it. If the host boot changes or the monotonic clock moves backwards,
the gate re-anchors its timeout to the new host stamp. The checkpoint retains
the re-anchor count; after three re-anchors, another such change settles the
gate as `timeout` with a reason naming the re-anchor limit. Replay restores
waiting without querying the host; explicit graph advance, resume and attempt
settlement re-evaluate the gate. Graph status reads the saved snapshot.
Pending CI gates persist their next `wake_at` and observation count in the
checkpoint. The graph's `wake_at` is the earliest running CI deadline or open
node ask deadline. The host's `provenant tick` advances only due graphs, checks
the deadline again under the graph journal lock, and honours each gate's own
deadline when several gates share a graph. Terminal and cancelled graphs do
not wake. Restart resumes from the stored deadlines without an in-process timer.

`domains.fabric.graph.ci_poll_backoff` controls `initial_seconds` and
`max_seconds`, both zero by default, making pending gates eligible on every tick. A positive
initial delay doubles after each pending observation up to the maximum, capped
by the remaining CI timeout.
`domains.fabric.graph.ci_poll_overrides.<node_id>` overrides that policy for a
named gate. Host configuration inspection reports effective values and their
origins. Explicit advancement can re-check a gate before its deadline.
For example, set both `initial_seconds = 60` and `max_seconds = 300` in
`[domains.fabric.graph.ci_poll_backoff]` to poll after 60, 120, 240 and then
300 seconds of continued waiting, rounded up to the next tick.
On macOS, one `dev.provenant.tick` LaunchAgent runs every 60 seconds and at
login. It also attempts GC daily, with the next daily deadline persisted under
`tick/cadence.json`; `gc.schedule = false` disables that task alone.
Daily collection runs in a detached one-shot worker under the collector's
existing lock, so slow fetches and cache walks do not delay the next tick.
`tick/gc-result.json` retains its queued, running, complete or failed state;
spawn failures stay due and are retried on the next tick.
Ticks detect interrupted workers through the worker lock and retain an error;
skipped projects and failed upstream observations are reported as incomplete.
Notice batch flushing and opportunistic quota refresh are reserved tick tasks with
no effect until their respective implementations land. Scheduler installation
is a platform seam; a Linux timer is deferred.
Gates must save their decision before dependants run and retain it on replay,
even if budgets change. Gates expose `PASS` or `FAIL` as their typed verdict.
The CI observation also carries `failed_checks`, each with a `name`, optional
`url` and `excerpt`. `condition.failed_checks_max_bytes` defaults to 16384 bytes
and `condition.max_failed_checks` defaults to 20; values in the project graph
override those built-in defaults. Checkpoint `ci.failed_checks_policy` records
the effective byte and count values and whether each came from the condition
or the default layer. The byte cap includes the encoded failed
check records, and `truncated` records omitted or shortened evidence. Both caps
are evidence retention policy and may be adjusted per gate. Failed CI gates
map those checks into typed `findings` outputs, allowing a repair node to
select them with `inputs = [{ node = "ci", outputs = ["findings"] }]`. A passing
gate has no failure findings. Gates never launch a model or create an attempt.

A failure target named by `on_fail` must depend on its gate, so ordinary cycle
and edge limits cover the failure path. Only that target may follow a failed
gate; other dependants and inactive failure paths must be skipped. An unhandled
failure or failed recovery must prevent graph delivery. Dependencies requiring
both outcomes of a gate must be rejected as unreachable. A recovered graph may
deliver once its active paths succeed, with gate evidence and skipped nodes
recorded in the final PR body.

- Why: deterministic decisions need durable evidence without a provider launch.
- Test: `crates/fabric-runtime/src/graph.rs::gate_verdict_fixture_is_attempt_less_and_replays`;
  `::gate_threshold_fixtures_count_missing_verdicts_and_strict_majorities`;
  `::gate_budget_fixtures_fail_closed_and_preserve_the_decision_on_resume`;
  `::gate_on_fail_routes_only_the_active_path_and_delivers_after_recovery`;
  `::gate_inactive_path_waits_for_other_parents_before_settling`;
  `::gate_checkpoint_rejects_attempts_inconsistent_results_and_forged_skips`;
  `::gate_validation_rejects_agent_fields_bad_thresholds_and_failure_cycles`;
  `::gate_rejects_unreachable_joins_of_mutually_exclusive_paths`;
  `::gate_only_graph_obeys_structural_limits_before_evaluating`;
  `::gate_verdict_rejects_spoofed_multiple_and_malformed_lines`;
  `::gate_budget_overrides_warn_only_for_configured_metrics`;
  `crates/fabric-runtime/src/graph/ci_tests.rs::ci_outcomes_are_recorded_and_fail_closed`;
  `::ci_pending_checkpoint_replays_then_passes`;
  `::ci_pending_times_out_using_original_observation_time`;
  `::ci_timeout_ignores_wall_clock_steps_and_reanchors_after_boot`;
  `::ci_backwards_clock_reanchors_are_bounded`;
  `::ci_document_checks_reach_host_query`;
  `::ci_optional_evidence_fields_remain_absent`;
  `::ci_check_names_are_validated`;
  `::pending_ci_does_not_consume_attempt_concurrency`;
  `::ci_failure_activates_recovery_path`;
  `::ci_validation_rejects_bad_targets_and_timeouts`;
  `crates/fabric-runtime/src/graph.rs::agent_json_still_requires_explicit_access`;
  `::legacy_checkpoint_keeps_its_digest_without_delivery_fields`;
  `::gate_skipped_chain_replays_every_save_after_the_gate_decision`;
  `::gate_reachability_handles_two_gates_and_deep_paths`;
  `::gate_reachability_accepts_a_limit_sized_graph_and_rejects_lower_limits`;
  `src/pr/status.rs::tests::branch_requirements_merge_protection_and_paginated_rulesets`;
  `::branch_requirements_reject_incomplete_data_and_empty_sets`;
  `::gate_check_buckets_fail_closed`.
- Owner: `crates/fabric-runtime/src/graph.rs`, `src/fabric.rs`, `src/pr/status.rs`

### Bounded loops

A node with `kind: "loop"` and `access: "read_only"` declares a `loop` object
containing `body`, `until` and a positive `max_rounds`. Optional
`max_extension_rounds` caps each extension decision and defaults to five
(allowed range 1 to 1024). The body is a
self-contained DAG of task, synthesise and gate nodes. `until` names a body
gate that follows every other body node, directly or transitively. Nested loops,
body `on_fail` routes and body delivery are rejected. The wrapper has no
prompt, route, worktree, inputs, condition or delivery configuration. All body
writers must name the same existing worktree; every round continues on its
branch without creating a checkout. Body writer worktrees, including no-op
rounds, remain available through exhaustion, acceptance and cancellation for
explicit delivery or later collection.

The runtime expands the full allowance before admission. Node, edge, prompt,
depth and fan-out limits therefore cover every possible round, including rounds
that later become inactive. Admission counts `max_rounds` copies of the body
plus the wrapper, with cross-round edges included. Budget and concurrency
checks use the ordinary scheduler and attempt telemetry. Extensions must also
fit effective limits and their host ceilings before any additional rounds are
saved. A refusal retains the exhausted checkpoint and its stop/accept path.
The default depth is four and the default ceiling is 64; larger bodies request
an explicit graph limit or host configuration, as shown in the host runbook.
Independent safety bounds reject more than 1024 rounds,
more than 16384 expanded nodes or an expanded node ID longer than 64 bytes.

Within a body, ordinary dependencies and inputs refer to nodes in the current
round. An input named `previous.NODE` selects the preceding round's output
using the existing output selectors and bounded untrusted-data handoff. It
adds the corresponding cross-round dependency and is omitted in round one.
An input naming a node outside the body, resolved through the enclosing
scopes, selects that node's output in every round and adds the dependency;
in round one it merges with the loop's own selection of the same node.
Every later round waits for the preceding `until` gate. A failed `until`
starts the next round; a passing gate succeeds the wrapper and skips unused
rounds. A failed task, synthesise node or other gate fails the loop rather
than consuming another round. The wrapper exposes its own `PASS` or `FAIL`
typed verdict; body results remain on their round nodes.

Each expanded node has the stable ID `LOOP--rN--BODY`, with rounds numbered
from one. Its checkpoint includes `round` metadata: `loop_id`, `round`, the
local `node_id`, whether it is the `until` gate, and `previous_gate` (null in
round one). Existing checkpoint-before-launch, attempt keys, gate decisions
and output records apply to these identities. Replay mid-round must retain
admitted attempts and settled outputs exactly. Graph status exposes these
round records and outputs, including failed checks and skipped future rounds.

If every allowed round settles without passing, the wrapper stays pending and
the graph pauses with an exhaustion reason. Optional `decision_session` names
the originating chair session explicitly; graphs do not infer a session from
their actor. With this field, exhaustion raises one loop ask (see [Node asks and decide
nodes](#node-asks-and-decide-nodes)) per exhausted allowance, with ask ID
`LOOP.rounds-N`. The request offers
`stop`, `extend` and `accept`: stop fails the wrapper, extend adds one round,
and accept succeeds with the existing outputs. A reply of
`{"decision":"extend","rounds":K}` adds a positive integer K within the safety
bounds. The answer is saved on the ask before it is applied, so a crash
re-applies the recorded answer rather than asking again. An answer that cannot
be applied stays on the ask as `rejected` and keeps the exhaustion pause with
its explicit-resume hint. Inbox availability errors retain a per-loop pause and explicit-resume hint;
they do not stop observations of independent running attempts. Status retains
all loop pause reasons when several loops exhaust together.

Without a decision session, or to resolve exhaustion explicitly, resume the
unchanged document with `--extend loop.ID.rounds=N`, where N is an absolute
total allowance greater than the document's `max_rounds`, or with
`--extend loop.ID.decision=stop|accept`. New loop extensions and decisions are
valid only at exhaustion. Saved allowances are monotonic; retrying the same
extension cannot add rounds twice. The original checkpoint and body worktree
remain in use. Existing graph budget and limit extensions still apply.

- Why: repeated review and repair needs bounded admission, exact replay and a
  human decision when the bound is exhausted.
- Test: `crates/fabric-runtime/src/graph/loop_tests.rs::loop_fixture_passes_on_round_two_and_status_retains_outputs`;
  `::loop_fixture_exhausts_and_pauses_without_delivery`;
  `::loop_fixture_resumes_after_extension_without_repeating_rounds`;
  `::loop_replay_mid_round_keeps_attempt_actor_and_input`;
  `::loop_admission_counts_every_round_before_any_launch`;
  `::loop_decisions_stop_or_accept_exhausted_outputs`;
  `::loop_writers_keep_one_worktree_and_never_deliver_between_rounds`;
  `::loop_body_failure_blocks_later_rounds_and_dependants`.
- Owner: `crates/fabric-runtime/src/graph/loops.rs`, `src/fabric.rs`

### Node asks and decide nodes

A node ask is one blocking inbox decision raised by the graph. Loop exhaustion,
a `decide` node and a non-loop graph pause each raise one. Asks live on the
checkpoint in `asks`, keyed by an `ask_id` unique within the checkpoint, so
several can be open at once. Each ask stores its rendered title, body, options,
optional default and deadline, so a replay sends the identical request under
the same inbox operation. A random checkpoint `instance_id` and the
`ask_actor` recorded before the first ask bind every request. Both survive
replay; recreating state for the same document gets a new instance and cannot
reuse an old answer. A checkpoint written before node asks, whose nodes still
carry `loop_asks`, `loop_inbox_error` or `loop_decision_actor`, is not
migrated: advancing or resuming it is refused with a message that names
`graph cancel`, which still works on it, and a rerun under a new `graph_id`.
Cancelling it also supersedes each open loop request under the inbox operation
it was first sent with, without rewriting the checkpoint.

A node with `kind: "decide"` and `access: "read_only"` declares a `decide`
object: a `question`, 2 to 16 `options` (each an `id` that is a valid inbox
option ID, a `label` and an optional `description`), an optional `default`
naming an option, an optional positive `timeout_seconds` whose deadline falls
within the wall-time range (a timeout needs a default; an advance whose
deadline from now would pass the last representable wall time is refused) and an
optional inbox `session`. Without `session`, the graph's
top-level `decision_session` is used; one of the two is required. A decide node
takes no prompt, route, worktree, inputs, delivery, `required_verdict` or
`on_fail`. Limits follow the inbox request contract: the rendered request
body, with the question, must fit the inbox's 8192 bytes, and an option
without a description sends its label as the description. Decide nodes may appear at
the top level, in loop bodies, in composites and, where `plan.kinds` allows
`decide`, in planner fragments. A decide node in a loop body asks once per
round, with the round's node ID in its ask ID.

When a decide node becomes ready, it raises ask `NODE.ask-N` and runs without
an attempt, so a waiting decide node does not count toward `concurrency`. `N`
follows the node's previous ask, so a replacement after cancel and resume
never reuses an ID or its inbox operation. A timeout's deadline is fixed when
the ask is raised; without a host wall clock the advance is refused and
retried, and the ask is not raised without its deadline. The answer, or the default once the deadline has passed, becomes
the node's output: the chosen option ID is its verdict, and its result excerpt
records the ask ID, the option and who answered or that the default was taken.
A verdict gate on that verdict routes the graph; with `on_fail`, the gate picks
one of two edges. A reply whose text, trimmed, is an option ID chooses that
option. Any other answer is saved on the ask with the reason it was rejected,
and the node stays open: once the old request is closed, the next advance asks
again under a new ask ID, and the new body ends with that reason.

With a graph `decision_session`, a non-loop pause such as a budget pause
raises ask `pause-N` with options `resume` and `cancel`. A reply of
`{"decision":"extend","extensions":["budget.KEY=VALUE"]}` raises budgets
first; it is checked like `graph resume --extend` and against the envelope.
Other extensions still need an explicit resume. An answer that cannot be
applied keeps the pause and the next ask names the reason. One pause ask is
open at a time, and the newest eight closed pause asks are kept as history.
When every node settles, a pause that no longer applies is cleared and its
ask is closed before the graph reports complete.

Each answered ask records `answer.source` (`answered` or `default`), the
option or reply text, the answering `actor` and `at`. The runtime reads and
acknowledges an immutable answer and saves it before acting on it; answer
delivery alone does not resume work. Resolved and cancelled asks are superseded
through the inbox's revision-checked close path after the graph transition is
saved. Pending cleanup survives crashes and is retried on graph advancement or
an explicit resume or cancel. A failed poll or close remains visible as the
ask's `inbox_error` until it can be retried.

An ask with a deadline stores `wake_at`, and the checkpoint's `wake_at` is the
earliest deadline among open asks. The inbox request carries the same deadline
and the default as its recommendation. Until a host tick reads `wake_at`, every
graph advance compares the deadline with the host wall clock itself.

- Why: graphs need human decisions at known points, with durable history and a
  safe default when nobody answers.
- Test: `crates/fabric-runtime/src/graph/ask_tests.rs::decide_answer_becomes_the_verdict_and_routes_the_chosen_edge`;
  `::decide_reply_text_naming_an_option_is_accepted_and_other_text_asks_again`;
  `::decide_ids_stay_unique_across_repeated_cancel_and_resume`;
  `::waiting_decide_nodes_do_not_use_up_concurrency`;
  `::decide_deadlines_wait_for_a_clock_instead_of_dropping_the_timeout`;
  `::completion_closes_a_pause_ask_left_by_lowered_limits`;
  `::decide_timeout_persists_wake_at_and_takes_the_default`;
  `::several_decide_asks_are_open_at_once_and_resolve_independently`;
  `::decide_nodes_run_inside_loop_bodies_once_per_round`;
  `::decide_configuration_is_checked_against_the_inbox_contract`;
  `::a_budget_pause_raises_one_ask_and_an_extend_answer_resumes`;
  `::a_pause_answer_can_cancel_and_a_rejected_answer_asks_again`;
  `::a_planner_fragment_can_add_a_decide_node_when_its_kinds_allow_it`;
  `::decide_timeouts_past_a_century_are_kept_until_the_deadline_overflows`;
  `crates/fabric-runtime/src/graph/loop_tests.rs::loop_checkpoint_from_before_node_asks_refuses_to_advance_but_can_be_cancelled`;
  `src/fabric.rs::graph_ask_inbox_tests::graph_ask_inbox_replays_requests_and_acknowledged_answers`;
  `::graph_decide_asks_stay_fresh_across_repeated_cancel_and_resume`;
  `::cancel_closes_a_legacy_loop_request_by_its_original_operation`;
  `src/fabric/loop_repair_tests.rs::bound_loop_inbox_keeps_original_actor_under_another_client`.
- Owner: `crates/fabric-runtime/src/graph/asks.rs`, `src/fabric.rs`

### Land nodes

A node with `kind: "land"` and `access: "read_only"` declares a `land`
object: `node`, an upstream writer whose `delivery` is `pr`; `tier`, one of
`routine`, `substantial`, `crucial` or `terminal`; and an optional inbox
`session` that falls back to the graph's `decision_session`. It takes no
prompt, route, worktree, inputs, delivery, `required_verdict`, `on_fail` or
`continues`. Its ancestors must include a verdict gate and a CI gate that
each follow the writer. Land nodes are refused in loop bodies.

When ready, the node pins the branch and newest commit delivered by the
writer and the writers that continue it. A passing verdict gate after the
writer must have reviewed that head: each of its inputs is an agent attempt
outside the writer's chain, never a gate, loop, decision or land output such
as CI's `PASS`, and judged the head
through its pinned `source` commit or, without one, as the newest head of the
writer's chain upstream of it. A composite input has no subject of its own, so
it resolves to the member that exported its verdict, recursively: a council
pinned to an earlier head never vouches for a later one that merely precedes
its boundary. Otherwise the node fails without calling the
host and names the heads that were reviewed, so a failed gate whose `on_fail`
reaches the land, or a review that predates a continuation, never lands. It
then resolves the host's tier policy
and saves both in the node's `land` record with the layer the policy came
from (`default`, `config` or `project`). With policy `auto` the record says
the policy authorised the land, and the host lands through the same path as
`provenant land N --head HEAD`: it finds the PR for the branch, checks CI
once, merges with `--match-head-commit` and runs the usual cleanup, so the
repository's own merge requirements still apply. With `ask` it raises ask
`NODE.ask-1` with the single option `hold`, naming `provenant land N --head
HEAD`, and only observes the PR: it never merges. Once the chair has merged
the PR at the pinned head, the node records the ask ID as the chair's
authority. An inbox answer never approves a land, because lanes can write the
Fabric store and configured peers can answer: `hold`, or any other answer,
fails the node. A tier that asks with no session fails the node and names
`provenant land` for a manual land. The host finds the PR by head branch,
never by `gh pr view BRANCH`, which reads a numeric branch as a PR number: it
needs exactly one same-repository PR that is open or already at the pinned
head. A merged PR succeeds the node with verdict `LANDED`, the branch and head
as outputs and an excerpt that names who or what authorised it. Pending checks, an unknown merge
state or an unavailable PR keep the node running and save a retry deadline,
spaced by the node's CI poll backoff, that host tick wake discovery includes; a
moved head, failed checks, a closed PR or a refused merge fail it. A waiting
land node does not count toward `concurrency`, and a paused graph does not
land.

- Why: delivery should finish without a chair for routine work while crucial
  work still waits for a person, under one policy the host owns. Inbox
  records and CI output are writable or deterministic, so neither is landing
  authority or review evidence.
- Test: `crates/fabric-runtime/src/graph/land_tests.rs::an_auto_tier_lands_the_newest_delivered_head_through_the_host`;
  `::a_crucial_tier_asks_and_succeeds_only_once_the_chair_merges_by_hand`;
  `::an_inbox_answer_of_land_never_approves_a_land`;
  `::a_hold_answer_fails_the_land_without_merging`;
  `::ci_output_never_substitutes_for_review`;
  `::a_configured_policy_overrides_the_default_and_records_its_source`;
  `::an_ask_tier_without_a_session_fails_with_guidance`;
  `::a_pending_land_is_observed_again_and_a_refusal_fails_the_node`;
  `::a_pending_land_persists_a_retry_deadline_that_survives_restart`;
  `::a_failed_review_routed_to_land_never_calls_the_host`;
  `::a_review_of_an_earlier_head_never_lands_a_continuation`;
  `::a_composite_verdict_binds_to_the_head_its_producer_reviewed`;
  `::a_loop_body_cannot_land`;
  `crates/fabric-runtime/src/config.rs::land_policy_layers_project_then_host_then_default`;
  `src/pr/land.rs::graph_land_tests::a_land_report_maps_to_a_graph_observation`;
  `src/pr.rs::branch_pull_tests::a_numeric_branch_resolves_to_its_unique_same_repository_pull_request`;
  `src/preset_deliver_tests.rs::repository_layers_never_lower_the_land_tier_or_add_a_land`.
- Owner: `crates/fabric-runtime/src/graph/land.rs`, `src/pr/land.rs`,
  `src/fabric.rs`

### Plan expansion and honest-stop continuation

A read-only `plan` node must declare `plan.kinds` and a positive
`plan.max_nodes` (at most 512). Its result must contain a complete fenced
`fragment` JSON block, schema version 1, within 12 KiB. Agent children require
a goal or brief, dependencies and acceptance checks; writers also require
relative literal write scopes, while read-only children may omit them. Omitted
routes resolve through `senior`. Gates use their deterministic
condition and decide nodes their `decide` object. Supported fragment kinds are
`task`, `plan`, `synthesise`, `gate` and `decide`.
The kind allow-list is the extension point for future fragment capabilities;
presets and review shapes are not interpreted here.

The runtime must validate the entire prospective graph against effective
limits and the host ceiling before saving any addition. Invalid, missing or
over-bound fragments must pause with a node-specific reason and admit no
consumers. Validated additions must retain the original plan digest, stable
namespaced node IDs and existing attempt keys across checkpoint replay.
The bounded fragment JSON must survive refusal in the checkpoint so a limit
extension can retry admission without rerunning the planner. Namespaced IDs
must fit within 64 bytes; refusals name the source and child to shorten.
Consumers must wait for all added work. Plans within loop rounds must retain
their round membership and the loop's shared writer checkout after persistence.
Within one scheduler advance, each validated addition updates the in-memory
graph and checkpoint before scheduling continues. The caller must hold exclusive
access to the journal for the whole advance. The journal is not reloaded for
each addition.

Disjoint scoped writers in separate registered checkouts may run concurrently.
Planner-emitted overlapping scopes, including transitive overlaps and identical
checkouts, must share one checkout and serial admission. Such components require
a graph integration branch; their writers use existing integrate delivery and the
graph owns at most one final PR. A planner never chooses a destination: a
fragment node with integrate delivery is refused unless the document names an
`integration_branch`, and its destination is always that branch. Write scope is a scheduling declaration and
part of the brief; existing worktree confinement remains the write boundary.
The admission fence checks every writer, including original document nodes;
an unspecified or noncanonical scope overlaps all scopes. Skipped or blocked
writers do not constrain checkout selection for later fragments. Worktree
custody excludes concurrent managed writers across graphs.

A successful writer task may report a structured `handoff` block with schema
version 2, status `stopped_early`, a non-empty note, and `remaining[]`,
`decisions[]` and `verify[]`. It uses the shared handoff codec and bounds from
[the run contract](run.md#a-partial-finish-is-flagged-not-failed). The fence
must end the answer; `done` and `blocked` do not request graph continuation.
This must create exactly one continuation per stopped node, in the same
checkout, carrying the handoff and original completion contract. Consumers'
selected outputs must follow the continuation. Prose alone must never trigger
continuation. Provider or delivery failures remain failures. Graph writer
checkouts must survive settlement for subsequent work.
Malformed optional handoffs produce output warnings and preserve the writer's
completion verdict contract; they must not pause a successful graph.

The per-graph `continuations` limit defaults to 2; 0 disables admission of
continuations. Exhaustion must pause before consumers or final delivery.
Extensions use the existing limit and ceiling mechanism. Checkpoint status
must expose validated expansions, handoffs, pause reasons and remaining node,
edge and continuation slots, including when every current attempt has settled.

- Why: bounded slicing and honest handoffs should advance through the same
  scheduler, persistence and delivery path as static graphs and loops.
- Test: `crates/fabric-runtime/src/graph.rs::plan_fixture_expands_and_replays_without_replanning`;
  `crates/fabric-runtime/src/graph/plan_tests.rs::plan_validates_effective_limits_and_ceiling_before_saving`;
  `::plan_overlapping_writers_share_one_worktree_integrate_and_deliver_once`;
  `::plan_disjoint_writers_are_admitted_while_a_peer_is_running`;
  `::planner_requires_write_scope_only_for_writers`;
  `::planner_integrate_delivery_needs_the_documents_integration_branch`;
  `::honest_stop_continues_once_on_same_checkout_with_handoff_and_verdict_contract`;
  `::continuation_budget_pauses_then_extends_without_duplicating_prior_work`;
  `::expansion_at_node_budget_reads_journal_once`;
  `::plan_inside_loop_keeps_round_membership_after_checkpoint_serialisation`;
  `src/fabric.rs::graph_status_handles_pause_left_after_terminal_checkpoint`.
- Owner: `crates/fabric-runtime/src/graph/plans.rs`, `src/fabric.rs`

### Typed node outputs

Settled nodes record version 2 outputs in the checkpoint, exposed by graph
status JSON. A result's `digest` is `sha256:` followed by the lowercase
SHA-256 of the complete stored result's raw bytes. This identifies the stored
artifact: it is not a hash of the excerpt or of the redacted output returned by
run output reads, and cannot be verified by hashing those redacted bytes when
redaction changed the content. The runtime applies stateful credential
redaction while streaming the result. Its `excerpt` is the longest UTF-8 prefix
of the redacted text whose canonical JSON encoding fits in 4096 bytes, so
control characters count at their escaped size. `truncated` indicates that the
stored result, the redacted text or the encoded excerpt exceeded that window. A local attempt may also
have a run-file path. The runtime retains only the excerpt and a 16 KiB tail
of the redacted text for parsing.

The optional typed verdict must be the final non-empty standalone line of the
complete redacted stream, ignoring a closed `handoff` fence (and blank lines)
that follows it. Its syntax is `VERDICT: TOKEN`, with an uppercase
ASCII token containing only letters, digits, underscores or hyphens. The line
must fit within 1024 bytes, including surrounding whitespace. A declaration is a
line that starts with `verdict:` (any case) after optional whitespace, markdown
emphasis, list markers (`-`, `*`, `+`, `1.`, `1)`) or heading marks, judged from
the start of the line even when it is overlong. Mentions in prose, and
blockquoted lines, are not declarations. More than one declaration anywhere in
the stream, even outside the retained windows, invalidates the evidence. Case
differences, trailing text, an overlong line or an unterminated trailing
`handoff` fence also invalidate it. Invalid evidence is recorded
in `verdict_error` and must fail any consuming verdict gate. A missing verdict
is a non-match; a matched `required_verdict` contract must not override invalid
typed evidence.
Run-store integrity verification rejects incomplete artifacts before their
verdicts can be consumed; excerpt truncation alone does not invalidate a verdict.

Findings use a fenced `findings` JSON block with `schema_version: 1`
and a `findings` array; each item has string fields `id`, `severity`, `file`, `summary` and
`status`, and an optional non-negative integer `line` (which may be null).
Unknown fields or a different schema version make the block invalid. Parsing
of findings only uses complete lines and blocks within a window, so content
in the middle of a large result may not be captured. A block may contain at most 32 findings
and 8 KiB of JSON, and text fields are
limited to 1024 bytes each; a block with an overlong field is invalid. Missing
or invalid findings produce an empty list and, when a result exists, a warning,
not a node failure. The final findings block in the tail window takes
precedence; if it is absent, the final block in the head window is used. An
invalid final block yields an empty list and warning.
The separate `required_verdict` contract checks for its exact line across the
complete redacted stream. Writers can also provide a branch or commit.

A node's `inputs` list selects `result`, `verdict`, `findings`, `branch`,
`commit` or `remaining` from named direct dependencies. `remaining` is the
`remaining[]` list of the handoff v2 block the node declared, in any status,
rendered as a JSON array of at most 4096 canonical bytes (leading items kept, with a warning when cut); it is null when no valid
handoff was declared or it lists no items. It is the only carrier of
unfinished work across a clipped `result` excerpt. Decision (#2279): the
earlier closing-window selector is removed rather than kept as a fallback,
because handoff v2 is bounded, structured and parsed from the complete stream;
a provider that emits no handoff gets the head excerpt and digest only. Each dependency and output selector
may occur only once. The selected data is rendered into its brief, which is
bounded by the effective per-node prompt byte limit. If the node prompt leaves
no room for the block, or the selected outputs do not fit, the node fails
before its next attempt is admitted with a per-node advance error. A failure
before a fallback preserves the earlier attempt's outcome and creates no
fallback attempt. An admitted attempt retains its admission cap on replay; a
node without selected inputs replays its full admitted prompt even if the
limit is lowered. Increase `node_prompt_bytes` within the host ceiling and start a new
graph to retry a failed node. A document prompt exceeding a structural limit
instead pauses the graph; it may be resumed after extending that limit.
Selected outputs appear in a delimited block labelled as untrusted data, with
delimiter collisions escaped. Result digests and excerpts are passed instead
of transcripts. Paths remain visible in status but are omitted from downstream
briefs.
- Why: downstream nodes can use bounded evidence without copying transcripts.
- Test: `crates/fabric-runtime/src/graph.rs::outputs_feed_direct_dependants_and_survive_replay`;
  `::output_parsing_is_lenient_and_rendering_fails_instead_of_omitting`;
  `::findings_and_verdict_at_the_end_of_a_large_result_are_parsed`;
  `::matched_required_contract_cannot_override_ambiguous_verdict_evidence`;
  `::upstream_data_boundary_cannot_be_closed_by_result_content`;
  `crates/records/src/result_capture.rs::hashes_all_bytes_and_keeps_bounded_windows`;
  `crates/records/src/result_capture.rs::verdict_evidence_is_unique_final_and_independent_of_chunk_boundaries`;
  `crates/run/src/tests.rs::streamed_result_is_verified_and_redacted_across_chunks`;
  `crates/run/src/tests.rs::streamed_verdict_evidence_validates_complete_large_artifacts`;
  `src/fabric.rs::graph_owner_fails_node_when_result_integrity_is_lost`
- Owner: `crates/fabric-runtime/src/graph.rs`

### Checkpoint before launch

The owner writes a checkpoint under a stable graph, node and route key
before calling run, so replay returns the existing attempt. A checkpoint
missing launch evidence or the submitting actor is refused, and digests
detect tampering. Cancelling a graph persists the admission fence first.
- Why: a restart must never launch a node twice.
- Test: `crates/fabric-runtime/src/graph.rs::checkpoint_attempts_require_launch_evidence_and_actor`;
  `::cancelled_graph_admits_no_work_and_resume_reuses_the_attempt_key`
- Owner: `crates/fabric-runtime/src/graph.rs`

### Checkpoint envelope

A graph checkpoint, with its document and host directory, must stay within
16 MiB of canonical JSON. Before the first save, the runtime computes a
worst-case size for the expanded plan and refuses a plan that could exceed the
envelope. The refusal names the bound, for example `graph checkpoint could
reach 25903705 bytes, over its 16777216-byte limit`. Host preflight shows the
same message. The bound counts the document and admitted expansions at their
actual size. To that it adds, for each planned node, the most its progress row
can hold:

| Stored text | Canonical cap |
| --- | --- |
| Result excerpt (composite) | 4096 (65536) bytes |
| Task findings list | 9216 bytes, otherwise the block is invalid |
| CI failed checks | `failed_checks_max_bytes` per gate, 16384 bytes by default; at most `max_failed_checks`, 20 by default |
| Warning | 512 bytes each, at most 4 |
| Verdict, path, branch, commit, gate reason, CI summary, PR URL | 1024 bytes each |
| Advance error, expansion pause, loop reason | 2048 bytes each |
| Usage failure and unknown-cost reason | 512 bytes each |
| Graph pause reason | 4096 bytes |
| Planner fragment and handoff note | 24576 and 8192 bytes, otherwise invalid |
| Host attempt reference, actor and boot identity | 256 bytes each, reserved |
| Ask answer text, option ID | 1024 and 128 bytes; longer text is recorded as rejected |

The runtime cuts each runtime string to its cap when it stores it. Lists that
grow during a run keep only their newest entries and count the rest:
`unknown_usage` keeps the 64 lines of the most recently settled attempts,
ordered by each attempt's `settled` sequence (a delivery retry clears it, so
the retried attempt is ordered by when it settles again), with
`unknown_usage_omitted`, and
`usage_events` keeps 128 events with `usage_events_omitted`. Events are emitted
once, when their attempt settles. Extensions have no count or length cap of
their own; the bound counts the saved extensions at their actual size, and an
extension that would take the checkpoint past the envelope is refused with
that message. The bound also reserves every ask the plan can hold: one per
loop round with a decision session, one per decide node and, with a graph
`decision_session`, the kept pause asks plus one open and one awaiting close.
A planner admission or loop extension whose plan would exceed the envelope
pauses with that message instead of growing the checkpoint. A checkpoint loads under its
byte limit alone: the member limit is half the byte limit, so a checkpoint that
saved can be read back.
- Why: a graph that was accepted cannot later stop on a checkpoint too large
  to save or load.
- Test: `crates/fabric-runtime/src/graph/envelope_tests.rs::worst_case_checkpoint_fits_its_bound`;
  `::an_oversized_graph_is_refused_before_its_first_save_naming_the_size`;
  `::a_graph_at_the_default_node_limit_is_admitted`;
  `::a_loop_extension_past_the_envelope_pauses_instead_of_growing`;
  `::usage_events_keep_the_newest_and_count_the_rest_without_churn`;
  `::usage_warnings_keep_the_most_recently_settled_attempts`;
  `::a_delivery_retry_takes_a_fresh_settlement_order`;
  `::long_and_many_extensions_are_kept_and_count_toward_the_envelope`;
  `::a_checkpoint_within_the_byte_limit_loads_past_the_old_member_limit`
- Owner: `crates/fabric-runtime/src/graph/envelope.rs`

### Fallback only for unavailability

Only a terminal `auth_required`, `usage_limited` or `adapter_unavailable`
moves a node to the next route. Model capacity failures are treated as model
unavailability for this purpose. Task failure, input requirements and uncertain
launches never fall back; after launch only read-only nodes may fall back. A failed
writer keeps its worktree. Graph writers deliver only when their node explicitly requests it.
An unreadable or integrity-invalid result fails the node without fallback,
regardless of the provider outcome.
- Why: retrying a writer elsewhere would duplicate edits.
- Test: `crates/fabric-runtime/src/graph.rs::writer_limit_after_launch_never_tries_a_second_route`;
  `::terminal_task_failure_does_not_try_a_fallback_route`
- Owner: `crates/fabric-runtime/src/graph.rs`

### Settlement advances graphs

After a run settles and its supervisor releases custody and writer locks,
the supervisor advances pending graphs from the journal's retained document. A
failed advance is recorded as `advance_error` until the next success;
`resume` is the manual path. A lane settlement error is recorded only on graphs
containing that attempt and does not stop other graphs from advancing.
- Why: dependants progress without a daemon.
- Test: `crates/fabric-runtime/src/graph.rs::advance_error_is_recorded_replaced_and_cleared_and_rebind_moves_directory`;
  `tests/dispatch_provider.rs::graph_settlement_advances_writer_and_account_routed_review`;
  `src/fabric.rs::settlement_failure_in_one_graph_does_not_stop_another_graph`
- Owner: `crates/fabric-runtime/src/graph.rs`

## Landing

### Fenced fast-forward push

Landing runs on the home host under a per-ref OS lock, binding a generation
and token to the expected and candidate OIDs. The operation is marked
`pushing` first, and expiry cannot admit a successor while it is
unresolved. The push proves a fast-forward and uses
`--force-with-lease=<ref>:<expected>`. Landing refuses origin URLs with credentials or
options. After a lost reply it reconciles the remote OID; without proof, the
outcome stays unknown and keeps the fence.
- Why: two landers must never overwrite each other's push.
- Test: `crates/fabric-runtime/src/landing_tests.rs::lost_push_reply_keeps_successor_fenced_until_remote_reconciliation`;
  `::local_url_rewrite_is_refused_before_it_can_push_to_another_repository`
- Owner: `crates/fabric-runtime/src/coordination.rs`

### Fence tokens stay secret

The landing fence token comes from `PROVENANT_LANDING_TOKEN`, never an
argument, and no token reaches an error renderer. `landing wait` never
acquires the fence.
- Why: argv is visible to every process on the host.
- Test: `tests/fabric.rs::landing_fence_reads_secret_from_environment_without_a_token_argument`;
  `::malformed_fence_tokens_never_reach_either_error_renderer`
- Owner: `src/fabric.rs`

## Peer hosts

Fabric works across the user's own Macs over SSH; each host keeps running
its own lanes when others sleep.

### Owners are authoritative

Each host is authoritative for the records it created and accepts writes only
through its own ingress. Only the owner reports liveness, so an unreachable
peer is `Unknown`, never dead. Each project has one home host for mail,
tasks, events, claims and landing, with no outbox or election; an
unreachable home is `HomeUnavailable` while local runs continue. A selector
matching several hosts is refused.
- Why: remote guesses or split-brain state could free a live lane.
- Test: `tests/fabric_transport.rs::run_status_and_wait_read_the_owner_store_and_event_pull_deduplicates`;
  `crates/fabric-runtime/src/home.rs::remote_coordination_recovers_lost_reply_by_receipt_then_replays_locally`.
  Ambiguous-selector refusal has no direct test (gap).
- Owner: `crates/fabric-runtime/src/hosts.rs`, `crates/fabric-runtime/src/home.rs`

### Fixed peer ingress

Peers use system SSH with no listener or credential of Provenant's own. The
remote command is the fixed `provenant fabric peer`, which takes one framed
request, never a command string. Ingress refuses another `protocol` before
decoding, and refuses an unknown source, its own host ID as source or a
`trace.host` other than the sender. There is no version negotiation.
The claimed `source_host` must name a configured peer, and the claimed agent
must form a valid actor address with that host. Neither claim is bound to the
SSH session; this is the existing configured-peer trust model.
- Why: the peer key is restricted to one command; nothing else may run.
- Test: `tests/fabric_transport.rs::peer_ingress_refuses_protocol_before_decoding_or_running_mutations`;
  `::peer_ingress_refuses_a_trace_naming_another_host_than_the_sender`
- Owner: `crates/fabric-runtime/src/hosts.rs`

### Remote mutations are never resent

A remote mutation is prepared durably, marked sent and recorded committed or
unknown. If its outcome is uncertain, Fabric asks only for the home host's receipt and
never repeats it or falls back locally, and a sent but unanswered launch is
`launch_unknown`. A peer error naming another operation, or a reply naming
the wrong attempt, is `state_corrupt`. Absolute paths under the user's home directory travel relative to it; others
are refused. Cancelling a peer call stops the SSH
child, never the remote run.
- Why: an SSH timeout after execution looks like a failure.
- Test: `crates/fabric-runtime/src/hosts.rs::fixed_peer_transport_preserves_mutation_uncertainty_without_retry`;
  `crates/fabric-runtime/src/home.rs::remote_coordination_read_rejects_forged_operation_errors`.
  Refusing absolute paths outside the home directory and the cancel scope have no direct
  test (gaps).
- Owner: `crates/fabric-runtime/src/hosts.rs`

### Peer agent and code transport

A per-user LaunchAgent relays SSH requests into the login session, one
thread per connection with no cap of its own; only a refused or missing
socket falls back to in-process handling, and a sent request is never
repeated. A peer writer takes its base from the shared remote with its own
Git authority and hooks off, refuses an unreachable base and transports only
committed heads. The sender binds `refs/provenant/transport/<op>/export`; the
receiver runs `git fetch` with `fetch.fsckObjects`, `transfer.fsckObjects`
and a forced `index-pack` into `<op>/incoming`, then promotes `<op>/verified-head`
after exact OID and ancestry checks. Git's receive path refuses corrupt,
missing, duplicate and malformed objects; there is no size cap, only a
30-minute fetch bound.
- Why: sign-in works over SSH without forwarding credentials, no dirty tree
  crosses hosts, and Git already verifies what it receives.
- Test: `crates/fabric-runtime/src/peer_agent.rs::forward_falls_back_only_when_no_agent_accepts`;
  `::requests_beyond_sixteen_held_connections_are_relayed`;
  `crates/fabric-runtime/src/git_transport.rs::dirty_worktree_and_untrusted_remote_inputs_refuse_before_transfer`;
  `::commits_over_64_mib_transport`;
  `::fetch_refuses_corrupt_missing_duplicate_and_malformed_objects`;
  `::fetch_refuses_a_corrupt_pack`
- Owner: `crates/fabric-runtime/src/peer_agent.rs`, `crates/fabric-runtime/src/git_transport.rs`

### Credentials never cross hosts

Provenant never copies, forwards or reads provider credentials, SSH keys or
keychains across hosts; sandbox policy applies on the executing host.
- Why: each host's sign-in stays under that host's user.
- Test: none (gap); peer request types carry no credential fields.
- Owner: `crates/fabric-runtime/src/hosts.rs`

### Peer changes need real-host acceptance

Peer changes need exact-head CI and a chair-run smoke between two real hosts.
- Why: fake SSH tests do not establish two-host behaviour.
- Test: none (review gate); the chair's smoke result is the evidence.
- Owner: the chair

## Not provided

- Named sessions, teams, mail acknowledgement and a task dependency API.
- Host auto-placement, moving a running lane, or local fallback after an
  explicit host choice.
- Shared or replicated SQLite, a daemon or listener, and the async `rmcp` SDK.

### Project-local scratch

Operator graph documents default to `.provenant/scratch/graphs/`; an existing
`.agent-run/` is reused as the scratch root. Bare document names resolve there
when no explicitly named file exists in the current directory. Explicit project-local
paths remain supported. Scratch setup appends its anchored directory pattern to
Git's common `info/exclude` under a stable lock, preserving existing bytes and
never editing tracked `.gitignore`. Symlinked scratch roots and exclude files
are refused. Writer lanes get a
read-only grant on the primary checkout's scratch root (never a symlink), so
briefs there stay readable from a linked worktree. The graph path error and CLI help name these directories. Scratch
is disposable operator data, separate from authoritative run state; expiry is
owned by GC rather than graph execution.
- Why: graph documents and lane briefs need a discoverable project-local home
  without creating tracked configuration changes.
- Test: `crates/confinement/src/scratch.rs::scratch_is_local_excluded_idempotent_and_refuses_links`;
  `crates/cli/src/run/provider.rs::writers_read_the_primary_checkouts_untracked_scratch`.
- Owner: `crates/confinement/src/scratch.rs`, `crates/cli/src/run/provider.rs`, `src/fabric.rs`

### Graph delivery

Nodes accept the same optional string `delivery` as dispatch: `none`, `push`,
`integrate:BRANCH` or `pr`. Read-only nodes cannot deliver. A graph may declare
`integration_branch` and `delivery: "pr"`; this excludes node PR deliveries.
Writer integration happens on the shared host delivery path under its per-ref
lock. A failed or unattempted delivery fails the node and blocks dependants.

Before dispatch, every integration target is checked against the default and
primary checkout branches, and node targets must match the graph's declared
integration branch. The graph uses the integration base OID pinned in its host
execution record before the first node is admitted. Replays preserve that base;
final PR commit messages
cover only `base..integration-head`, regardless of the primary checkout's HEAD.
A legacy graph without a recorded base must use a new graph ID for PR delivery.
Once all active paths succeed, gate failures are handled and custody is released, the graph publishes local
integration through a separately created graph branch,
recovering an interrupted publication when its unmarked ref still points at
the exact graph head. A matching sealed marker also permits recreating an absent
publication ref at that same head. A different marker, a different ref head or
a symbolic ref refuses. Alternatively,
the graph uses its existing remote integration branch, and opens one PR. Its body
contains bounded commit messages, run and attempt IDs, gate decisions and skipped nodes.
Graph review-council attempts render as the same `Independent review` rows used
by `pr open`, with each review role, model family and recorded verdict. The rows
include lens, verification and synthesis attempts, including retries, while
subject-framing tasks remain ordinary node evidence. The checkpoint
records the PR URL and successful final delivery;
replay observes it. A failed final delivery keeps its advance error and remains
eligible for advancement or manual resume. Neither conflicts nor unknown
launches are auto-resolved.
- Why: lanes can feed one integration branch while the graph owns the single
  review boundary and reuses run's delivery checks.
- Test: `crates/fabric-runtime/src/graph.rs::graph_delivers_nodes_and_opens_one_final_pr_only_after_success`;
  `::legacy_checkpoint_keeps_its_digest_without_delivery_fields`;
  `src/fabric.rs::graph_submission_pins_the_integration_base_across_replay`;
  `crates/run/src/delivery_tests.rs::graph_pr_body_uses_its_recorded_base_despite_primary_checkout_changes`;
  `crates/run/src/delivery_tests.rs::local_integration_conflict_dirty_and_checked_out_refuse_without_partial_state`.
- Owner: `crates/fabric-runtime/src/graph.rs`, `src/fabric.rs`
