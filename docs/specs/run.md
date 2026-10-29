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

`run` accepts the same effort and network controls as `dispatch`. An explicit
model without an effort uses its configured route or pool default when those
defaults agree; conflicting defaults leave the provider default in place.
The receipt reports the applied effort and effective network access.

## Cargo home seed

An opted-in macOS writer may seed its private Cargo registry and Git store from
the host stores independently. It must never replace an existing store or publish
a partially cloned tree. Unsupported entries or clone failures must fall back without
blocking the lane. Each attempt must report the seed outcome and reason in
`cargo-setup.json` and `cargo-git-setup.json` on a best-effort basis.
- Why: a seed must preserve lane isolation and leave Cargo usable when cloning
  is unavailable, while making the outcome observable.
- Test: `crates/run/src/cargo_seed.rs::tests::existing_registry_is_never_replaced`;
  `::failed_clone_removes_partial_tree`; `::registry_clone_is_private`;
  `::git_seed_is_independent_and_preserves_existing_stores`; `::git_clone_is_private`;
  `crates/run/src/writer.rs::cargo_setup_tests::cargo_setup_record_reports_writer_and_read_only_outcomes`;
  `src/compose_tests.rs::writer_dispatch_uses_lane_cache_and_narrow_provider_state`;
  `::read_only_dispatch_applies_bound_git_and_project_writes`
- Owner: `crates/run/src/cargo_seed.rs`, `crates/run/src/writer.rs`

## Cargo target seed

An opted-in macOS writer with a fresh target may clone the newest host target
generation matching the complete `rustc -vV` output and host triple. Preparation
must never execute a writer-controlled toolchain outside confinement and must
fall back when the lane compiler cannot be verified. Mismatches
and clone failures leave the target empty and are reported in
`cargo-target-setup.json`. Existing targets are retained. A clone is private and
writable; no lane may write the published generation. Preparation holds the
store lock until cloning finishes, so GC cannot remove the source mid-clone.
- Test: `crates/run/src/cargo_seed.rs::tests`; `crates/run/src/target_seed.rs::lane_probe_tests`; `crates/run/src/writer.rs::cargo_setup_tests`; `crates/cli/src/run/provider.rs::configured_grants_cannot_expose_host_indexes`; `crates/run/src/target_seed_gc.rs::tests::active_clone_lock_prevents_collection`; `src/compose_tests.rs::writer_dispatch_uses_lane_cache_and_narrow_provider_state`
- Owner: `crates/run/src/cargo_seed.rs`, `crates/run/src/target_seed.rs`

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

## PR contribution storage

The run owner provides a host-state store at `pr-contributions.sqlite3` for
explicit attempt and GitHub repository/PR links. A link is idempotent and has
an unknown weight until a complete allocation is sealed. Weights use unsigned
millionths and must cover every link for the attempt and sum to 1,000,000.
Sealing and its replay are transactional; conflicting replays and new links
after sealing are refused. Callers must establish contribution from trusted
host facts, not transcripts. The database and its SQLite companions are part
of host state and must be retained in backups.

Capture uses facts owned by the attempt's local host. Writer dispatch into a
registered worktree snapshots project, branch and attempt before launch.
Read-only dispatch from that worktree records the `read_only` role; writers use
`worker`, and their continuations use `repair`. Capture includes queued,
failed and cancelled attempts. An attempt appears once per PR, including on
dispatch, continuation or binding replay. Resolved model and account labels
are retained at admission and terminal publication; requested placement is
not substituted for unknown execution facts.

`pr open` binds the branch to its canonical GitHub repository, PR number and
head after creation; `--dry-run` captures nothing. `land` binds before merging
or removing worktrees. Landing skips new bindings for fork PRs, protecting
local namesake branches. PR creation and landing warn on attribution failure
and name `provenant pr contributions N --bind` as the retry; an attribution
failure cannot fail an otherwise successful operation. Capture errors after
run creation likewise leave dispatch successful and seal the first diagnostic
in `runs/<run>/pr-contribution-diagnostic.txt`, with a warning on stderr.

