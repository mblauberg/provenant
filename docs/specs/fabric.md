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

Fabric refuses coordination mutations from lanes at the tool surface
([nested lanes](routing.md#lane-identity-and-refusals)), and `fabric peer`
and `peer-agent` refuse to start under the lane environment. Lanes still
have direct write access to `fabric.sqlite3`, because opening the store
needs a writable WAL connection.
- Why: the tool refusal is today's boundary; the residual is recorded.
- Test: `tests/fabric_transport.rs::lane_graph_status_does_not_create_shared_graph_store`.
  The direct SQLite write grant is an open gap.
- Owner: `crates/fabric-runtime/src/coordination.rs`

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

Human requests, immutable answers, revision history and the delivery outbox
belong to Fabric. CLI and MCP must use the same service. The service binds
project, host and actor from Fabric identity; request JSON cannot supply them.
The chair declares its exact session explicitly. This declaration is not
session discovery or evidence of a live provider connection.
- Why: a shared agent label cannot identify a particular chair session.
- Test: `tests/inbox.rs::inbox_cli_refuses_forged_ownership_and_closes_without_approval`;
  `crates/fabric-runtime/src/inbox/tests.rs::inbox_acknowledgement_requires_the_exact_host_project_session_and_actor`
- Owner: `crates/fabric-runtime/src/inbox.rs`, `crates/records/src/inbox.rs`

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

The inbox must refuse a configured remote home rather than opening a local
mirror. Reads disclose local-only coverage and create no session records.
Paging binds project, host, session, state, page size and request snapshot;
changed filters or snapshots require an explicit restart. New observations
invoke no model, provider probe, polling loop or background process.
- Why: unknown peer/session coverage must remain unknown.
- Test: `crates/fabric-runtime/src/inbox/tests.rs::inbox_remote_owner_and_worker_mutations_are_refused_without_opening_store`;
  `crates/fabric-runtime/src/inbox/tests.rs::inbox_paging_binds_filters_snapshot_and_limits`
- Owner: `crates/fabric-runtime/src/inbox.rs`

## CLI and MCP surfaces

The tool catalogue is generated reference in
[`docs/reference/mcp-tools.md`](../reference/mcp-tools.md); CLI leaves are in
the [CLI reference](../reference/cli.md).

The default MCP catalogue exposes run and output operations, messaging and
identity, plus `fabric_discover`. Discovery returns the schemas for graph,
lease, task and event operations, which remain callable by their tool names.
Run submission can wait for a result in the same bounded call; a timeout
returns a receipt and continuation instructions. Run listings are bounded and
newest first, and compact status, wait and cancel responses can request detail.
Run operation IDs are generated when omitted and returned for retries;
explicit IDs preserve planned retry identity.

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

A graph must obey the configured structural limits and budgets. Each attempt
is also bounded by `limits.graph_timeout_seconds`. The [host runbook](../runbooks/fabric-hosts.md#configure-hosts)
describes defaults, ceilings, budget telemetry and resuming a paused graph.

Each node has an optional task `title`, defaulting to its ID; titles use the
shared task title validation and appear in lanes and graph status. A node may
specify a single `route` or an ordered `routes` list for agent work. A route can name a pool
with just `alias`, resolved as for `dispatch --route`, or supply an explicit
adapter. Routes require an adapter or alias and may mix these forms in a list.

An agent node may set `kind` to `synthesise` and select one or more direct
dependencies with `inputs`. Its configured prompt states the synthesis purpose.
The runtime uses the same bounded, untrusted-data handoff and typed settlement
as a task. Omitting `kind`, or setting it to `task`, preserves task behaviour.

Why: one configurable agent can combine results for different purposes without
adding a node kind for each purpose.
Test: `synthesise_receives_selected_reviewer_findings_and_emits_typed_output`.
Owner: Fabric runtime.

### Deterministic gates

A `gate` must evaluate its condition without a model or attempt. Verdict gates
must compare selected direct dependencies' verdicts exactly, with missing
verdicts counting as non-matches. Majority requires strictly more than half;
count requires at least the configured positive count. Budget gates must check
settled attempt usage against the effective host budgets, including saved
extensions, and fail on unknown or partial usage for a configured metric.
Gates must save their decision before dependants run and retain it on replay,
even if budgets change. Gates expose `PASS` or `FAIL` as their typed verdict.

A failure target named by `on_fail` must depend on its gate, so ordinary cycle
and edge limits cover the failure path. Only that target may follow a failed
gate; other dependants and inactive failure paths must be skipped. An unhandled
failure or failed recovery must prevent graph delivery. A recovered graph may
deliver once its active paths succeed, with gate evidence and skipped nodes
recorded in the final PR body.

- Why: deterministic decisions need durable evidence without a provider launch.
- Test: `crates/fabric-runtime/src/graph.rs::gate_verdict_fixture_is_attempt_less_and_replays`;
  `::gate_threshold_fixtures_count_missing_verdicts_and_strict_majorities`;
  `::gate_budget_fixtures_fail_closed_and_preserve_the_decision_on_resume`;
  `::gate_on_fail_routes_only_the_active_path_and_delivers_after_recovery`;
  `::gate_inactive_path_waits_for_other_parents_before_settling`;
  `::gate_checkpoint_rejects_attempts_inconsistent_results_and_forged_skips`;
  `::gate_validation_rejects_agent_fields_bad_thresholds_and_failure_cycles`.
- Owner: `crates/fabric-runtime/src/graph.rs`, `src/fabric.rs`

### Typed node outputs

Settled nodes record version 2 outputs in the checkpoint, exposed by graph
status JSON. A result's `digest` is `sha256:` followed by the lowercase
SHA-256 of the complete stored result's raw bytes. This identifies the stored
artifact: it is not a hash of the excerpt or of the redacted output returned by
run output reads, and cannot be verified by hashing those redacted bytes when
redaction changed the content. The runtime applies stateful credential
redaction while streaming the result. Its `excerpt` is a UTF-8 prefix of at
most 4096 bytes of the redacted text, and `truncated` indicates that either the
stored result or redacted text exceeded that window. A local attempt may also
have a run-file path. The runtime retains only the excerpt and a 16 KiB tail
of the redacted text for parsing.

When `required_verdict` matches, the recorded verdict is that exact trimmed
line. Otherwise the optional verdict is the last `VERDICT:` line in the tail
window, falling back to the last such line in the head window, and may be
absent. Findings use a fenced `findings` JSON block with `schema_version: 1`
and a `findings` array; each item has string fields `id`, `severity`, `file`, `summary` and
`status`, and an optional non-negative integer `line` (which may be null).
Unknown fields or a different schema version make the block invalid. Parsing
only uses complete lines and blocks within a window, so content in the middle
of a large result may not be captured. A block may contain at most 32 findings
and 8 KiB of JSON, and text fields are
limited to 1024 bytes each; a block with an overlong field is invalid. Missing
or invalid findings produce an empty list and, when a result exists, a warning,
not a node failure. The final findings block in the tail window takes
precedence; if it is absent, the final block in the head window is used. An
invalid final block yields an empty list and warning.
The separate `required_verdict` contract checks for its exact line across the
complete redacted stream. Writers can also provide a branch or commit.

A node's `inputs` list selects `result`, `verdict`, `findings`, `branch` or
`commit` from named direct dependencies. Each dependency and output selector
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
  `::matched_required_verdict_takes_precedence_even_in_middle`;
  `::upstream_data_boundary_cannot_be_closed_by_result_content`;
  `crates/records/src/result_capture.rs::hashes_all_bytes_and_keeps_bounded_windows`;
  `crates/run/src/tests.rs::streamed_result_is_verified_and_redacted_across_chunks`;
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

A per-user LaunchAgent relays SSH requests into the login session; only a
refused or missing socket falls back to in-process handling, and a sent
request is never repeated. A peer writer takes its base from the shared
remote with its own Git authority and hooks off, refuses an unreachable base
and transports only committed, digest-checked, bounded bundles.
- Why: sign-in works over SSH without forwarding credentials, and no dirty
  tree crosses hosts.
- Test: `crates/fabric-runtime/src/peer_agent.rs::forward_falls_back_only_when_no_agent_accepts`;
  `crates/fabric-runtime/src/git_transport.rs::dirty_worktree_and_untrusted_remote_inputs_refuse_before_transfer`
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
contains bounded commit messages, run and attempt IDs, gate decisions and skipped nodes. The checkpoint
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
