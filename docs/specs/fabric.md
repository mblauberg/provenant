# Fabric

Owner: `crates/fabric-store` (SQLite), `crates/fabric-runtime` (identity,
coordination, peers, graphs, Git transport), `crates/fabric-mcp` (stdio MCP)
Anchor: `crates/fabric-store/src/tests.rs`, `crates/fabric-runtime/src/graph.rs`,
`crates/fabric-mcp/src/stdio_tests.rs`

Fabric is the daemonless coordination layer for one user's agents and hosts.
It grants no authority to install, land or run a provider, and keeps no second
run status ([run](run.md)). ADRs
[0020](../adr/0020-retire-the-daemon-fabric.md),
[0021](../adr/0021-configured-workspace-dispatch-boundaries.md),
[0026](../adr/0026-federate-fabric-across-own-hosts-over-ssh.md) and
[0039](../adr/0039-graph-of-templates.md) record the decisions; the
[peer host runbook](../runbooks/fabric-hosts.md) and `provenant help --all`
and the MCP tool list
own use, flags and limits. Paths are relative to `tools/front-door/`.

## Identity and store

### Projects never merge by guess

`ProjectId` derives from the host and canonical Git common directory, so linked
worktrees share it; the cross-host key is home-relative and verified at each
peer. No scan or URL heuristic joins projects.
- Why: two checkouts must never share coordination state by accident.
- Test: `tests/fabric.rs::linked_worktrees_share_projects_and_ignore_git_redirects`;
  `crates/fabric-runtime/src/identity.rs::keys_are_home_relative_and_local_ids_are_separate`

### Lane store isolation

Lanes open the database read-only and no lane profile grants writes to it, its
companions or Fabric locks. A lane reaches the host only through its own
attempt-bound socket, where the host, not the request, fixes the sender; CLI
coordination mutations refuse inside lanes.
- Why: a lane must not forge records that the host or another agent trusts.
- Test: `crates/providers/src/seatbelt_snapshots.rs::fabric_profiles_only_read_database_and_allow_only_the_attempt_socket`;
  `crates/fabric-mcp/src/coordination_tests.rs::lane_relay_binds_sender_replays_receipts_and_refreshes_read_only_wal`
- Owner: `crates/fabric-mcp/src/lane.rs`, `crates/fabric-store/src/db.rs`

### Private, versioned, atomic store

The store is one private SQLite database; symlinked or shared components and a
future schema are refused before any mutation, and config may lower limits but
never raise them. A mutation and its event commit together, and no transaction
stays open across waits, spawns, Git or SSH. A retried operation ID returns its
stored receipt; the same ID with a different digest conflicts.
- Why: a swapped database could forge fences, and agents retry after timeouts.
- Test: `crates/fabric-store/src/tests.rs::state_symlinks_and_non_private_components_are_refused`;
  `::event_failure_rolls_back_mail_mutations`;
  `crates/fabric-store/src/tests/operations.rs::concurrent_duplicate_operations_apply_once_and_share_the_committed_receipt`

### Fences and cursors

Generations survive release and expiry, and claims use the home host's clock.
Path claims are advisory and never authorise writing. Cursors advance
monotonically, only for the caller, in the same transaction as deduplication;
an unreachable peer is `Unknown`, never complete.
- Why: a stale holder must never act, and a reader must never skip events.
- Test: `crates/fabric-store/src/tests/f2.rs::canonical_overlap_expiry_release_and_generation`

## Human requests

One service for CLI and MCP owns requests and immutable answers, binding
project, host and actor from identity, never request JSON. An answer commits
with its history, delivery intent and receipt in one transaction, and a
competing answer cannot replace the winner. Claimed work stays unverified until
host coverage verifies it. Only an explicit acknowledgement from the exact
target consumes an answer, and an answer never starts, resumes or approves
anything. Remote answers run on the owning host; an unavailable owner is
reported, never mirrored.
- Why: a timeout, a forged label or a competing UI must not produce a second
  authoritative answer or stand in for a workflow gate.
- Test: `tests/inbox.rs::inbox_cli_refuses_forged_ownership_and_closes_without_approval`;
  `crates/fabric-runtime/src/inbox/tests.rs::inbox_concurrent_answers_have_one_winner_and_one_mail`;
  `::request_correlation_lookup_never_holds_fabric_write_lock`;
  `::inbox_fabric_mail_is_correlated_and_read_cursor_does_not_acknowledge`;
  `tests/fabric_transport.rs::remote_inbox_owner`
- Owner: `crates/fabric-runtime/src/inbox.rs`, `crates/fabric-store/src/human_inbox.rs`

## CLI and MCP surfaces

