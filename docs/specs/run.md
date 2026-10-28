# Execution: runs

Owner: `crates/run` (admission, attempts, publication, reconcile),
`crates/records` (shared records and `AttemptFact`)
Anchor: `crates/run/src/tests.rs`, `crates/run/src/dispatch_tests.rs`,
`crates/run/src/delivery_tests.rs`

Run turns a request into bounded provider attempts and keeps the exact
prompt, route and result of each, so completion and cancellation survive a
disconnected caller. Routing resolves models ([routing](routing.md));
supervisor owns every process and signal ([supervisor](supervisor.md)); Fabric
stores chair associations, with execution facts owned by Run ([fabric](fabric.md)). Run starts no process
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

## Task titles

Run and dispatch accept an optional human-supplied title, including per-task
titles in manifests and Fabric submissions. The title is sealed in the request
and copied to each attempt; it participates in operation replay identity.
Titles are nonempty, contain no control characters and are limited to 256 UTF-8
bytes. `lanes` displays titles when present and includes them in JSON output;
absent titles remain absent in older records. Titles are never inferred from
prompts.
- Test: `crates/run/src/service_tests.rs::task_title_roundtrip`
- Owner: `crates/run/src/service.rs`, `crates/records/src/run.rs`

## Chair associations

Local CLI and MCP submissions seal a chair's host and Fabric inbox actor with
the request. CLI selection matches session discovery; MCP uses its bound inbox.
This actor-scoped association identifies no particular live chair session and
does not change run ownership. Worker provider sessions never establish it.

The Fabric `run_chairs` index in `<state>/fabric/host.sqlite3` is authoritative,
keyed by project and run. This database and its companions are outside the exact
coordination database write grants lanes receive. It seals the request digest
and optional chair before launch. Each nested launch inherits from the host
index using the parent's host-sealed request
digest. A disagreeing child proposal is refused. Lane-local requests and even a
lane-local Fabric database or the shared mail database cannot replace this
association. A retry preserves the original proposal; continuations copy the
original host index binding.
Index failures or exposing write grants refuse launch. Replay a root submission with the
same operation ID: launch repairs its index from the immutable host request,
including owned-worktree replay. Lane stores cannot repair it.
Transient index errors leave queued children available for a later scan.

Launch validation reads `fabric-runtime::chairs::ChairIndex`; missing records
and older runs are unbound. Display readers arrive in a later slice. Provider handles and child requests are never fallbacks. Graph and
peer executions remain unbound until they retain an originating chair separately
from the actor owning operation replay. Board and tree rendering are separate.

Association rows live as long as run metadata. Current GC removes provider
payloads, logs, events and worktrees; it never removes run/attempt metadata.
Whole-run collection must delete its association in the same host-owned removal
workflow. Lane-writable missing child records alone cannot authorise deletion.

- Test: `crates/fabric-runtime/src/chairs.rs::nested_chair_binding`;
  `crates/run/src/service_tests.rs::chair_binding_replays_the_original_submission`
- Owner: `crates/fabric-runtime/src/chairs.rs`, `crates/fabric-store/src/chairs.rs`,
  `crates/cli/src/run.rs`, `crates/cli/src/run/children.rs`

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

## Lane priority

The host selects a configured nice value for each lane: 10 for writers, 5 for
read-only lanes and 15 for build-heavy lanes by default. The guardian applies
it to the provider before confinement; the provider's descendants inherit it.
An inherited higher nice value is preserved. Failure to read or change priority
must never refuse a lane: launch proceeds and the reason is recorded. Launch
evidence reports the applied value when known and the requested value when it
differs.
- Why: lanes should yield CPU without requiring new privileges or preventing
  nested work from starting.
- Test: `crates/supervisor/src/process_os/provider.rs::provider_and_descendant_inherit_priority`;
  `::priority_failures_degrade_and_preserve_evidence`;
  `crates/run/src/tests.rs::lanes_read_last_output_time_from_supervisor_checkpoint`;
  `crates/cli/src/run.rs::render_lanes_removes_peer_terminal_controls`
- Owner: `crates/supervisor/src/process_os/provider.rs`, `crates/run/src/launch.rs`

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