Bindings belong to a project/branch generation. Landing and observed
`CLOSED`/`MERGED` PRs (`pr status`, `land`, or `pr contributions N --bind`)
close existing bindings. Before binding, including after `pr open`, sweep every
other locally open binding on the branch with one GitHub state lookup per PR.
Close bindings GitHub reports as `CLOSED` or `MERGED` before inserting the new
binding. A failed lookup warns and retains the existing binding behaviour.
Every newly observed closure advances the generation;
replays cannot reopen it or advance twice. PRs still open at the boundary retain
bindings in the old generation and span into the new one. A newly bound PR only
receives attempts from the current generation, even when an older PR remains
open. Capture resolves only attempts and open PRs sharing a generation. Continuations and validated children inherit their
parent's generation unless an authorised replacement changes branches.
Capture stays offline. The sweep protects attempts captured after the next
binding; it cannot retrospectively distinguish attempts captured between an
external closure and that observation. To establish the boundary before new
work, use `pr status N` or `pr contributions N --bind` on the old PR first.
A closed-then-reopened PR is not re-tracked, even with `--bind`: closure is an
immutable generation boundary and replay cannot reopen its binding. Open a new
PR to track subsequent work. Historical links remain
when the branch is removed. Links describe branch participation, not commit
authorship. Automatic capture leaves weights unknown and never seals an
allocation. Explicit links continue to refuse a newly discovered link after
sealing. Automatic resolution skips sealed attempts without changing their
allocation, continues binding other attempts and reports `coverage.skipped_sealed`.

Multiple open PRs on one branch retain their links. An attempt linked to more
than one PR without a sealed allocation is `ambiguous`; consumers must exclude
it or use an explicit complete allocation rather than double-count its full
cost. The link cap is 256 per attempt. Additional links are skipped and
coverage becomes `truncated`, including for a PR whose link was skipped. This
does not block further dispatch, binding or landing. A study must count an
attempt once per merged PR, keyed by host, repository, PR number and attempt,
and exclude unbound, ambiguous, truncated, skipped or missing coverage until resolved.

`provenant pr contributions N [--json]` reads retained local links without
creating or migrating a store; GitHub's repository lookup supplies the
canonical repository namespace. `--bind` explicitly retries binding using the
observed branch, head and state, and refuses fork PRs. Version 1 rows remain
readable with null metadata. Versions 1 and 2 migrate to version 3 at the next
host write; old `review` roles become `read_only` because no explicit review
request field established that intent. Existing historical links are retained,
without reconstructing their original lifecycle. The database is outside
attempts and worktrees, so `gc --apply`, landing cleanup and worktree removal
retain links and metadata. Rollback must retain the database; older binaries
cannot write version 3.

### PR contributions JSON reference

The normal command-result envelope contains `data` with these fields:

| Field | Type and meaning |
|---|---|
| `pull_request` | Object with non-null canonical lower-case `repository` (`owner/repo`) and positive integer `number`. |
| `attempts` | Array, once per local attempt linked to this PR, ordered by `queued_at` then `attempt`. Null queue times sort first. |
| `coverage` | Object described below; prospective local capture never proves complete historical or peer coverage. |

Each `attempts` row always includes all of these fields. Missing facts are
JSON null (text output uses `unknown`).

| Field | Type and nullability |
|---|---|
| `attempt` | Non-null attempt UUID string. |
| `pull_request` | Non-null repository/number object, as above. |
| `run` | Run UUID string, or null for legacy/explicit links without host metadata. |
| `outcome` | Terminal outcome string, or null before terminal publication or when unavailable. Values: `succeeded`, `review_required`, `failed`, `rejected`, `cancelled`, `timed_out`, `rate_limited`, `quota_limited`, `auth_required`, `model_unavailable`, `permission_denied`, `quarantined`, `lost`, `uncommitted`, `input_required`. |
| `queued_at` | Canonical RFC3339 UTC string with six fractional digits, or null when unavailable. |
| `finished_at` | Same timestamp format, or null before terminal publication or when unavailable. |
| `branch` | Captured local branch string, or null when unavailable. |
| `head` | Observed full PR Git SHA string, or null when unavailable. |
| `role` | `worker` for writer dispatch, `read_only` for cwd-linked read-only dispatch/validated children, `repair` for writer continuations; null for legacy/explicit links. No review intent is inferred. |
| `model`, `account` | Resolved placement strings, or null when unknown; requested or lane-written placement is not substituted. |
| `weight_millionths` | Integer from 0 to 1,000,000, or null until explicit complete allocation is sealed. |
| `pr_count` | Non-null integer count of retained PR links for this attempt, across repositories and including closed PRs. A truncated count is a lower bound. |
| `ambiguous` | Non-null boolean: more than one retained PR link and no sealed allocation. |
| `truncated` | Non-null boolean: additional links for the attempt have been skipped at the cap. |