Tool input is a strict tagged enum with no coercion, validated before side
effects. Output is lossless and paged within its byte budget, but committed
mutation receipts always survive. A full stdio queue is a typed busy error, and
cancellation suppresses the late reply without cancelling a launched run. CLI
and MCP render one typed category per error. Settlement reactions (graph
advance, chair notice, chair log) run once in effect; notices never instruct.
- Why: a lenient parser turns a typo into another request, and a lost client
  must never kill lanes.
- Test: `crates/fabric-mcp/src/schema_tests.rs::generated_schema_values_match_strict_serde_for_every_operation_and_field`;
  `crates/fabric-mcp/src/tools_tests.rs::result_limit_preserves_mutation_receipts_and_rejects_oversized_observations`;
  `crates/fabric-mcp/src/stdio_tests.rs::cancellation_suppresses_late_observations_without_cancelling_runs`;
  `crates/cli/src/fabric.rs::upgrade_and_peer_version_refusals_match_on_both_edges`
- Owner: `crates/fabric-mcp/src/`, `crates/fabric-runtime/src/coordination/facade.rs`

## Bounded graphs and route fallback

The graph runtime admits a template, document or planner fragment only after
checking schema capability, structural limits, the fully expanded plan, the
checkpoint envelope and selected outputs. A launch retry without a wall clock
leaves the prior checkpoint.
- Why: a graph that starts must be able to finish within its bounds.
- Test: `crates/fabric-runtime/src/graph/graph_launch_retry_tests.rs::launch_retry_without_a_wall_clock_keeps_the_prior_checkpoint`;
  `crates/fabric-runtime/src/graph/plan_tests.rs::plan_validates_host_limits_before_saving`
- Owner: `crates/fabric-runtime/src/graph.rs`

### Sources and read roots

Read roots grant reading only, limited to the project, its linked worktrees and
configured roots. A pinned `source` fixes a node's subject before its first
attempt and is materialised as an attempt-owned tree, so later edits cannot
change what a reviewer sees; a missing subject fails closed. A `head` source
pins the newest commit of a writer's continuation chain. The runtime never
resets or creates a worktree to satisfy a binding.
- Why: scouts share read access while review and repair keep one immutable subject.
- Test: `crates/fabric-runtime/src/graph/land_tests.rs::a_head_source_reviews_the_newest_continuation_and_lands_it`

### Writer continuation

A writer that `continues` an upstream `pr` or `push` writer reuses its worktree
and branch, so repairs update the same pull request. The host pushes without
force and refuses a mismatched branch.
- Why: a repair must land on the reviewed PR and never rewrite a moved branch.
- Test: `crates/fabric-runtime/src/graph/land_tests.rs::repair_reuses_the_writer_worktree_and_pushes_only_to_its_branch`;
  `crates/run/src/delivery_tests/prepare.rs::host_push_delivery_refuses_a_non_fast_forward_and_leaves_the_branch`

### Deterministic gates

Gates decide without a model or attempt and save the decision before dependants
run. CI gates query the exact pinned SHA; unavailable checks or policy fail
explicitly.
- Why: deterministic decisions need durable evidence without a launch.
- Test: `crates/fabric-runtime/src/graph/graph_plan_and_gates_tests.rs::gate_verdict_fixture_is_attempt_less_and_replays`;
  `crates/fabric-runtime/src/graph/ci_tests.rs::ci_outcomes_are_recorded_and_fail_closed`

### Bounded loops

Admission counts every possible round. A body writer integrates by
fast-forward only and never merges. Exhaustion pauses the graph without
delivery, and an extension never adds rounds twice.
- Why: repeated review and repair needs bounded admission and a human decision
  at the bound.
- Test: `crates/fabric-runtime/src/graph/loop_tests.rs::loop_fixture_exhausts_and_pauses_without_delivery`;
  `crates/run/src/delivery_tests/remote_delivery.rs::fast_forward_only_integration_advances_but_never_merges`

### Node asks and decide nodes

A node ask is one blocking request in the human inbox, which is the only store
of its question, options and answer. The checkpoint keeps just a link: the ask
ID, kind, node, session, deadline and the request ID. Filing is idempotent by an
operation ID derived from the graph instance, the ask and the session, so a
replay after a crash finds the same open request and never files a second. An
answer still needs the held authority, whether it arrives by chat or the CLI.
A decide node's chosen option, or its default at the deadline, becomes its
verdict; the link is dropped in the same save as the verdict. A cancelled graph
closes the requests it still owns.
- Why: the owner and the chair read one list, and two copies of a question could
  disagree.
- Test: `crates/fabric-runtime/src/graph/ask_tests.rs::decide_answer_becomes_the_verdict_and_routes_the_chosen_edge`;
  `crates/fabric-runtime/src/graph/ask_link_tests.rs`;
  `tests/inbox.rs::console_overview_and_the_cli_list_the_same_open_asks`

### Land nodes

