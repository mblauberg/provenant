# Command surface

Owner: `crates/cli` (grammar and rendering), `src/` (composition and hooks)
Anchor: `tests/parser.rs`, `crates/cli/src/render.rs`, `tests/catalogue_read_only.rs`

Use `provenant help --all` for commands and `provenant config schema` for configuration.
The outward contract is declared in `crates/cli/src/external_contract.rs`, covering commands, lane environment and JSON schemas.
`scripts/generate-reference` renders `external-contract.md` from that declaration through `provenant-cli print-reference-data`; the same data derives threshold behaviour from the policy table.
No command currently promises cross-version stability, and direct internal-state readers are unsupported.
Conditional PR wait observations report `review=unknown` because the REST response has no GraphQL review decision; CI settlement still determines the wait verdict. One-shot `pr status` keeps its detailed review observation.
Repository CI reads its tier declaration from `config/ports.json`; the optional `.provenant/ports.json` entry links that same table, so running repository checks requires neither an installed Provenant nor a `.provenant` directory.

## Targets

Target verbs share one grammar and resolve only the named graph. Ambiguity is
reported, never resolved by guessing; conflicting manifest settings are refused.

- Why: an action must address the intended work.
- Test: `crates/cli/src/lib.rs::every_target_verb_parses_the_generated_target_table_alike`; `crates/cli/src/target.rs::resolution_reads_only_the_named_graph`

## Parsing

Help and version inspection require no project state. Paths retain their native
bytes, and invalid or conflicting arguments are rejected.
The catalogue derives flag relationships and child commands from the parser, and declares each command's configuration domains.

- Why: inspection must be safe outside a checkout and paths must not change in transit.
- Test: `tests/parser.rs::help_and_version_need_no_roots_and_catalogue_help_drives_every_leaf`; `crates/cli/src/lib.rs::byte_paths_survive_clap_without_text_conversion`

## Console catalogue and peer coverage

Console reads do not allocate product state. Peer observations retain local
results and identify incomplete coverage rather than presenting it as complete.
At most eight peer reads run concurrently under one shared deadline, retaining configured host order.

- Why: a partial observation must not hide available work or imply complete coverage.
- Test: `crates/cli/src/console/peer.rs::peer_board_coverage`; `tests/catalogue_read_only.rs::catalogue_read_only`

### Nested tree

The tree distinguishes dispatch, continuation and retry relationships, and
reports missing parents, gaps and incomplete usage as observations, not facts
about unseen work.

- Why: nested work must remain visible without inventing edges or totals.
- Test: `crates/cli/src/console/tree.rs::host_tree_preserves_lineage_detail_and_usage`

### Board snapshot and cursor contract

A board cursor binds the selection and a high-water ceiling; replay reads
current facts within that ceiling. Damaged ancillary observations cannot hide
valid indexed rows, and missing usage remains distinct from zero.

- Why: an active board must be pageable without inventing observations.
- Test: `crates/cli/src/console/board.rs::board_paging_end_to_end`; `crates/run/src/board.rs::board_paging_snapshot_and_metadata`

## Results

### One bounded envelope, never truncated

A result is whole or refused before output. Paged reads bind the cursor to the
input, and terminal text escapes control sequences. Unknown facts stay unknown.
Compact output uses semantic owner summaries and a detail command; JSON preserves the complete envelope.

- Why: consumers must not mistake partial or forged output for a complete result.
- Test: `crates/cli/src/render.rs::oversize_is_rejected_before_any_stdout_and_lf_counts`; `crates/cli/src/render.rs::terse_quotes_and_escapes_anything_that_could_mislead_a_terminal`

### Error codes

### Each refusal has a stable code

A refusal carries a stable typed code independent of message wording. The
command catalogue owns the list of codes and their meanings.

- Why: consumers can act on a refusal without depending on prose.
- Test: `crates/run/src/service_tests.rs::each_refusal_has_its_own_stable_code`; `crates/cli/src/lib.rs::help_all_lists_each_error_code_once_with_its_meaning`

### Waits are readers

Waiting observes work; timeout or cancellation does not stop it. A target
settles by its own state, so an older settlement cannot satisfy a wait for new
work. Local graph waiters share one filesystem event source (inotify or kqueue)
for settlement logs. When that source is unavailable, the result reports why
and rereads at `domains.wait.local_interval_ms`. Peer or relayed rows also use
paced rereads at that setting because the relay socket has no remote event
subscription; the result identifies that source. Pull requests use forge
observation. Settlement events are emitted as they are observed.
With `--json`, wait emits JSONL events followed by a terminal command-result envelope, including without `--follow`.

- Why: a reader must neither terminate healthy work nor wake on stale evidence.
- Test: `src/status_wait.rs::a_target_settles_by_its_own_row`; `src/status_wait.rs::any_returns_at_the_first_settlement_and_takes_its_verdict`; `crates/supervisor/tests/custody.rs::cancelled_reader_preserves_healthy_custody_and_cancel_escalates`

