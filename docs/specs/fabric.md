# Fabric

Owner: `crates/fabric-store` (SQLite), `crates/fabric-runtime` (identity,
coordination, lane relay, peers, graphs, Git transport)
Anchor: `crates/fabric-store/src/tests.rs`, `crates/fabric-runtime/src/graph.rs`,
`crates/fabric-runtime/src/relay/lane_tests.rs`

Fabric is the daemonless coordination layer for one user's agents and hosts.
It grants no authority to install, land or run a provider, and keeps no second
run status ([run](run.md)). ADRs
[0020](../adr/0020-retire-the-daemon-fabric.md),
[0021](../adr/0021-configured-workspace-dispatch-boundaries.md),
[0026](../adr/0026-federate-fabric-across-own-hosts-over-ssh.md) and
[0039](../adr/0039-graph-of-templates.md) record the decisions; the
[peer host runbook](../runbooks/fabric-hosts.md) and `provenant help --all`
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
attempt-bound socket, where the host, not the request, fixes the sender.
`fabric mail send` uses that relay inside a lane; other CLI coordination
mutations refuse. A caller-supplied sender is rejected at the relay boundary.
- Why: a lane must not forge records that the host or another agent trusts.
- Test: `crates/providers/src/seatbelt_snapshots.rs::fabric_profiles_only_read_database_and_allow_only_the_attempt_socket`;
  `crates/fabric-runtime/src/relay/lane_tests.rs::lane_relay_binds_sender_replays_receipts_and_refreshes_read_only_wal`
- Owner: `crates/fabric-runtime/src/relay/host.rs`, `crates/fabric-store/src/db.rs`

### Private, versioned, atomic store

Coordination and the host custody ledger use private SQLite databases; symlinked or shared components and a
future schema are refused before any mutation, and config may lower limits but
never raise them. A mutation and its event commit together, and no transaction
stays open across waits, spawns, Git or SSH. A retried operation ID returns its
stored receipt; the same ID with a different digest conflicts.
- Why: a swapped database could forge fences, and agents retry after timeouts.
- Test: `crates/fabric-store/src/tests.rs::state_symlinks_and_non_private_components_are_refused`;
  `crates/fabric-store/src/tests.rs::event_failure_rolls_back_mail_mutations`;
  `crates/fabric-store/src/tests/operations.rs::concurrent_duplicate_operations_apply_once_and_share_the_committed_receipt`

### Custody and cursors

Fabric claims and graph chair identity use one host-owned custody ledger.
Subject kinds are declared once in `records::custody::SubjectKind`: graph,
attempt, branch, worktree, pr, issue, path and cloud key. Records retain holder
(chair or attempt nested under a chair), project, origin, active/landed/retired/
released state, last activity, related artefacts and dispatch provenance.
`LandedEvidence` names its predicate (subsumed or landed) and proof source;
`RetirementRecord` retains that evidence. These records are defined in records
for landing and retention to share; state alone never authorises deletion.

Claims do not expire, lock or refuse overlapping advisory ownership. Acquiring
another holder's item warns, records the previous holder in the same transaction
as the custody change, and proceeds. Gone holders leave unattended items for
adoption. Unknown native presence stays unknown. Run writer leases still own
Git isolation. Cursors advance monotonically; an unreachable peer is Unknown.
- Why: unattended work must remain discoverable without making advisory custody
  another admission boundary.
- Test: `crates/fabric-store/src/tests/f2.rs::custody_is_durable_advisory_and_queryable_by_declared_kinds`;
  `crates/fabric-store/src/tests/f2.rs::custody_event_failure_rolls_back_the_record`;
  `crates/fabric-store/src/tests/f2.rs::custody_queries_page_and_include_nested_lanes_without_crossing_chairs`
- Owner: `crates/records/src/custody.rs`, `crates/fabric-store/src/claims.rs`

### Chair bindings follow graph names

The host chair index reads graph subjects in the custody ledger, keyed by
project and graph name. A graph's first launch seals its dispatch provenance;
every later attempt resolves it. Direct host launch observes the chair when
its canonical request names none, without rewriting that request. An explicit
chair change warns and records the previous holder. Child graphs inherit their
parent's chair; an unknown parent stays unbound. Explicit adoption changes
advisory custody and is recorded. Graph names, rather than attempt or operation
identifiers, address this view.
- Why: every node in a graph resolves the same dispatching chair without a
  second identity mapping.
- Test: `crates/fabric-runtime/src/chairs.rs::tests::associations_are_keyed_by_project_and_graph_and_inherit_by_name`
- Owner: `crates/fabric-store/src/chairs.rs`, `crates/fabric-runtime/src/chairs.rs`

## Human requests

One service for the CLI owns requests and immutable answers, binding
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
  `crates/fabric-runtime/src/inbox/tests.rs::request_correlation_lookup_never_holds_fabric_write_lock`;
  `crates/fabric-runtime/src/inbox/tests.rs::inbox_fabric_mail_is_correlated_and_read_cursor_does_not_acknowledge`;
  `tests/fabric_transport.rs::remote_inbox_owner`
- Owner: `crates/fabric-runtime/src/inbox.rs`, `crates/fabric-store/src/human_inbox.rs`

## CLI and relay surfaces

Relay input is a strict tagged enum with no coercion, validated before side
effects, and the relay carries coordination only. A queued relay caller must
still be present with budget left before any effect, so a timed-out call cannot
commit. The CLI and the relay render one typed category per error. Settlement
reactions (graph advance, chair notice, chair log) run once in effect; notices
never instruct.
- Why: a lenient parser turns a typo into another request, and a lost lane must
  never commit after its deadline.
