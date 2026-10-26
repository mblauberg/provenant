# Execution: runs

Owner: `crates/run` (admission, attempts, publication, reconcile),
`crates/records` (shared records and `AttemptFact`)
Anchor: `crates/run/src/tests.rs`, `crates/run/src/dispatch_tests.rs`,
`crates/run/src/delivery_tests.rs`

Run turns a request into bounded provider attempts and keeps the exact
prompt, route and result of each, so completion and cancellation survive a
disconnected caller. Routing resolves models ([routing](routing.md));
supervisor owns every process and signal ([supervisor](supervisor.md)); Fabric
keeps no second registry ([fabric](fabric.md)). Run starts no process
itself. Grammar and bounds are in the generated [CLI
reference](../reference/cli.md). Paths are relative to `tools/front-door/`.

## Operation IDs

An operation ID is 1 to 128 ASCII letters, digits, `.`, `_` or `-`, starting
alphanumeric. Run scopes it by caller and project and binds it to the
operation kind and a digest of prompt, route, authority and execution
controls, not reader waits. The same ID and digest returns the recorded
outcome without repeating effects; the same ID with a different digest is `operation_conflict`, exit 2.
- Why: lost replies and concurrent duplicates, including resume, must never
  launch twice.
- Test: `crates/core/src/ids.rs::operation_keys_are_bounded_and_cannot_be_path_components`;
  `crates/run/src/service_tests.rs::dispatch_replay_output_and_cancel_share_the_service`
- Owner: `crates/core/src/ids.rs`, `crates/run/src/store.rs`

## Atomic admission

Admission and operation insertion are one transaction, and the immutable
request is published before acknowledgement. No launch, probe, wait or
supervisor call happens inside a transaction. Admission bounds active lanes
at 64 per host and 16 per project and queued lanes at 256 per host, with
memory and disk floors, startup spacing and a deadline.
- Why: an accepted receipt must mean durable admission.
- Test: `crates/run/src/dispatch_tests.rs::admission_holds_run_concurrency_spacing_floors_and_deadline`;
  `crates/run/src/tests.rs::full_database_retains_admission_and_recovers_original_terminal_intent`
- Owner: `crates/run/src/admission.rs`

## Immutable attempts, no writer replay

Terminal attempts are immutable; a retry or continued turn is a new attempt
linked by `retry_of`, and the latest is the highest attempt number, never
the latest timestamp. Read-only fallback needs explicit opt-in and a typed
transient failure. A writer is never replayed automatically, and two writers
never share a worktree.
- Why: evidence must not be rewritten, and replaying a writer clobbers edits.
- Test: `crates/run/src/dispatch_tests.rs::resume_continues_a_terminal_attempt_once_with_its_route`;
  `crates/fabric-runtime/src/graph.rs::writer_limit_after_launch_never_tries_a_second_route`
- Owner: `crates/run/src/store.rs`, `crates/run/src/launch.rs`

## Publication order

1. One transaction reserves the operation, digest, IDs and tickets.
2. The scheduler samples resources outside it, takes writer custody and
   hands supervisor the plan. A lost acknowledgement records
   `launch_unknown`, keeps capacity and the reservation, and is never
   replayed.
3. Result and exit evidence are sealed first; then one commit writes the
   terminal attempt, summary, operation state, released capacity and event.
4. Recovery retries a pending commit without replacing a committed outcome.
5. A run closes only when every task and registered child is terminal.

If publication keeps failing, the attempt stays `settling` with
`publication_pending: true` and keeps custody; run never reports success or
frees a writer early to hide a bookkeeping failure.
- Why: each step is the evidence the next recovers from.
- Test: `crates/run/src/tests.rs::publication_crashes_preserve_committed_records_and_allow_retry`;
  `::prelaunch_refusal_publishes_durable_never_launched_evidence`.
  No run-level test asserts that `launch_unknown` is never replayed (gap).
- Owner: `crates/run/src/launch.rs`, `crates/run/src/store.rs`

## Success needs proof

Success needs a supervisor-observed exit 0, a matching provider identity and
a complete, validated, non-empty result; capture bytes are never completion
evidence. A dead owner is not a failed provider. A completed writer whose
protected changes need review ends `review_required`, and one with only
uncommitted edits ends `uncommitted`; neither delivers. Run reads no provider
transcript except Codex's post-exit identity projection.
- Why: a crash, truncation or impostor must never read as success.
- Test: `crates/run/src/dispatch_tests.rs::holder_commits_decoded_success_only_with_exit_zero_and_observed_identity`;
  `crates/run/src/tests.rs::protected_review_retains_candidate_and_paths_without_delivery`
- Owner: `crates/run/src/holder.rs`, `crates/run/src/writer.rs`

## Host inputs fail before launch

