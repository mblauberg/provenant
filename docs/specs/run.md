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
alphanumeric. Run scopes it by caller and project, and binds it to the
operation kind and a digest of the prompt, route, authority and execution
controls. Reader waits are excluded from the digest. The same ID and digest
return the recorded outcome without repeating effects. The same ID with a
different digest fails with `operation_conflict` (exit 2).
- Why: lost replies and concurrent duplicates, including resume, must never
  launch twice.
- Test: `crates/core/src/ids.rs::operation_keys_are_bounded_and_cannot_be_path_components`;
  `crates/run/src/service_tests.rs::dispatch_replay_output_and_cancel_share_the_service`
- Owner: `crates/core/src/ids.rs`, `crates/run/src/store.rs`

## Atomic admission

Admission and operation insertion are one transaction, and the immutable
request is published before acknowledgement. No launch, probe, wait or
supervisor call happens inside a transaction. Admission bounds active lanes
at 64 per host and 32 per project and queued lanes at 256 per host, with
memory and disk floors, startup spacing and a deadline.

Each ticket and host ledger lease records a lane class: `writer` for a
`worktree-write` lane, `read_only` for every other. Startup spacing is per
class and measured only against the last start of that class: writers wait 20 s
after the last writer start, and read-only lanes wait 2 s after the last
read-only start, so a burst of provider launches is still staggered but a
read-only lane never waits behind a writer. Spacing is measured on the host's
monotonic clock since boot (`CLOCK_MONOTONIC` on macOS, `CLOCK_BOOTTIME` on
Linux, both counting sleep), never the wall clock. Every process on the host
shares that clock, and the ledger records each class's last start as a reading
of it with the boot identity it was read under (`kern.bootsessionuuid` on macOS,
`/proc/sys/kernel/random/boot_id` on Linux). The ledger reads the clock inside
the transaction that checks and records spacing, never before a probe or the
lock, so lock order is clock order and an admission delayed behind another
process's newer start still waits for it. When the stored boot identity is the
current one, spacing is the clock now minus the start, and no wall-clock step,
forward or back, moves it. The guarantee is: within one boot, no two lanes of a
class start less than one spacing apart. A reboot resets spacing: a start from
another boot, or none, leaves the class free, apart from the one-time upgrade
below of a version 1 ledger that recorded a start, which holds each class for
one spacing. If the clock or the boot identity cannot be read the claim is refused with a reason (`startup spacing cannot be
judged: host clock or boot identity unreadable`), which the guardian publishes
as the lane's never-spawned reason, rather than judged on the wall clock;
`lanes` then names no spacing wait. The wall clock is kept only as the `started_at` a lane displays. The
memory floor (5 %) and disk floor (500 MiB) refuse both classes. A ledger row
or ticket with no class is a writer; opening a version 1 ledger upgrades it in
place, in one transaction, carrying any wall-time start across as a start of now under the current boot
for each class, whatever its age and without reading the wall clock, so the
one-time upgrade holds each class for one spacing (a version 1 ledger with no
start holds neither); it refuses the upgrade if the
clock or boot identity cannot be read. Spacing is enforced at
admission, the ledger commit, not at provider spawn, so a guardian suspended
between the two can spawn alongside a later lane. A run's own batch concurrency (1 to 16) defaults to
8 when any lane writes and to 16 when none does, from `lanes.max_concurrency`
and `lanes.max_concurrency_read_only`; an explicit value wins. A
`lanes.max_concurrency` the user set also caps the read-only default unless
they set the read-only knob too. Every entry point (CLI, Fabric and a peer)
accepts a value above the task count; it cannot admit more lanes than the run
has.

`lanes` shows each queued or starting lane's `lane_class` and `waiting_for`,
and uses the latter as its reason: launch not yet handed to a guardian, run
concurrency, the class's remaining startup spacing, or otherwise host
capacity. It reads the store and the host's ledger without locking, from a nested
lane too, and names only a wait it can see. `lanes --host` and the other peer
listings show the same two fields: the peer derives them from its own store and
ledger (peer action `lane_waits`) and an older peer that does not know the
action simply shows none.
- Why: an accepted receipt must mean durable admission, and read-only lanes
  must not queue behind writer spacing that exists to pace worktree and
  provider startup.