- Test: `crates/fabric-runtime/src/relay/host.rs::tests::disconnected_queued_caller_cannot_commit_after_admission`;
  `crates/fabric-runtime/src/relay/host.rs::tests::configured_frame_limits_reject_before_any_effect`;
  `crates/cli/src/fabric.rs::upgrade_and_peer_version_refusals_match_on_both_edges`
- Owner: `crates/fabric-runtime/src/relay/`, `crates/fabric-runtime/src/coordination/facade.rs`

## Bounded graphs and route fallback

The graph runtime admits a template, document or planner fragment only after
checking schema capability, structural limits, the fully expanded plan, the
checkpoint envelope and selected outputs. A launch retry without a wall clock
leaves the prior checkpoint.
- Why: a graph that starts must be able to finish within its bounds.
- Test: `crates/fabric-runtime/src/graph/graph_launch_retry_tests.rs::launch_retry_without_a_wall_clock_keeps_the_prior_checkpoint`;
  `crates/fabric-runtime/src/graph/plan_tests.rs::plan_validates_host_limits_before_saving`
- Owner: `crates/fabric-runtime/src/graph.rs`

### Verified live steering

The host verifies launch evidence and ownership before reading steering input.
Only the current live continuation leaf may receive control; authenticated
peer requests must also match the origin and receiver identities. Missing,
conflicting, terminal or unverifiable ownership is refused.

- Why: a lane must not direct another attempt.
- Test: `tests/graph_cli.rs::steer_graph_and_node_refuse_an_unlaunched_attempt_before_reading_stdin`

### Graph cancellation intent

`stop` first writes a bounded request under a short control lock that is separate
from the graph journal lock. The request is bound to the graph plan and instance,
and may name the graph or one node subtree. A launch owner checks requests at
graph and attempt binding boundaries; it saves the cancellation fence before
acknowledging and removing the request. A stop that finds the graph journal busy
returns `cancellation_requested` with its request ID and pending acknowledgement
state. Re-reading status does not enqueue another request. The owner preserves
the accepted attempt receipt before applying cancellation, and a resume cannot
clear a newly acknowledged stop in the same advance.
- Why: a launch may hold the journal lock while preparing a writer, but stop
  must remain durable and available during that work.
- Test: `crates/fabric-runtime/src/graph/cancellation_intent.rs::cancellation_intent_survives_graph_lock_and_is_acknowledged_after_save`

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

The inbox owns the question and answer; graph checkpoints link to it. Replay
must not create another request. A decision records the chosen option or the
declared default at its deadline. Answers require held authority and the exact
target; cancellation closes owned requests. A timed-out authority gate never
grants authority through silence.

- Why: the owner and chair need one authoritative answer.
- Test: `crates/fabric-runtime/src/graph/ask_tests.rs::decide_answer_becomes_the_verdict_and_routes_the_chosen_edge`

### Land nodes

Graph landing uses the [landing authority policy](cli.md#landing-adds-no-merge-authority). An inbox answer alone never approves a land.

- Why: a graph cannot confer authority through an inbox answer.
- Test: `crates/fabric-runtime/src/graph/land_tests.rs::an_inbox_answer_of_land_never_approves_a_land`

### Typed node outputs

Settled outputs are bounded and passed to dependants as untrusted data. Invalid
verdict evidence fails a consuming gate; full findings remain retrievable.

- Why: dependants need evidence without copying or trusting transcripts.
- Test: `crates/fabric-runtime/src/graph/graph_inputs_and_outputs_tests.rs::outputs_feed_direct_dependants_and_survive_replay`; `crates/fabric-runtime/src/graph/graph_review_contract_tests.rs::seven_long_blocking_findings_remain_retrievable_with_the_verdict`; `crates/fabric-runtime/src/graph/graph_delivery_and_routes_tests.rs::matched_required_contract_cannot_override_ambiguous_verdict_evidence`

### Checkpoint before launch

Transitions are pure reducers; the scheduler saves the checkpoint under a stable
attempt key before it launches, so replay returns the existing attempt. A
checkpoint missing launch evidence or its actor is refused.
- Why: a restart must never launch a node twice.
- Test: `crates/fabric-runtime/src/graph/graph_delivery_and_routes_tests.rs::checkpoint_attempts_require_launch_evidence_and_actor`;
  `crates/fabric-runtime/src/graph/transitions.rs::complete_attempt_transition_needs_no_owner_or_journal`

### Checkpoint envelope

Admission bounds the worst-case checkpoint before saving the plan.

- Why: an accepted graph must be able to save its checkpoint.
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
  `src/fabric/peer_graph_routing.rs::failed_settlement_does_not_advance_unrelated_graphs`

## Peer hosts

Each host owns its records and liveness; an unreachable peer is unknown, not
dead. An uncertain remote mutation is resolved by the owner's receipt, never
resent. Remote attempts bind the origin identity, and transferred Git objects
are verified before use. Protocol mismatch is refused before decoding;
credentials do not cross hosts.

- Why: a remote guess or resent mutation could free a live lane or apply twice.
- Test: `tests/fabric_transport.rs::peer_ingress_refuses_protocol_before_decoding_or_running_mutations`; `crates/fabric-runtime/src/hosts.rs::fixed_peer_transport_preserves_mutation_uncertainty_without_retry`; `crates/fabric-runtime/src/git_transport.rs::fetch_refuses_corrupt_missing_duplicate_and_malformed_objects`