Read-only runs with a verified Git binding prepare `scratch/git-objects` with
the bound common directory's `objects` as an alternate. The shell Git wrapper
sets `GIT_OBJECT_DIRECTORY` for `show`, `log`, `diff`, `merge-tree` and
`hash-object` only when Git resolves that common directory, including linked
worktrees. Aliases and external subcommands receive no override. The launch
environment has no lane-wide override, so other repositories and submodules
retain their own object stores. The wrapper honours recognised Git global
options; unknown options and direct
absolute-path Git calls run without this temporary-object convenience.
Configured callbacks of those builtins inherit their repository-local Git
environment; callbacks entering another repository must clear Git-local
variables.
The shared Git directories remain write-denied, and symlink creation in the
scratch object store is denied. No quarantine write exception is granted.
The lane can alter its scratch objects and alternates; lane Git output must
never serve as trusted host evidence. The host uses its own Git environment.
- Why: `show --remerge-diff` needs temporary objects, while #1532 requires the
  shared store to remain unwritable. Repository scoping preserves other reads.
- Test: `crates/providers/src/tests.rs::prepared_git_objects_are_repository_scoped_for_every_adapter`;
  `::prepared_git_reads_linked_and_other_repositories`;
  `crates/providers/src/seatbelt_snapshots.rs::agy_read_only_linked_worktree_git_directories_are_not_writable`
- Owner: `crates/providers/src/launch.rs`, `crates/confinement/src/permissions.rs`

## Project GitHub policy

`projects/<ProjectId>/github.json` holds an optional host-owned advisory deny
list of GitHub CLI verb prefixes and labels for writer lanes. Missing policy
defaults to empty. When publishing is available, the lane's `gh` shim refuses
matching calls with the policy key on standard error. This is an accidental-use
guard, not a security boundary: a lane can invoke the real `gh` directly.

- Why: projects can reserve GitHub actions for people or workflows without
  relying on instructions in a lane's prompt.
- Test: `crates/config/src/tests.rs::github_policy_is_scoped_to_project_and_defaults_only_when_absent`;
  `crates/providers/src/lane.rs::github_policy_guards_labels_and_verb_prefixes`
- Owner: `crates/config/src/github_policy.rs`; `crates/providers/src/lane.rs`

## Writer hard-link refusal

On macOS, launch refuses a writable worktree containing an existing hard-linked
regular file and names the offending path. Directories Git ignores wholesale,
such as an in-tree Cargo `target/` or `node_modules/`, are skipped by the
census and its entry bound. An operator can inspect a refused file and remove or
rebuild it if it is disposable output before retrying.
- Why: a writable hard link could modify data outside the granted tree.
- Test: `crates/confinement/src/permissions.rs::writable_tree_hard_link_refusal_names_nested_file`;
  `::the_census_neither_walks_nor_counts_skipped_directories`;
  `crates/confinement/src/ignored.rs::wholly_ignored_directories_are_listed_and_others_are_not`
- Owner: `crates/confinement/src/permissions.rs`, `crates/confinement/src/ignored.rs`

## Reconcile and temporary cleanup

`reconcile` recovers queued and orphaned work idempotently and never replays
an uncertain launch. If a writer guardian commits a terminal record and releases
its reservation while recovery is preparing, recovery re-reads the canonical
attempt and accepts absence only for that same attempt and generation. A
missing reservation on a nonterminal attempt or unreadable retained custody
still reports uncertainty. A start with no summary, held lock or recorded provider
start settles as rejected; a held lock stays unobservable. Terminal `tmp/`
removal runs only when no worktree reservation remains: reconcile checks
reservations before taking the attempt's writer lock, and an unreadable
reservation blocks removal. Deletion never follows links, checks identity first and
unlinks symlinks themselves. Lock files may be removed only below a terminal,
unreserved `tmp/`; owner locks such as `writer.lock`,
`publication.lock` and the run store lock keep their inodes.
- Why: unlinking an owner lock lets two owners lock different inodes, and
  deleting under a reservation destroys a writer's work.
- Test: `crates/run/src/holder.rs::writer_recovery_accepts_only_matching_terminal_absence`;
  `crates/run/src/dispatch_tests.rs::reconcile_preserves_protected_locks_outside_temporary`;
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
Reuse preserves the existing lane owner's creation base and refuses adopted
owners without changing their records. Operation replay
returns its original run, including after owned cleanup, and checks changed
inputs before any worktree creation.
The private Git admin directory holds `provenant-owner.json` as a bounded,
atomic `WorktreeOwner`: optional attempt and operation IDs, short branch,
pinned base OID, host, creation time, origin (`lane` or `adopted`) and optional
owner label. `load_owner` and `save_owner` require a verified linked worktree.