A host failure to read the stored prompt, or to stage or read back a context
file, ends the attempt `rejected` before launch with its cause. `--base`
context is captured on the host with fsmonitor, external diff, textconv,
mailmap and fetching off, 10 s per read and 1 MiB per file; a timeout keeps
partial output and is recorded. Reviewers must report inputs they could not
read and truncated captures.
- Why: repository config is hostile input, and a lane must not run on input
  the host could not verify.
- Test: `crates/run/src/dispatch_tests.rs::missing_stored_prompt_ends_the_attempt_rejected_with_the_cause`;
  `crates/run/src/git_context.rs::git_context_pins_hardening_flags_and_writes_prompt_paths`
- Owner: `crates/run/src/git_context.rs`

## Launch prompts

Launch prompts are bounded to 1 MiB. Argv adapters take up to 64 KiB; a
larger top-level prompt is sealed as `prompt.md` behind a pointer, while
nested argv attempts refuse it because the parent lane can write their
directory. Prompt digests cover the full text in `prompt.txt`.
- Why: a spill file a lane can rewrite would change its child's task.
- Test: `crates/cli/src/run/provider.rs::launch_prompt_spills_only_top_level_argv_and_refuses_conflicting_file`;
  `::launch_prompt_refuses_symlinked_attempt_ancestors`
- Owner: `crates/cli/src/run/provider.rs`

## Read-only write exceptions

`projects/<ProjectId>/read-only.json` may grant read-only runs write
exceptions: existing directories relative to the verified repository root.
Absolute paths, traversal, symlinks and Git or provider configuration are
refused, and the attempt records every granted path.
- Why: only the user widens a read-only lane.
- Test: `crates/confinement/src/plan/read_only.rs::policy_defaults_and_refuses_escape_control_paths_and_symlinks`;
  `crates/run/src/dispatch_tests.rs::read_only_exceptions_are_recorded_and_temporary_paths_are_private_per_attempt`
- Owner: `crates/confinement/src/plan/read_only.rs`

## Reconcile and temporary cleanup

`reconcile` recovers queued and orphaned work idempotently and never replays
an uncertain launch. A start with no summary, held lock or recorded provider
start settles as rejected; a held lock stays unobservable. Terminal `tmp/`
removal runs only when no worktree reservation remains: reconcile checks
reservations before taking the attempt's writer lock, and an unreadable
reservation blocks removal. Deletion is no-follow and identity-checked, and
symlinks are unlinked, never followed. Only below a terminal, unreserved
`tmp/` are lock names disposable; owner locks such as `writer.lock`,
`publication.lock` and the run store lock keep their inodes.
- Why: unlinking an owner lock lets two owners lock different inodes, and
  deleting under a reservation destroys a writer's work.
- Test: `crates/run/src/dispatch_tests.rs::reconcile_preserves_protected_locks_outside_temporary`;
  `::reconcile_leaves_temporary_data_behind_a_writer_reservation`;
  `::reconcile_unreadable_reservation_only_retains_existing_temporary_data`;
  `::reconcile_temporary_removal_never_follows_symlinks`
- Owner: `crates/run/src/controls.rs`

## Store

The run store is SQLite with write-ahead logging (WAL), foreign keys and `synchronous=FULL`.
Unknown or newer versions fail closed without modification, unsafe
companions are refused, and caps (1 MiB record or prompt, 64 MiB stream,
4 MiB result) make an oversized result a typed failure, never a truncation.
- Why: a corrupt or foreign store must never be repaired into evidence.
- Test: `crates/run/src/tests.rs::opens_refuse_unsafe_database_companions`;
  `::oversize_is_refused_before_publication_or_read`
- Owner: `crates/run/src/db.rs`

## Reads and waits

`output` offsets count original bytes and reads are lossless; prompt reads
need dispatch-equivalent authority. A terminal attempt with no requested
output is `NOT_FOUND`. `wait any` returns on the next unseen terminal or
`input_required` lane, and `wait all` when all are terminal or any needs
input. The set is fixed at entry and excludes the caller and its ancestors.
- Why: attention must never wait behind a running sibling.
- Test: `crates/run/src/tests.rs::create_publish_and_read_exact_bounded_facts`;
  `crates/fabric-runtime/src/runs.rs::input_required_wakes_all_without_waiting_for_a_sibling`
- Owner: `crates/run/src/reader.rs`

## Nested submission

Nested submission uses a dedicated store under the submitting attempt,
selected by `PROVENANT_RUN_STATE_DIR`; identity and config stay on the
original root. The host launches only accepted operations, under the rules
in [nested lanes, MCP and hooks](routing.md#nested-lanes-mcp-and-hooks).
- Why: a lane must not reach the parent's database or other runs.
- Test: `src/worktree_tests.rs::lane_prepare_isolates_nested_state`
- Owner: `crates/run/src/plan.rs`

## Not provided

- Watchdog silence as an outcome, `fallback:any` or silent effort downgrade.
- Alias coercion, old-root scans, record migration or a state deletion
  command.