A land node merges only a head that a passing independent review judged and CI
passed, through the host's own land path and tier policy. An inbox answer never
approves a land, because lanes and peers can write the inbox; a tier that asks
succeeds only once the chair merges at the pinned head.
- Why: routine work lands without a chair while crucial work waits for a person.
- Test: `crates/fabric-runtime/src/graph/land_tests.rs::an_inbox_answer_of_land_never_approves_a_land`;
  `::a_crucial_tier_asks_and_succeeds_only_once_the_chair_merges_by_hand`
- Owner: `crates/fabric-runtime/src/graph/land.rs`, `src/pr/land.rs`

### Typed node outputs

Settled nodes record bounded outputs: a digest of the full stored result, a
redacted excerpt, and optional verdict, findings, branch and commit. A verdict
is valid only as exactly one `VERDICT: TOKEN` line outside fenced blocks,
wherever it sits; a rejection names what was seen. Invalid evidence fails a
consuming gate; a matched `required_verdict` cannot override it. Selected
outputs reach dependants as delimited untrusted data that must fit the prompt
budget, or the node fails before launch.
- Why: dependants use bounded evidence without copying transcripts.
- Test: `crates/fabric-runtime/src/graph/graph_inputs_and_outputs_tests.rs::outputs_feed_direct_dependants_and_survive_replay`;
  `crates/fabric-runtime/src/graph/graph_delivery_and_routes_tests.rs::matched_required_contract_cannot_override_ambiguous_verdict_evidence`

### Checkpoint before launch

Transitions are pure reducers; the scheduler saves the checkpoint under a stable
attempt key before it launches, so replay returns the existing attempt. A
checkpoint missing launch evidence or its actor is refused.
- Why: a restart must never launch a node twice.
- Test: `crates/fabric-runtime/src/graph/graph_delivery_and_routes_tests.rs::checkpoint_attempts_require_launch_evidence_and_actor`;
  `crates/fabric-runtime/src/graph/transitions.rs::complete_attempt_transition_needs_no_owner_or_journal`

### Checkpoint envelope

A plan whose worst-case checkpoint could exceed 16 MiB is refused before its
first save, naming the size; every stored string is capped and growing lists
keep their newest entries.
- Why: an accepted graph must never stop on a checkpoint too large to save.
- Test: `crates/fabric-runtime/src/graph/envelope_tests.rs::worst_case_checkpoint_fits_its_bound`

### Fallback only for unavailability

A node moves to its next route only on a typed pre-launch authentication, quota
or adapter refusal with custody released, or a classified refusal of a launched
read-only attempt within its [retry](retry-owner.md) budget. Launched writers
and uncertain launches never switch.
- Why: a launched writer may already have had effects.
- Test: `crates/fabric-runtime/src/graph/graph_delivery_and_routes_tests.rs::fallback_outcomes_are_named_by_access_and_launch_state`

### Graph delivery

Graph delivery opens at most one final PR, only after every active path
succeeds and custody is released; settlement advances only the matching graph.
- Why: a partial graph must never publish, and settlement must not scan unrelated graphs.
- Test: `crates/fabric-runtime/src/graph/graph_delivery_and_routes_tests.rs::graph_delivers_nodes_and_opens_one_final_pr_only_after_success`;
  `src/fabric/peer_graph_routing.rs::settlement_without_hint_does_not_advance_unrelated_graphs`

## Peer hosts

Each host owns its records and liveness, so an unreachable peer is `Unknown`,
never dead. Peers use system SSH and the fixed `provenant fabric peer` command,
which refuses another protocol before decoding. An uncertain remote mutation is
resolved only by the home host's receipt, never resent, and code crosses hosts
only as committed objects Git verifies. Credentials, keys and keychains never cross hosts (no test:
peer request types carry no credential fields), and peer changes need a
chair-run smoke between two real hosts (review gate).
- Why: remote guesses or a resent mutation could free a live lane or apply twice.
- Test: `tests/fabric_transport.rs::run_status_and_wait_read_the_owner_store_and_event_pull_deduplicates`;
  `::peer_ingress_refuses_protocol_before_decoding_or_running_mutations`;
  `crates/fabric-runtime/src/hosts.rs::fixed_peer_transport_preserves_mutation_uncertainty_without_retry`;
  `crates/fabric-runtime/src/git_transport.rs::fetch_refuses_corrupt_missing_duplicate_and_malformed_objects`
- Owner: `crates/fabric-runtime/src/hosts.rs`, `crates/fabric-runtime/src/git_transport.rs`

## Project-local scratch

Scratch lives in `.provenant/scratch/`, excluded through Git's common metadata
and never tracked; symlinked roots refuse.
- Why: graph documents need a project-local home without tracked changes.
- Test: `crates/confinement/src/scratch.rs::scratch_is_local_excluded_idempotent_and_refuses_links`
- Owner: `crates/confinement/src/scratch.rs`