`worktree adopt PATH [--owner LABEL]` proves common-repository registration
and the Git backlink, acquires repository and writer locks, refuses live or
reserved writers and publishes the ordinary custody identity plus an adopted
owner. Ownership never grants deletion authority by itself. Existing explicit
`--worktree` dispatches keep their existing lifecycle.

Owned lane settlement may prune its own clean checkout only after success
without requested delivery, or after successful delivery, and when its exact
recorded head is reachable from the pinned delivered ref or its creation base.
Local branch deletion additionally requires proof of landed integration;
pushes and open PRs retain local branches. Settlement rechecks ownership, terminal state, absent live lanes,
registration, writer custody, Git sequencer state and head under the existing
removal locks. Resumed attempts may settle a worktree whose owner is in their
recorded continuation lineage, using the same bounded ancestry proof as resume.
Ignored output uses GC's existing declared-regenerable walker
and keep paths. Only the project's declared regenerable output may be removed;
other ignored files retain the tree. Other worktrees keep the existing
GC/removal policy. A host lifecycle receipt records intent before removal,
then pruning or the retention reason in `worktree-settlement.json` beside the
attempt result. An interrupted intent whose checkout is gone replays as pruned.
At settlement the host best-effort refreshes the exact `origin` tracking ref
for every writer with a branch, including explicit and adopted worktrees,
before any prune upstream pin. The bounded fetch runs outside the collection
lock and does not affect the lane outcome. A missing `origin` or remote branch
is silently skipped. Other failures may add a fixed, redacted
`tracking_ref_warning` to a lane-owned settlement receipt; they never create
or change an adopted worktree's receipt. Concurrent refreshes of the same
branch may race and produce a warning.
- Why: a lane may reclaim only work it owns and commits proven retained elsewhere.
- Test: `crates/confinement/src/worktrees.rs::adopted_owner_proves_membership_and_enables_custody`;
  `::owned_cleanup_refuses_dirty_unmerged_and_adopted_trees`;
  `tests/dispatch_provider.rs::owned_dispatch_creates_and_replays_after_pruning`;
  `tools/front-door/src/gc_support.rs::writer_tracking_refresh_handles_remote_branch_absence_and_failure`;
  `tools/front-door/src/fabric.rs::settlement_records_tracking_ref_failure_without_failing_lane`;
  `tools/front-door/src/fabric.rs::explicit_writer_refreshes_tracking_ref_while_collection_is_busy`;
  `tools/front-door/src/fabric.rs::adopted_writer_refresh_failure_does_not_change_receipt`.
- Owner: `crates/confinement/src/worktrees/owner.rs`, `crates/cli/src/run.rs`, `src/fabric.rs`

## Delivery modes and settled pickup

`dispatch --deliver none|push|integrate:BRANCH|pr` resolves to the existing
explicit writer destination contract. Omission keeps delivery off. Push and
PR modes use `origin` and the lane branch; `integrate:BRANCH` selects an
existing local branch, and `integrate:origin/BRANCH` selects an existing
remote integration branch. The legacy remote, branch and open-PR flags remain
aliases and conflict with `--deliver`. Read-only lanes may select only none.
Direct push and integration refuse publication to the default branch recorded
by any configured remote and the primary checkout branch. If no remote HEAD
is available, both `main` and `master` are protected. PR delivery publishes a
feature branch and may target a protected branch as its PR base; its published
head branch is subject to the same protection.
Dispatch checks publication targets before admission for concise modes,
legacy flags and manifests, so a refused target leaves no admitted run.
Destinations supplied in flags are also checked before creating an owned tree.
Accepted operations compare and replay their immutable inputs before checking
current branch protection; later protection changes do not invalidate replay.
Actual delivery still rechecks protection.

Integration serialises by destination using a stable lock in the common Git
directory. It fast-forwards when possible, otherwise uses `merge-tree` and
`commit-tree` to prepare a merge commit without switching checkouts. A conflict
or configured custom merge driver refuses before publishing a ref. A local
integration branch checked out in any worktree is refused. Publication uses
expected-old ref comparison locally or an explicit remote lease after an
ancestry proof. Remote delivery refreshes and verifies its tracking ref.