Coverage fields are always present and non-null:

| Field | Values |
|---|---|
| `local_attempts` | `unbound` when the project is unregistered, the store is absent, or this PR has neither binding nor legacy links; otherwise `truncated` when any associated attempt lost a link, `skipped` when a sealed allocation prevented a link, `ambiguous` when any row is ambiguous, or `captured`. The precedence is unbound, truncated, skipped, ambiguous, captured. An empty captured list is possible for a successfully bound PR with no captured attempts. |
| `ambiguous` | Boolean indicating any returned row has ambiguous allocation, independently of the coverage precedence. |
| `truncated` | Boolean indicating omitted links, including attempts absent from the returned rows. |
| `skipped_sealed` | Non-negative integer count of distinct branch attempts omitted from this PR because their allocation was already sealed, including attempts absent from the returned rows. Legacy schemas return zero. |
| `peer_attempts` | Always `unknown`. |
| `known_peer_hosts` | Array of configured peer host labels, sorted and deduplicated; empty when none are configured. |

The reader never opens peer records or forwards peer queries. Older attempts
without captured membership are not inferred from transcripts or centrally
backfilled.

- Tests: `src/pr_land_tests.rs::pr_contribution_roundtrip` drives dispatch,
  continuation, PR creation, landing, worktree removal and GC;
  `crates/run/src/pr_contribution.rs` covers allocation and retained placement.
- Owners: `crates/run/src/pr_contribution.rs`, `crates/run/src/service.rs`,
  `src/pr/contributions.rs`.

## Work correlation lookup

`run::WorkIndex` owns immutable membership in `<host-state>/work-index.sqlite3`.
The trusted host seals root, resume and continuation attempts before marking
them Starting. New nested dispatches use host-verifiable derived IDs. Their
canonical request carries `child_attempt_id_version: 1`. The child run ID is
the first 16 bytes of SHA-256 over `provenant-child-run-v1\0`, then the raw
project and parent attempt IDs, the SHA-256 digest of the operation ID's UTF-8
bytes, and the canonical request digest. Each attempt ID is the
first 16 bytes of SHA-256 over `provenant-child-attempt-v1\0` followed by the raw,
fixed-width project, parent attempt, run, task and canonical request digest
bytes, in that order. The namespace comes from the host's project and sealed
parent context. The host checks every proposed ID during child validation and
checks both run and attempt identities again at launch and sealing. A mismatch refuses
launch before membership capture, so rewriting a queued child ID cannot claim
a sibling's membership or poison its launch. A lane cannot claim a historical
run namespace by choosing its run ID and recomputing the public attempt hash.