### Status selection

Status includes all active graph work; omitted settled work is counted. Human
and structured output share the selection. `status --mine` reads active custody
across projects, or one explicit `--project`, including attempts nested under
the current chair and unattended items available for adoption. It is short by
default, expands with `--full`, and exposes typed records and native presence
with `--json`. Pagination uses `--limit` and `--cursor`; `--session` selects the
exact chair session for the session-start digest. Settled custody leaves the
active view. The digest reads this view instead of extracting PRs from prose.

`adopt KIND:ID` takes custody of an external item; `release KIND:ID` records an
explicit release. `fabric claim acquire|renew|verify|release` takes the same
subject spelling; renew records activity without a lease or secret token.
Run launch, worktree creation, PR binding/landing and cloud launch acquire
custody automatically. Attempt/graph settlement releases those subjects; landing
records typed evidence on PR and associated branch/worktree subjects. Custody
failures after a successful external action warn and can be retried by adoption.
Transfer to a named session is deferred.

- Why: a chair acting on a listing must see work still running.
- Test: `src/fabric/unified.rs::the_listing_never_hides_active_work_and_counts_what_it_omits`

### Output references and bounded tails

Output and steering resolve the same numbered attempt. Output reads are bounded
and a clamped tail reports its effective bound.
By default, `output` shows result text whole up to 12 KiB, then marked head and tail; `--full` shows complete text.

- Why: a reader must be able to inspect the intended attempt safely.
- Test: `crates/cli/src/target.rs::steer_and_output_reach_the_same_numbered_attempt`; `crates/cli/src/run/tests_access.rs::paged_output_never_splits_a_character`

### Settled attempt provenance receipts

A receipt identifies a settled local attempt and reports only supported route
and reviewer attribution. Ambiguous, running or peer-owned targets are refused;
missing evidence stays unknown.

- Why: provenance must not mistake a writer for a reviewer or infer a route.
- Test: `tests/graph_provider.rs::receipt_output_resolves_the_work_key`; `tests/graph_provider.rs::receipt_output_classifies_writers_and_refuses_running_attempts`; `tests/graph_provider.rs::receipt_output_reports_unknown_route_for_settled_lost_attempt`

## Guards

### Merge guard

Landing verifies and merges the same head, and refuses changed heads or
incomplete required check evidence.

- Why: a merge must land the head that CI verified.
- Test: `tests/pr.rs::land_refuses_head_changes_before_and_after_checks`; `tests/pr.rs::land_refuses_unpassed_missing_and_malformed_checks`

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

The `command` strategy invokes `land.command.argv` in the repository checkout,
with the full pending intent as JSON on stdin. Its required declaration includes
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
- Test: `src/pr_land_tests.rs::pr_land_merges_then_fetches_fast_forwards_and_prunes`; `crates/confinement/src/worktrees/lifecycle_tests/worktree_operations.rs::named_removal_refuses_a_head_that_moved_after_the_callers_check`

### Pull request bodies come from receipts

Pull request creation derives its claims from the settled attempt and delivery
receipt, and states absent evidence rather than inventing it.

- Why: a hand-built body can claim a head or route the receipts do not support.
- Test: `crates/cli/src/pr_body.rs::the_repository_template_is_filled_from_the_receipts`; `src/pr_land_tests.rs::pr_open_refuses_what_the_receipts_cannot_support`

### Shell commands use confinement

The hook validates its event envelope but does not decide authority from shell
syntax. Writer isolation and authority are enforced at the boundaries described
in [supervision](supervisor.md#permission-policy).

- Why: shell syntax rules cannot enforce those boundaries.
- Test: `tests/hooks.rs::shell_commands_are_left_to_confinement_even_with_unreadable_config`

### Pre-push refuses data loss

The pre-push guard protects shared default-branch history and reports advisory
conflict markers.

- Why: shared history must not be deleted or rewritten accidentally.
- Test: `tests/hooks.rs::pre_push_refuses_non_fast_forward_to_remote_default_alias`; `tests/hooks.rs::pre_push_uses_real_git_to_refuse_data_loss_and_conflict_markers`

### Worktree hooks respect ownership

Worktree hooks use the worktree owner and cannot remove a worktree they did not
create or override the owner's refusal.

- Why: a hook must not widen removal authority.
- Test: `tests/hooks_session.rs::worktree_hooks_create_and_remove_through_the_worktree_owner`; `tests/hooks_session.rs::worktree_remove_keeps_a_listed_clean_tree_the_hook_did_not_make`

### Checks report, never repair

Checks report findings and unavailable evidence without repairing the subject.
Release checks inspect the artifact to be distributed as well as Git evidence.

- Why: an inspection must not change its subject or certify an uninspected artifact.
- Test: `tests/checks.rs::release_checks_the_running_binary_as_an_artifact`; `tests/graph_provider.rs::provider_checks_preserve_routes_and_settlement_causes`