- Test: `crates/run/src/dispatch_tests.rs::admission_holds_run_concurrency_spacing_floors_and_deadline`;
  `::read_only_lane_is_admitted_inside_a_writers_spacing_window`;
  `::writers_stay_20_seconds_apart_and_read_only_starts_do_not_reset_the_clock`;
  `::read_only_lanes_are_spaced_2_seconds_apart`;
  `::memory_floor_refuses_both_lane_classes`;
  `::project_cap_allows_up_to_32_active_lanes`;
  `::legacy_rows_without_a_class_are_treated_as_writers`;
  `::batch_concurrency_defaults_by_lane_class_and_an_explicit_value_wins`;
  `::sixteen_read_only_lanes_run_in_one_batch_two_seconds_apart`;
  `::the_store_accepts_run_concurrency_above_the_task_count_up_to_sixteen`;
  `::a_nested_lane_reads_class_spacing_from_the_host_ledger_only_with_the_host_root`;
  `crates/cli/src/run.rs::a_nested_lane_reads_the_host_ledger_when_it_opens_read_only`;
  `crates/cli/src/run/remote.rs::a_peers_unstarted_lanes_show_their_class_and_wait_like_local_ones`;
  `tests/fabric_transport.rs::run_status_and_wait_read_the_owner_store_and_event_pull_deduplicates`;
  `crates/fabric-runtime/src/coordination/input.rs::concurrency_above_the_task_count_is_accepted_up_to_sixteen`;
  `::a_delayed_admission_cannot_start_inside_a_newer_start_or_rewind_it`;
  `::a_wall_clock_step_back_then_forward_cannot_admit_two_writers_within_a_spacing`;
  `::a_forward_wall_clock_step_alone_does_not_free_the_class`;
  `::read_only_spacing_holds_on_the_host_clock_and_never_holds_a_writer`;
  `::a_new_boot_frees_the_class_and_spacing_runs_on_the_new_boot`;
  `::an_unreadable_host_clock_or_boot_identity_refuses_the_claim`;
  `::a_start_ahead_of_the_host_clock_in_the_same_boot_never_frees_the_class`;
  `::lane_waits_count_spacing_on_the_host_clock_and_say_nothing_when_it_is_unreadable`;
  `::a_version_2_ledger_that_kept_wall_milliseconds_starts_every_class_free`;
  `::upgrading_a_version_1_ledger_is_atomic`;
  `::a_version_1_start_of_any_age_holds_each_class_one_full_spacing`;
  `::a_version_1_ledger_with_no_start_holds_nothing`;
  `::a_version_1_upgrade_refuses_when_the_host_clock_cannot_be_read`;
  `crates/supervisor/tests/custody.rs::the_guardian_names_an_unreadable_host_clock_in_its_admission_refusal`;
  `crates/run/src/holder.rs::host_clock_tests::an_unreadable_host_clock_reaches_the_guardian_as_itself`;
  `crates/os/src/sys/clock.rs::host_monotonic_clock_does_not_go_backwards_and_the_boot_identity_is_stable`;
  `::lane_waits_name_the_class_and_what_each_unstarted_lane_waits_for`;
  `crates/cli/src/run.rs::render_lanes_says_what_an_unstarted_lane_waits_for`;
  `crates/run/src/tests.rs::full_database_retains_admission_and_recovers_original_terminal_intent`
- Owner: `crates/run/src/admission.rs`, `crates/run/src/ledger.rs`, `crates/os/src/sys/clock.rs`

## Immutable attempts, no writer replay

Terminal attempts are immutable; a retry or continued turn is a new attempt
linked by `retry_of`, and the latest is the highest attempt number, never
the latest timestamp. Read-only fallback needs explicit opt-in and a typed
transient failure. A writer (an attempt that may edit files) is never replayed automatically, and two writers
never share a worktree.
- Why: evidence must not be rewritten, and replaying a writer clobbers edits.
- Test: `crates/run/src/dispatch_tests.rs::resume_continues_a_terminal_attempt_once_with_its_route`;
  `crates/fabric-runtime/src/graph.rs::writer_limit_after_launch_never_tries_a_second_route`
- Owner: `crates/run/src/store.rs`, `crates/run/src/launch.rs`

## Publication order

1. One transaction reserves the operation, digest, IDs and tickets.
2. The scheduler samples resources outside the transaction, takes writer custody and
   passes the plan to supervisor. A lost acknowledgement records
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
mailmap and fetching disabled, limited to 10 s per read and 1 MiB per file; a timeout keeps
partial output and is recorded. Reviewers must report inputs they could not
read and truncated captures.
- Why: repository config is hostile input, and a lane must not run on input
  the host could not verify.
- Test: `crates/run/src/dispatch_tests.rs::missing_stored_prompt_ends_the_attempt_rejected_with_the_cause`;
  `crates/run/src/git_context.rs::git_context_pins_hardening_flags_and_writes_prompt_paths`
- Owner: `crates/run/src/git_context.rs`

## Launch prompts