`lane deliver ATTEMPT [--deliver MODE]` uses the recorded destination, or
origin push when the settled lane recorded none. It requires a succeeded
writer, released custody, no new reservation, a clean tree and the exact
receipt head. After reviewing a `review_required` writer, the chair can use
`lane deliver --accept-review ATTEMPT` to accept its protected changes and
deliver the same exact head. The delivery receipt records the acceptance and
protected paths. Lanes and peers cannot accept review. Delivery persists a
keyed host pickup receipt separately from the immutable terminal record.
Successful pickup replays without repeating
external effects, while failures remain explicitly retryable. A worktree
already pruned by its own lifecycle may replay its successful receipt.
PR bodies use commit subjects and bodies with run and attempt IDs, capped at
60,000 bytes with an explicit truncation note and supplied to `gh` through stdin;
existing PR reuse requires an open PR at the exact delivered head.
- Why: delivery is host authority attached to a verified result, and a later
  pickup must not silently deliver unrelated edits.
- Test: `crates/run/src/delivery_tests.rs::local_integration_fast_forwards_merges_and_replays`;
  `::local_integration_conflict_dirty_and_checked_out_refuse_without_partial_state`;
  `::remote_integration_refreshes_the_leased_destination`;
  `::settled_delivery_refuses_dirty_or_changed_head_and_replays_success`;
  `crates/cli/src/lib.rs::lifecycle_flags_parse_and_refuse_ambiguous_authority`.
- Owner: `crates/run/src/delivery.rs`, `crates/records/src/run.rs`, `crates/cli/src/run.rs`

## Fresh continuation fallback

Resume defaults to a fresh attempt when no provider/session was launched or
retained continuation state is missing, expired or cannot be searched within
its bound. `--no-fallback` gives a typed continuation refusal instead. The
new attempt preserves the parent, route, account, access, writer worktree,
base, delivery and authority; it skips provider continuation and records
`continuation: fresh` in the attempt and the bounded reason in its sealed
canonical request. It does not mutate the terminal source record.

The fresh prompt contains the original prompt (following bounded retry
lineage), prior result digest and a bounded tail, bounded Git history since
the owner's recorded base
with any unavailable-history diagnostic, and the new prompt. When the prior
worktree is displaced, the prompt states that its history is unavailable;
it never substitutes primary-checkout history. Existing secret
scanning and prompt limits apply to the combined text. The continuation
admission fence still permits one successor, and operation replay retains the
original choice and prompt even after provider-state expiry. Uncertain custody
never triggers a new writer; refusal names the fresh fallback when it cannot
launch safely.
On the host, policy, provider preparation and write-grant validation run before
recording the fallback operation or spending the continuation. A preparation
refusal leaves no successor or staging, and the same operation can be retried
after repairing the environment. Admission retains that exact prepared plan.
Later custody or handoff failures are reported as execution outcomes of the
accepted attempt, as for dispatch.
Before admission and again under writer custody, continuations verify the
worktree owner's lane lineage. Launch snapshots durably record whether the tree
was lane-owned. Explicit unowned and adopted trees resume in place unless a
current lane owner names another lineage. Previously lane-owned trees still
require a matching lane owner even if that owner record is later removed or
replaced by adoption. Older attempts without launch evidence retain compatibility
with explicit worktrees, but a current foreign lane owner always refuses. Missing or
reassigned worktrees require a new owned checkout for fresh fallback;
`--no-fallback` refuses them.
- Why: a missing provider handle should not require the chair to reconstruct
  context or bypass immutable attempt records and writer custody.
- Test: `crates/run/src/dispatch_tests.rs::fresh_resume_preserves_lineage_and_authority_without_provider_state`;
  `::resume_continues_a_terminal_attempt_once_with_its_route`;
  `crates/run/src/controls.rs::lineage_tests::resume_preserves_explicit_unowned_and_adopted_worktrees`;
  `::resume_refuses_a_pruned_worktree_reused_by_another_lane`;
  `crates/cli/src/run.rs::tests::displaced_fallback_reports_unavailable_history`;
  `crates/cli/src/lib.rs::lifecycle_flags_parse_and_refuse_ambiguous_authority`.
- Owner: `crates/run/src/controls.rs`, `crates/run/src/launch.rs`, `crates/cli/src/run.rs`