Nested continuation requests use version 1, retaining the original
parent-attempt namespace and including their continuation parent in the
canonical digest. Replays retain the original scheme, including legacy
prepared operations whose request was not yet published. A child-store
continuation uses its requesting parent's sealed network bound, or the same
provider-only profile as other provider-only lanes when that context is
unavailable. On macOS that profile currently permits outbound TCP 443 beyond
provider endpoints ([#2118](https://github.com/mblauberg/provenant/issues/2118));
see [child launch policy](routing.md#the-host-validates-and-launches-children).

This scheme avoids rekeying lane-published records and IDs already returned to
callers. There is no migration of existing child records: an absent version
marker preserves explicit `Unindexed` coverage. Chair admission separately
requires a retained host seal for legacy child launch, as described below.
An unknown version refuses launch. Removing the marker only loses indexing;
it never adopts a lane-proposed ID as trusted membership. Replays preserve the
original version, including repair of legacy prepared operations. Calling the
ordinary start path on a child store cannot seal membership. Only host child
launch from a validated versioned request can seal nested membership.

Opening and sealing remain crate-private. The public `validate_work` reader
accepts only the configured host `StateRoot`; CLI/MCP bind it to their trusted
project identity and pass its coverage to Fabric before a Fabric transaction.
Identical captures replay; disagreements and permanent capture errors refuse
handoff. Contention or cancellation leaves the attempt queued for retry.
Repository identity is the verified canonical Git common directory. Head is the
host-observed launch HEAD for readers; writer heads and unavailable Git facts
remain explicitly unproven. Membership proves neither chair-session ownership,
successful launch nor workflow authority.

`WorkSelector` requires project and run, with optional task, attempt, repository
and head constraints. Reads check at most 64 candidates and 64 KiB per record,
using the same byte bound as writes. `Verified` requires a task or attempt;
run-only matches are at best `Partial` because there is no trusted complete member list.
Legacy or missing nested, historical and unprepared work, including an empty
run-only selection, returns `Unindexed`. Contradictory
facts return `Mismatch`, ahead of unavailable repository/head facts and unindexed
coverage, including run-only selectors. A mismatch observed among the first 64
candidates also wins over `Limit`; otherwise `Limit` never proves absence. Lookups traverse no child roots and accept
no caller-supplied store locator. Configured grants cannot expose the host index,
its companions or lock. Membership survives lane-local record removal. Index
growth is unbounded and is not counted towards the disk floor.

- Why: caller claims cannot establish membership or hide a known contradiction,
  and a waiting index reader must not block unrelated Fabric writers.
- Test: `crates/run/src/correlation.rs::tests`; `crates/cli/src/run/provider.rs::configured_grants_cannot_expose_host_indexes`
- Owner: `crates/run/src/correlation.rs`, `crates/run/src/launch.rs`

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
digest. The index write uses the exact request returned by child validation;
it never re-reads lane-writable request bytes to choose the digest to seal.
A disagreeing child proposal is refused. Lane-local requests and even a
lane-local Fabric database or the shared mail database cannot replace this
association. A retry preserves the original proposal; continuations copy the
original host index binding.
Index failures or exposing write grants refuse launch. Replay a root submission with the
same operation ID: launch repairs its index from the immutable host request,
including owned-worktree replay. Lane stores cannot repair it.
Transient index errors leave queued children available for a later scan.

Admission audit (#2088):

- New child associations require the version 1 derived run ID check used by
  WorkIndex, before any index write. An unversioned request with a lane-chosen
  run ID is refused, so it cannot bind a missing row or shadow another run.
  Existing legacy host rows remain readable and replayable only with their
  sealed request digest; lane stores cannot create or repair missing legacy
  rows. Removing the version marker grants no chair association. A legacy
  continuation whose host seal is lost is therefore refused permanently rather
  than repaired from lane bytes.
- New nested continuations never take a lane-supplied ID: resume derives and
  overwrites their run and attempt IDs.
- Host continuations given a reopened lane-local store check the replayed run ID
  against the request and operation using the same derivation before copying
  the chair binding. New inheritance also verifies the source run and attempt
  derivations and requires the source's host seal. Copying another run's
  request and operation while substituting the source attempt is refused.
  A legacy source cannot prove attempt membership for new inheritance.
  Retained continuation rows, including unversioned ones, replay their sealed
  binding without copying a lane-proposed source. Forged continuation
  operations cannot create a chair row under a lane-chosen ID, including the
  host's early operation replay after reopening the lane store. The regression
  also replays a retained legacy continuation through that entrypoint.
- Root replay with the same operation ID and a different submitting chair
  retains the original immutable request. Even repair of a missing host row
  seals the original chair and digest; a conflicting row is refused.

The chair row is sealed before handoff, so presence in the index does not imply
that the run launched. Launch validation reads `fabric-runtime::chairs::ChairIndex`;
for reads of existing runs, missing records and older runs are unbound. `console board` reads existing host associations
without initialising the index. Provider handles and child requests are never fallbacks. Graph and
peer executions remain unbound until they retain an originating chair separately
from the actor owning operation replay. Board and tree rendering are separate.

Association rows live as long as run metadata. Current GC removes provider
payloads, logs, events and worktrees; it never removes run/attempt metadata.
Whole-run collection must delete its association in the same host-owned removal
workflow. Lane-writable missing child records alone cannot authorise deletion.

- Test: `crates/fabric-runtime/src/chairs.rs::nested_chair_binding`;
  `crates/run/src/service_tests.rs::chair_binding_replays_the_original_submission`;
  `crates/cli/src/run/children.rs::tests::{legacy_child_cannot_claim_a_chair_under_a_lane_chosen_run_id, child_chair_seals_the_validated_request_despite_a_lane_rewrite, continuation_cannot_copy_a_chair_to_a_lane_chosen_replay_run, continuation_cannot_redirect_its_source_to_another_runs_chair}`;
  `crates/cli/src/run.rs::tests::operation_replays_after_its_context_ref_moves_or_is_deleted`;
  `crates/run/src/dispatch_tests.rs::{nested_continuation_uses_host_derived_ids, nested_continuation_preserves_legacy_replay_even_before_request_publication, nested_continuation_preflight_does_not_reuse_crashed_staging}`
- Owner: `crates/fabric-runtime/src/chairs.rs`, `crates/fabric-store/src/chairs.rs`,
  `crates/cli/src/run.rs`, `crates/cli/src/run/children.rs`

## Atomic admission

Admission and operation insertion are one transaction, and the immutable
request is published before acknowledgement. No launch, probe, wait or
supervisor call happens inside a transaction. Admission bounds active lanes
at 64 per host and 32 per project and queued lanes at 256 per host, with
memory and disk floors, startup spacing and a deadline.

On macOS, Warning or Critical kernel memory pressure defers both classes.
Normal pressure still requires the memory floor. An unavailable or unknown
pressure sample is logged and falls back to the floor; Linux uses the floor.
Disk and memory floors apply to both classes. Writers also require a host-wide
build slot, defaulting to one at or below 16 GiB of RAM and two above that;
read-only lanes do not consume one. The provider-exit callback releases this
slot before Git import or terminal bookkeeping, including on cancellation. At expiry, disk floor, known pressure and
full build slots have distinct refusal reasons. Disk-floor refusal takes precedence
when disk and pressure are both insufficient; otherwise Warning or Critical
pressure is named before build-slot contention. A failed memory sample or a
floor-only memory shortage retains the generic admission refusal. The ledger associates new leases
with the boot and guardian identity; a different boot, a definitely absent
guardian or an admission row proven absent from the same readable owner store
releases a lease. A busy, missing, replaced or unreadable store retains the
lease until the guardian is confirmed absent or the boot changes. The ledger
records the owner file's device and inode to detect replacement. Malformed
lease evidence is retained unless independent evidence proves release.
A parsed host configuration with invalid build settings fails immediately with a named configuration error. An unreadable or syntactically incomplete configuration is retried, so an unavailable current disk floor never admits unchecked.
An unavailable guardian identity is retried until the admission deadline, then
reported as a typed refusal.
Both identity fields are required on every lease. Earlier lease schemas were
never installed; upgrades retain spacing but discard their lease rows.

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
memory floor (5 %) and the live `gc.free_floor` disk floor refuse both classes.
The disk default is 10 % of the state volume, with a 50 GiB minimum. A ticket with no class is a writer; opening a version 1 ledger upgrades it in
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
capacity. It reads the store and host ledger, from a nested lane too, and names only a wait it can see. `lanes --host` and the other peer
listings show the same two fields: the peer derives them from its own store and
ledger (peer action `lane_waits`) and an older peer that does not know the
action simply shows none.
- Why: an accepted receipt must mean durable admission, and read-only lanes
  must not queue behind writer spacing that exists to pace worktree and
  provider startup.
- Test: `crates/run/src/ledger.rs::tests::{live_guardian_fences_busy_replaced_and_corrupt_store, cross_boot_malformed_and_readable_row_removal_release}`;
  `crates/run/src/dispatch_tests.rs::{memory_pressure_defers_even_when_available_memory_is_high, pressure_errors_fall_back_and_normal_still_obeys_floor, writer_slot_refusal_and_release}`;
  `crates/providers/src/lane.rs::tests::claude_shell_restores_jobs_after_scrub`;
  `crates/config/src/gc.rs::build_tests::{defaults_and_overrides, automatic_and_invalid_build_values}`;
  `crates/run/src/dispatch_tests.rs::{configured_build_limit_controls_host_leases, invalid_build_settings_refuse_without_waiting, guardian_identity_unknown_retries_and_names_expiry}`;
  `crates/fabric-runtime/src/config.rs::tests::lane_build_settings_validate_shared_bounds_and_auto`;
  `crates/run/src/dispatch_tests.rs::admission_holds_run_concurrency_spacing_floors_and_deadline`;
  `::read_only_lane_is_admitted_inside_a_writers_spacing_window`;
  `::writers_stay_20_seconds_apart_and_read_only_starts_do_not_reset_the_clock`;
  `::read_only_lanes_are_spaced_2_seconds_apart`;
  `::memory_floor_refuses_both_lane_classes`;
  `::project_cap_allows_up_to_32_active_lanes`;
  `::legacy_tickets_without_a_class_are_treated_as_writers`;
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
2. The scheduler samples resources outside the transaction, takes writer custody
   and passes the plan to supervisor. A lost acknowledgement records
   `launch_unknown`, keeps capacity and the reservation, and is never replayed.
3. Supervisor observes provider exit and releases host build capacity. Once
   cleanup proves quiescence, it releases the live writer lock before settlement.
   The durable reservation still excludes another writer from this worktree.
4. Settlement imports objects, publishes the branch, records the outcome and
   releases the reservation through the durable phases below.
5. A run closes only when every task and registered child is terminal.

`settlement-progress.json` binds the attempt scope, pinned branch base and head,
current phase, native process owner, retry deadline and one import attempt counter. A live owner
excludes a second driver. A dead owner can be replaced after checking its boot,
PID and start identity. An unknown process observation protects its owner for
at most ten minutes from the claim. The replacement continues the recorded phase; it does
not restart the provider. Journal locks cover only short reads and durable
updates. No settlement lock or writer lock spans an external command.

| Phase | Durable effect and replay |
| --- | --- |
| `import` | Import the pinned head through Git's quarantined receive path. Repeating the same staging ref is harmless. |
| `publish` | Compare-and-swap the real branch from its pinned base; the same head already published counts as success. |
| `record` | Seal result and exit evidence, remove the attempt's base and import refs, then commit the terminal attempt, summary, operation state and event. Recover an interrupted terminal commit from its pending evidence. |
| `release` | Verify the committed outcome and release evidence, then remove the settling reservation. Repeated release accepts the same committed attempt. |
| `complete` | Settlement is finished; ordinary terminal cleanup may reclaim disposable scratch. Import refusals retain private Git for host recovery. |

Settlement permits three attempts that reach object import. Contention and
pre-import checks do not consume this counter; resuming a later phase does not
consume another import attempt. Import claims back off for 5 seconds, 30 seconds
and 2 minutes instead of retrying immediately. Exhaustion records a terminal
failure naming the limit and retained private Git. Rejected objects and failed
publication never certify success. A pending terminal commit remains `settling`
with `publication_pending: true` and keeps the worktree reservation, while its
provider has already stopped consuming host build capacity and live custody.

`cancel` durably records the request and signals the supervisor without waiting
for a writer lock. The settlement driver checks it between phases and polls it
every 50 ms during import, instruction checks and optional automatic delivery,
stopping the owned Git child before recording `cancelled`.
Public `reconcile` starts the same binary's detached `internal settle` worker
only when a non-terminal attempt needs settlement. A run-scoped queued claim
and worker exclusion deduplicate concurrent requests; a queued claim expires
after ten minutes if no worker starts. Reconcile promptly reports pending
settlement. Neither CLI waits for object transfer
or an existing settlement owner; the supervisor owns this work after return.
- Why: each phase records enough evidence to resume without holding a writer
  lock through Git, and cancellation must remain available during import.
- Test: `crates/run/src/settlement.rs::tests`;
  `crates/run/src/objects_tests.rs::cancellation_kills_a_blocking_import_within_two_seconds`;
  `crates/run/src/tests.rs::publication_crashes_preserve_committed_records_and_allow_retry`;
  `::prelaunch_refusal_publishes_durable_never_launched_evidence`.
  No run-level test asserts that `launch_unknown` is never replayed (gap).
- Owner: `crates/run/src/settlement.rs`, `crates/run/src/holder.rs`,
  `crates/run/src/launch.rs`, `crates/run/src/store.rs`

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

Read-only runs use their bound repository with its Git metadata read-only.
Lane Git output is never trusted host evidence; the host uses its own pinned
Git and environment for evidence and settlement.
- Why: a read grant must not permit mutations of shared Git metadata.
- Test: `crates/providers/src/seatbelt_snapshots.rs::agy_read_only_linked_worktree_git_directories_are_not_writable`
- Owner: `crates/providers/src/launch.rs`, `crates/confinement/src/permissions.rs`

## Lane Git and import

Every writer gets a complete Git directory at `scratch/git-common`: its own
index, HEAD, refs, config, temporary files, merge and rebase state. The host
copies the initial linked index and ref snapshot before launch. Plain
`GIT_DIR` and `GIT_COMMON_DIR` select that directory, `GIT_WORK_TREE` selects
the lane checkout, and `GIT_OBJECT_DIRECTORY` selects `scratch/git-objects`.
That private object store reads the shared store through
`objects/info/alternates`. Stash, rebase, merge and garbage collection operate
on private Git state without individual metadata grants. Launch pins the base
as `refs/provenant/lane-base/<attempt>` until terminal publication, so shared
garbage collection cannot discard a base that a private rebase no longer names.

The original worktree gitfile stays unchanged so the host retains its verified
repository identity. Seatbelt denies writes throughout both the shared Git
directory and the linked admin directory, including refs, config, hooks, info
and the original index. These environment selectors intentionally select the lane
repository. To inspect a second repository, run the pinned native Git executable
with GIT_DIR, GIT_COMMON_DIR, GIT_WORK_TREE and GIT_OBJECT_DIRECTORY unset.
The small Git wrapper has one source and restores
scrubbed environment or supplies publishing credentials; it does not inspect
Git commands or route their objects.

The host seals the selected attempt candidate before launch. After provider
exit and quiescence, it reads the selected private branch as raw metadata and
pins its OID in the settlement journal. It never runs host commands with the
lane's mutable Git config. Before import, a recursive census refuses symlinks
anywhere in `git-objects`. A foreign `info/alternates` entry is refused; the
host's original shared alternate is removed along with HTTP alternates,
commit graphs and multi-pack indices. A clean host-owned bare sender view
then selects only the bound private objects and shared store as alternates,
with `GIT_ALTERNATE_OBJECT_DIRECTORIES` explicitly empty. It preserves only the
host's trusted shallow boundary. Lane metadata cannot declare missing parents
to be a valid shallow boundary. Custody must still be absent immediately before
Git re-resolves the private object path.

The host pushes the pinned head into the shared repository using its pinned
Git executable for both processes. In shell notation the receive operation is:

```sh
git push --no-verify \
  --receive-pack='git -c core.hooksPath=/dev/null -c receive.fsckObjects=true -c transfer.fsckObjects=true -c receive.unpackLimit=1 -c transfer.unpackLimit=1 -c receive.maxInputSize=CAP receive-pack' \
  SHARED_GIT_DIR HEAD:refs/provenant/import/OID
```

The production sender selects the pinned OID rather than trusting lane HEAD.
The receive path is preferred to fetch because `receive-pack` quarantines the
incoming objects before promotion. Git checks new object contents and pack
integrity with fsck enabled, then checks connectivity against existing refs.
Both unpack limits force `index-pack` for a non-empty transfer, including a
small duplicate-entry pack. The input cap is four times the sanitised private
object bytes plus 16 MiB. The receive timeout is 120 seconds plus one second
per MiB of that cap, rounded up. Oversized input receives an explicit size
refusal. Before import, a sweep removes abandoned quarantine and Provenant
staging directories only when their contents are all older than 24 hours.
A refusal keeps incoming corrupt or disconnected objects out of the shared
store. Import work follows the new transfer and its connectivity frontier;
it does not hash or enumerate unrelated shared history. Shared objects are
trusted host-owned data because lanes cannot write them. An unused malformed
private object omitted from the transfer never enters the shared store.

A local Git 2.53.0 probe used a 32 MiB random shared blob and one new text-file
commit. Receive took 0.0738 seconds and added three objects totalling 290 bytes;
the sender's private store used 471 bytes including its alternates file. This
checks transfer scope on a small fixture. It is not a 50 GB seal benchmark or
a peak-disk measurement.

Git owns hashing, parsing, pack checks and quarantine. There is no custom
object verifier, full-store generation scan, verification cache or separate
verification deadline/retry mechanism. SHA-1 and SHA-256 repositories use the
same Git receive boundary. The host restores the private index for instruction
quarantine only after import succeeds, clearing skip-worktree and
assume-unchanged flags so an uncommitted edit remains visible. The host
publishes the real branch with a
compare-and-swap against the pinned launch base. Replaying the same head is
idempotent; a competing shared ref change refuses publication. A published tip
must descend from the lane base or the current `origin` default branch, which
allows rebases while refusing unrelated and orphan histories. Success,
cancellation and failure remove `refs/provenant/import/<head>` and the lane's
base pin. Host integration and delivery check connectivity with
`rev-list --objects --missing=error` excluding the lane base, then read those
delta objects through `cat-file --batch` to refuse corrupt reachable payloads.
They do not rescan unrelated shared history.

[Drain writers before upgrading](../runbooks/install.md#bootstrap-and-upgrade).
An older interrupted settlement with its bound private object directory can
resume through this import path. An attempt without that source is refused
with instructions to recover its commits on the host and start a fresh run.
There is no legacy verifier or separate settlement path. Terminal refusals
release live custody and the reservation while preserving private Git evidence
for host recovery.
- Why: Git already owns the receive boundary; private Git state removes the
  fragile list of individual write exceptions under the shared repository.
- Test: `crates/confinement/tests/lane_git_conformance.rs`;
  `crates/run/src/objects_tests.rs`;
  `crates/run/src/delivery_tests.rs::terminal_imports_private_objects_before_quarantine_reads_the_lane`
- Owner: `crates/confinement/src/permissions.rs`, `crates/providers/src/launch.rs`,
  `crates/providers/src/lane.rs`, `crates/run/src/objects.rs`,
  `crates/run/src/writer.rs`, `crates/run/src/delivery.rs`

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

`reconcile` queues bounded background recovery of queued and orphaned work
and never replays an uncertain launch. The supervisor resumes the durable
settlement phase; the CLI does not wait for import or take the writer lock.
If a writer guardian commits a terminal record and releases
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
Adoption does not mistake an incomplete native census for a live writer. As
with ordinary mutations, an exclusive writer lock and absent reservation prove
custody; an observed live process still refuses, and callers requiring a
complete census still refuse unknown observations.

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

Integration prepares objects without holding an application lock through Git.
It fast-forwards when possible, otherwise uses merge-tree and commit-tree to
prepare a merge commit without switching checkouts. A conflict or configured
custom merge driver refuses before publishing a ref. A local integration branch
checked out in any worktree is refused. Publication uses expected-old ref
comparison locally or an explicit remote lease after an ancestry proof.
A concurrent publisher can win; the losing delivery records an integration
failure for explicit host pickup rather than adding another retry counter.
Before resolving an origin integration target, the host refreshes the exact
remote branch and refuses delivery if the refresh fails. Final graph PR
delivery follows the same refresh rule.

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
after repairing the environment. Root fallback admission retains that exact
prepared plan. Versioned nested fallbacks preflight in fresh per-call staging,
discard that plan and prepare again at the derived final paths after admission.
This preserves crash retry without reusing unrecorded staging; launch repeats
policy and write-grant validation before handoff.
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