Launch prompts are bounded to 1 MiB. Argv adapters take up to 64 KiB; a
larger top-level prompt is sealed as `prompt.md` and replaced by a pointer to it, while
nested argv attempts refuse it because the parent lane can write their
directory. Prompt digests cover the full text in `prompt.txt`.
- Why: a spill file a lane can rewrite would change its child's task.
- Test: `crates/cli/src/run/provider.rs::launch_prompt_spills_only_top_level_argv_and_refuses_conflicting_file`;
  `::launch_prompt_refuses_symlinked_attempt_ancestors`
- Owner: `crates/cli/src/run/provider.rs`

## Read-only write exceptions

`projects/<ProjectId>/read-only.json` may grant read-only runs write
exceptions for existing directories, given relative to the verified repository
root.
Absolute paths, traversal, symlinks and Git or provider configuration are
refused, and the attempt records every granted path.
- Why: only the user widens a read-only lane.
- Test: `crates/confinement/src/plan/read_only.rs::policy_defaults_and_refuses_escape_control_paths_and_symlinks`;
  `crates/run/src/dispatch_tests.rs::read_only_exceptions_are_recorded_and_temporary_paths_are_private_per_attempt`
- Owner: `crates/confinement/src/plan/read_only.rs`

## Writer hard-link refusal

On macOS, launch refuses a writable tree containing an existing hard-linked
regular file and names the offending path. An operator can inspect it and
remove or rebuild it if it is disposable output before retrying.
- Why: a writable hard link could modify data outside the granted tree.
- Test: `crates/confinement/src/permissions.rs::writable_tree_hard_link_refusal_names_nested_file`
- Owner: `crates/confinement/src/permissions.rs`

## Reconcile and temporary cleanup

`reconcile` recovers queued and orphaned work idempotently and never replays
an uncertain launch. A start with no summary, held lock or recorded provider
start settles as rejected; a held lock stays unobservable. Terminal `tmp/`
removal runs only when no worktree reservation remains: reconcile checks
reservations before taking the attempt's writer lock, and an unreadable
reservation blocks removal. Deletion never follows links, checks identity first and
unlinks symlinks themselves. Lock files may be removed only below a terminal,
unreserved `tmp/`; owner locks such as `writer.lock`,
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
The store refuses unknown or newer versions without modifying them and
refuses unsafe companion files beside the database, and caps (1 MiB record or prompt, 64 MiB stream,
4 MiB result) make an oversized result a typed failure, never a truncation.
- Why: a corrupt or foreign store must never be repaired into evidence.
- Test: `crates/run/src/tests.rs::opens_refuse_unsafe_database_companions`;
  `::oversize_is_refused_before_publication_or_read`
- Owner: `crates/run/src/db.rs`

## Reads and waits

`output` offsets count original bytes and reads are lossless; prompt reads
need dispatch-equivalent authority. A terminal attempt with no requested
output is `NOT_FOUND`. `wait any` returns on the next unseen terminal or
`input_required` lane, and `wait all` returns when all are terminal or any needs
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

## Lane-owned worktrees and adoption

Dispatch may opt into local lifecycle ownership with `--worktree-branch BRANCH
[--base REF]`, exclusive with `--worktree` and peer `--branch`. Creation uses
confinement's existing registered-worktree path. Reuse requires the same branch,
a clean tree and free writer custody; an explicit base applies at creation.
The private Git admin directory holds `provenant-owner.json` as a bounded,
atomic `WorktreeOwner`: optional attempt and operation IDs, short branch,
pinned base OID, host, creation time, origin (`lane` or `adopted`) and optional
owner label. `load_owner` and `save_owner` require a verified linked worktree.

`worktree adopt PATH [--owner LABEL]` proves common-repository registration
and the Git backlink, acquires repository and writer locks, refuses live or
reserved writers and publishes the ordinary custody identity plus an adopted
owner. Ownership never grants deletion authority by itself. Existing explicit
`--worktree` dispatches keep their existing lifecycle.

Owned lane settlement may prune its own clean checkout and local branch only
when its exact recorded head is reachable from the pinned delivered ref or its
creation base. It rechecks ownership, terminal state, absent live lanes,
registration, writer custody, Git sequencer state and head under the existing
removal locks. Ignored output uses GC's existing declared-regenerable walker
and keep paths; undeclared ignored files retain the tree. Other worktrees keep
the existing GC/removal policy. A host lifecycle receipt names pruning or the
retention reason in `worktree-settlement.json` beside the attempt result.
- Why: a lane may reclaim only work it owns and commits proven retained elsewhere.
- Test: `crates/confinement/src/worktrees.rs::adopted_owner_proves_membership_and_enables_custody`;
  `::owned_cleanup_refuses_dirty_unmerged_and_adopted_trees`.
- Owner: `crates/confinement/src/worktrees/owner.rs`, `crates/cli/src/run.rs`, `src/fabric.rs`
