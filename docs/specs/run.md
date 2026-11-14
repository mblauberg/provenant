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
itself. Grammar and bounds are in `provenant help --all`. Paths are relative to `tools/front-door/`.

Templates bind route and access controls before admission. The receipt reports
the applied effort and effective network access.

Every adapter launch passes through `finalise_provider_env`. Sandboxed and
native lanes receive the resolved `HOME`, attempt `TMPDIR` and private lane
`CARGO_HOME`, replacing inherited selectors even when ambient `HOME` is absent.
An attempt whose provider never spawned retains `terminal_cause = "spawn_failure"`.
With owner-proved custody release, the graph tries a new route; it never
classifies that infrastructure failure as a failed task.

## Cargo home seed

Writers share downloaded registry and Git data in a host-managed lane-only
store, separate from the host's trusted `~/.cargo`. Each writer keeps a private
`CARGO_HOME` for configuration and credentials and links only the download
stores and Cargo's package-cache locks to that shared store. Cargo serialises
concurrent cache mutation; automatic cache cleanup is disabled in lanes.
The launcher anchors and revalidates an existing shared download root before
compiling the profile; a substituted symlink cannot redirect that grant to
the host registry. Startup never copies or traverses the host registry. Existing private stores
are retained without walking or deleting them; fresh lane homes use sharing.
An absent host Git store is normal when the host has no Git dependencies and
is no longer a seed failure: the lane store is created empty.
- Why (default): registry-sized launch work delays every lane; sharing only
  lane downloads avoids allowing a lane to poison sources the host builds
  outside confinement. Private configuration and targets remain isolated.
- Test: `crates/run/src/cargo_seed.rs::download_stores_are_shared_without_reading_host_registry`;
  `crates/run/src/cargo_seed.rs::existing_private_downloads_are_retained_without_a_walk`;
  `crates/providers/src/tests_lane_toolchains.rs::no_lane_can_write_shared_toolchain_stores`;
  `crates/run/src/writer.rs::cargo_setup_tests`
- Owner: `crates/run/src/cargo_seed.rs`, `crates/providers/src/launch.rs`

Fresh provider-home preparation must not copy or scan sessions, rollouts or
other history. Confined Codex homes receive selected authentication and skill
links; native Codex retains the selected login's home. Resume and post-exit
model observation have separate bounded history readers.
- Test: `crates/providers/src/tests_lane_toolchains.rs::fresh_codex_home_setup_never_includes_provider_history`
- Owner: `crates/providers/src/launch.rs`, `crates/providers/src/codex.rs`

## Cargo target seed

On the host, fetch `origin/main` first and build seeds only from a trusted
`origin/main`. From the project, run `git fetch origin main` followed by
`provenant worktree seed-build`. The command pins the local remote-tracking
commit, checks it out in a disposable host checkout and runs `cargo build
--locked --all-targets` and `cargo test --locked --no-run` in `tools/front-door`.
It publishes a successful generation under the state directory's
`cargo-target-seeds/generations/`. The scheduled host refresh follows the daily
collection's fetch; when collection is disabled, fetch and build manually.

A writer with seeding enabled (the default) and a fresh target may clone the newest host target
generation matching the complete `rustc -vV` output and host triple. Preparation
must never execute a writer-controlled toolchain outside confinement and must
fall back when the lane compiler cannot be verified, recording a stable
`detail` code for the refusal (never an environment value). Mismatches
and clone failures leave the target empty and are reported in
`cargo-target-setup.json`. Existing targets are retained. Native and confined lanes receive the same
Cargo home, rustup home and target selectors before verification. Compiler
metadata has a two-second process timeout; target selection, cloning and
partial-stage cleanup share a further two-second cooperative budget, checked
between filesystem operations. Store and seed-phase locks are non-blocking.
An exhausted budget leaves any private partial stage unpublished and records
`seed_budget_exhausted`; a later attempt cleans it within the same budget.
Individual filesystem operations can exceed that budget under host I/O load.
A clone is private and writable; no lane may write the published generation. Preparation holds the
store lock until cloning finishes, so GC cannot remove the source mid-clone.
- Test: `crates/run/src/cargo_seed.rs::tests`;
  `crates/run/src/target_seed.rs::lane_probe_tests`;
  `crates/run/src/writer.rs::cargo_setup_tests`;
  `crates/cli/src/run/provider/confinement_test.rs::configured_grants_cannot_expose_host_indexes`;
  `crates/run/src/target_seed_gc.rs::active_clone_lock_prevents_collection`;
  `src/compose_tests.rs::writer_dispatch_uses_lane_cache_and_narrow_provider_state`
- Owner: `crates/run/src/cargo_seed.rs`, `crates/run/src/target_seed.rs`

## Lane build output

A lane defaults to at most two compiler jobs per build, with explicit job
settings retained. A lane builds with `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=line-tables-only`
unless `domains.fabric.lanes` or the lane environment says otherwise. Idle Cargo
profile directories in lane caches are collected least recently used first until
the allocated bytes of all profile directories, idle and busy, are within
`build_dir_max_bytes` and free space meets `gc.free_floor`; when busy directories
alone exceed the cap every eligible idle directory goes and the total stays over.
A directory is idle only when a non-blocking exclusive `flock` succeeds on all of
`.cargo-lock`, `.cargo-build-lock` and `.cargo-artifact-lock`; a held lock, or a
lock that cannot be probed, keeps the directory. Free locks are not enough: test
and run binaries execute after Cargo drops its locks, so a directory written
within `build_dir_grace_seconds` (15 minutes by default), or since the sweep
measured it, is kept. The grace covers only recent writes: a binary that runs longer than the grace without writing under `target/` is unprotected unless a Cargo lock is still held. The walk takes no lock and a sweep with nothing to remove
stops there; removal holds the host admission lock, so no lane is admitted
mid-removal, and probes each candidate again. Collection runs when a lane
settles, when admission finds the disk below the floor, and in `provenant gc`.
- Why (default): disposable build output must stay bounded without deleting active builds.
- Test: `crates/run/src/build_dir_gc.rs::a_held_lock_prevents_deletion_whichever_of_the_three_is_held`;
  `crates/run/src/build_dir_gc.rs::least_recently_used_goes_first_and_the_cap_stops_collection`;
  `crates/run/src/build_dir_gc.rs::the_free_space_floor_is_honoured_and_no_more_is_removed`;
  `crates/run/src/dispatch_writer_tests.rs::admission_below_the_floor_reclaims_build_output_and_then_admits`;
  `crates/cli/src/run/provider/mod.rs::cargo_profile_tests`
- Owner: `crates/run/src/build_dir_gc.rs`, `crates/cli/src/run/provider/mod.rs`

## Operation IDs

An operation ID is 1 to 128 ASCII letters, digits, `.`, `_` or `-`, starting
alphanumeric. Run scopes it by caller and project, and binds it to the
operation kind and a digest of the prompt, route, authority and execution
controls. Reader waits are excluded from the digest. The same ID and digest
return the recorded outcome without repeating effects. The same ID with a
different digest fails with `operation_conflict` (exit 2). `--operation-id`
exists on `steer`, inbox, chair and Fabric writes; `run`, `stop` and `retry`
do not take it, and a graph operation that carries `operation_id` is refused
in one line naming `name`, never silently ignored.

A graph launch replays by name and digest instead. `--name` is only a handle: a
graph name is its graph id, a lowercase letter then lowercase letters, digits
and single hyphens, at most 48 bytes, with no trailing hyphen. The host digests
the template name (or graph document), the resolved body of that template and
of every template it reaches through `use`, brief, typed `--set` values, placement and
route flags, and the source commit. The same digest under a name returns that
graph's status with exit 0 and launches or advances nothing, checking every graph in the family
(`NAME`, `NAME-2`, ...). A different digest launches the next free `NAME-k`
and says so in one line; no conflict is raised. Concurrent launches with
different digests that pick the same `NAME-k` both launch: the one that loses
the name takes the next free suffix. Without `--name` the host
assigns `TEMPLATE-K` (dots become hyphens) and never replays. A name with no
room for a suffix within 48 bytes is refused in one line. The digest is stored
on the graph document, so a lost reply that is repeated returns the graph.

A graph name remains reserved after checkpoint collection. Collection leaves a
durable reservation beside retained events, chairs and WorkKeys, all of which
identify the graph by name. A collected name has no checkpoint to replay, so a
later launch takes the next unused family suffix, even when its input digest
matches the collected graph. Names are not reused while any metadata can retain
the old identity. A direct journal bind that bypasses family allocation is
refused when its name belongs to a collected or incomplete generation.

Each attempt row also carries its work key: project, graph, node and attempt
number. The graph assigns the number, one-based in launch order within the
node and never reused; the store enforces one row per key and never assigns
it. A plain lane is a one-node graph with attempt 1.
The project-scoped graph checkpoint retains the same graph, node and attempt
tuple; the owner materialises its WorkKey in that project namespace and verifies
it against the referenced RunStore record. The route index is routing metadata,
not attempt identity. A budget wrap-up retains its terminal predecessor as a
separate history entry and records the admitted successor under its own WorkKey
number on the same route. `NAME/NODE#N` resolves that stored number, including
after checkpoint reload, rather than numbering entries by their list positions.
Admitting a key that already holds a row with the same request digest returns
that run; a different digest fails with `operation_conflict`. Operation IDs
remain an adaptor on the attempt row; there is no operation or run table.
- Why (default): lost replies and concurrent duplicates, including resume, must never
  launch twice.
- Test: `crates/core/src/ids.rs::operation_keys_are_bounded_and_cannot_be_path_components`;
  `crates/run/src/service_tests.rs::dispatch_replay_output_and_cancel_share_the_service`;
  `crates/run/src/tests.rs::plain_lane_and_one_node_graph_store_the_same_attempt_row`;
  `crates/run/src/tests.rs::concurrent_creates_of_one_work_key_admit_one_attempt`;
  `crates/fabric-runtime/src/graph/template_tests.rs::a_name_replays_its_digest_and_takes_the_next_suffix_for_a_new_one`;
  `src/fabric/interface.rs::a_named_launch_with_the_same_digest_returns_its_graph_without_launching`;
  `tests/graph_cli.rs::a_named_run_replays_its_input_and_a_changed_input_takes_the_next_name`
- Owner: `crates/core/src/ids.rs`, `crates/run/src/store.rs`,
  `crates/fabric-runtime/src/graph/template.rs`

### Refusal before admission

After an operation identity is assigned (for a graph launch, its name), a
permanent refusal before a run is admitted belongs to that operation, even when writer preparation cannot produce
a canonical run request. Retain its safe diagnostic and any bound submitting
chair before reporting the refusal. A storage failure
returns the storage error; it cannot acknowledge an unrecorded outcome.
Replaying that operation returns its retained refusal without consulting changed
worktree state or repeating preparation. A different request under the same
operation identity still conflicts; a graph launch with a changed digest takes
the next name instead. Correct the cause and start a new attempt through
`retry` to try again.

A refusal is stored as a terminal attempt row under the work key it would have
been admitted with, with no attempt record. That key is never reused, and
attempt readers and lane listings skip refused rows.

Graph launch refusals use the saved attempt key and launch actor. A proven
pre-admission refusal is evidence that no provider was accepted, so it can be
retried or cancelled without treating it as uncertain custody. It never has an
admitted run to read. Refusal notices reuse one message identity per operation;
notice recovery cannot create a second message or change the stored outcome.
- Why (default): a dirty writer must remain observable after the caller disconnects, and
  a lost reply must not turn its replay into a second launch.
- Test: `tests/refused_operation.rs::dirty_writer_refusal_is_durable_visible_and_notified_once`;
  `src/fabric/owner.rs::graph_refusal_replays_before_preparation_and_rejects_changed_intent`
- Owner: `src/fabric/owner.rs`, `crates/run/src/store.rs`, `src/settlement.rs`

## Task titles

Run and dispatch accept an optional human-supplied title, including per-task
titles in manifests and Fabric submissions. The title is sealed in the request
and copied to each attempt; it participates in operation replay identity.
Titles are nonempty, contain no control characters and are limited to 256 UTF-8
bytes. `status` displays titles when present and includes them in JSON output;
absent titles remain absent. Titles are never inferred from
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
Capture stays offline. Each observed closure reads `mergedAt`/`closedAt` and
moves unsealed attempts queued after it to the new generation, reported as
`coverage.reassigned`; attempts with no queue time stay and count as
`coverage.unplaced`. GitHub times have 1 s precision and are compared with the
local clock that stamped `queued_at`, so attempts within clock skew of the
boundary may be misplaced.
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
observed branch, head and state, and refuses fork PRs. The database is outside attempts and worktrees; GC, landing cleanup and
worktree removal retain contribution links.

### PR contributions JSON reference

The [contribution owner](../../tools/front-door/crates/run/src/pr_contribution.rs)
and [command renderer](../../tools/front-door/src/pr/contributions.rs) own the
payload. Missing metadata remains null; peer coverage is unknown. Readers must
not infer contribution from transcripts or centrally backfill old attempts.

- Test: `src/pr_land_tests.rs::pr_contribution_roundtrip`
- Owner: `crates/run/src/pr_contribution.rs`, `src/pr/contributions.rs`

## Work correlation lookup

Membership facts (repository and head) live on the host's attempt rows in
`<host-state>/run.sqlite3`, beside the attempt's work key; there is no separate
work index. The trusted host seals root, resume and continuation attempts
before marking them Starting. A nested dispatch is a child node of the calling
graph, admitted by the host graph owner, so its work key and attempt ID come
from the host's graph journal and run store, never from lane bytes. Calling the
ordinary start path on a child store cannot seal membership; only host launch
of an admitted attempt can. A child-store continuation uses its requesting
parent's sealed network bound, or open network when that context is
unavailable. The host network ceiling still narrows the effective access; see
[child launch policy](routing.md#the-host-validates-and-launches-children).

Opening and sealing remain crate-private. The public `validate_work` reader
accepts only the configured host `StateRoot`; the CLI binds it to its trusted
project identity and pass its coverage to Fabric before a Fabric transaction.
Identical captures replay; disagreements and permanent capture errors refuse
handoff. Contention or cancellation leaves the attempt queued for retry.
Repository identity is the verified canonical Git common directory. Head is the
host-observed launch HEAD for readers; writer heads and unavailable Git facts
remain explicitly unproven. Membership proves neither chair-session ownership,
successful launch nor workflow authority.

`WorkSelector` requires project and graph, with optional node, attempt,
repository and head constraints. Reads check at most 64 candidates and 64 KiB
per record, using the same byte bound as writes. `Verified` requires a node or
attempt; graph-only matches are at best `Partial` because there is no trusted
complete member list. Missing nested and unprepared work, including an empty
graph-only selection, returns `Unindexed`. Contradictory
facts return `Mismatch`, ahead of unavailable repository/head facts and unindexed
coverage, including graph-only selectors. A mismatch observed among the first 64
candidates also wins over `Limit`; otherwise `Limit` never proves absence. Lookups traverse no child roots and accept
no caller-supplied store locator. Configured grants cannot expose the host store,
its companions or lock. Membership is retained with the attempt row.

- Why (default): caller claims cannot establish membership or hide a known contradiction,
  and a waiting index reader must not block unrelated Fabric writers.
- Test: `crates/run/src/correlation.rs::tests`;
  `crates/cli/src/run/provider/confinement_test.rs::configured_grants_cannot_expose_host_indexes`
- Owner: `crates/run/src/correlation.rs`, `crates/run/src/launch.rs`

## Chair associations

Local CLI submissions seal a chair's host and Fabric inbox actor with
the request, and selection matches session discovery.
This actor-scoped association identifies no particular live chair session and
does not change attempt ownership. Worker provider sessions never establish it.

The Fabric `graph_chairs` index in `<state>/fabric/host.sqlite3` is
authoritative, keyed by project and graph name, as
[Fabric](fabric.md#chair-bindings-follow-graph-names) states. This database and
its companions are outside the exact coordination database write grants lanes
receive. The host seals a graph's binding at its first launch, from the host's
immutable request, before any provider starts. Every later attempt in the graph
resolves that binding: a request naming no chair inherits it, and one naming a
different chair is refused. A continuation inherits its parent's graph binding,
reading the parent's graph from the host store; a missing parent binding yields
an unbound association. Lane-local requests, a lane-local Fabric database and
the shared mail database cannot replace an association. Index failures or
exposing write grants refuse launch. Replaying a root submission (the same
`run --name`) repairs a missing binding from the immutable host request,
including owned-worktree replay; lane stores cannot repair it, and a replay
naming a different chair is refused. Transient index errors leave queued
children available for a later scan.

The binding is sealed before handoff, so presence in the index does not imply
that an attempt launched. Launch validation reads
`fabric-runtime::chairs::ChairIndex`; for reads, a graph with no record is
unbound. `console board` reads existing host associations without initialising
the index. Provider handles and child requests are never fallbacks. Board and
tree rendering are separate.

Association rows live as long as graph metadata. GC removes provider payloads,
logs, events and worktrees; it never removes attempt metadata. Whole-graph
collection must delete its association in the same host-owned removal workflow.
Lane-writable missing child records alone cannot authorise deletion.

- Test: `crates/fabric-runtime/src/chairs.rs::tests::associations_are_keyed_by_project_and_graph_and_inherit_by_name`;
  `crates/run/src/service_tests.rs::chair_binding_replays_the_original_submission`;
  `crates/cli/src/run/tests.rs::operation_replays_after_its_context_ref_moves_or_is_deleted`;
  `crates/run/src/dispatch_lifecycle_tests.rs::nested_continuation_keeps_the_host_attempt_id_and_replays`;
  `crates/run/src/dispatch_lifecycle_tests.rs::nested_continuation_preflight_does_not_reuse_crashed_staging`
- Owner: `crates/fabric-runtime/src/chairs.rs`, `crates/fabric-store/src/chairs.rs`,
  `crates/cli/src/run.rs`, `crates/cli/src/fabric/graph_relay.rs`

## Atomic admission

Writer custody is exclusive per verified worktree, including retained-import
fences across run-state roots. Git probes, scans, seeding and provider preparation
must not hold a shared writer registry lock. Repository mutation locks protect
creation/removal; they do not serialise independent writer claims. Contention
waits until the admission deadline, never an unrelated retry-count limit.

Admission and the attempt rows commit together; the immutable request is
published before acknowledgement. The store refuses a second row for a work key
inside the same transaction. Transactions contain no launches, probes,
waits or supervisor calls. Optional total and writer-specific host/project caps
are independent. Build slots gate compiling Cargo commands rather than idle
writers. Configuration and bounds belong to `provenant config schema --all`.

Known kernel memory pressure may defer either class. Low available memory alone
is advisory. The disk floor holds lanes with write authority, including prepared
tool/project grants; private bookkeeping/scratch alone warns and proceeds.
Expiry reports the actual limiting condition and configuration source, with disk
refusal taking precedence over pressure for writers. Invalid parsed settings
fail immediately; unreadable configuration retries rather than admitting with an
unknown disk policy.

Host leases bind the boot, guardian identity and owner-store identity. A changed
boot or proven absence releases a lease; busy, replaced, corrupt or unreadable
state retains it until independent custody evidence permits release.

Startup spacing is independent for writer and read-only classes. Preparation
promotes a ticket that gains write authority. The host monotonic clock and boot
identity are read inside the ledger transaction, so lock order is clock order;
wall-clock adjustments cannot shorten spacing. Reboot clears prior-boot spacing.
An unreadable clock/boot identity refuses the claim. Spacing constrains admission,
not eventual provider spawn. Each launch admits exactly one attempt.

Status derives queued waits from the owner store and ledger, including over peer
reads. Unknown observations remain unavailable. A running guardian's observed
silence may return a wait early with the condition unmet; it never becomes
settlement or changes the execution state. Resume of progress clears that
observation. The [admission owner](../../tools/front-door/crates/run/src/ledger.rs)
owns lease persistence and defaults.

- Why (default): an accepted receipt must mean durable admission, and read-only lanes
  must not queue behind writer spacing that exists to pace worktree and
  provider startup.
- Test: `crates/run/src/ledger.rs::live_guardian_fences_busy_replaced_and_corrupt_store`;
  `crates/run/src/ledger.rs::cross_boot_malformed_and_readable_row_removal_release`;
  `crates/run/src/dispatch_core_tests.rs::pressure_errors_and_low_advisory_memory_do_not_refuse`;
  `crates/providers/src/lane.rs::claude_shell_restores_jobs_after_scrub`;
  `crates/os/src/build_slots.rs::slots_are_bounded_and_reusable`;
  `tests/build_slot.rs::more_builds_than_slots_compile_serially`
- Owner: `crates/run/src/admission.rs`, `crates/run/src/ledger.rs`, `crates/os/src/sys/clock.rs`

## Lane priority

The host selects a configured nice value for each lane: 10 for writers, 5 for
read-only lanes and 15 for build-heavy lanes by default. The guardian applies
it to the provider before confinement; the provider's descendants inherit it.
An inherited higher nice value is preserved. Failure to read or change priority
must never refuse a lane: launch proceeds and the reason is recorded. Launch
evidence reports the applied value when known and the requested value when it
differs.
- Why (default): lanes should yield CPU without requiring new privileges or preventing
  nested work from starting.
- Test: `crates/supervisor/src/process_os/provider.rs::provider_and_descendant_inherit_priority`;
  `crates/supervisor/src/process_os/provider.rs::priority_failures_degrade_and_preserve_evidence`;
  `crates/run/src/tests.rs::lanes_read_last_output_time_from_supervisor_checkpoint`;
  `crates/cli/src/run/tests_submission.rs::cancel_and_reconcile_report_leftover_paths_and_reasons_without_terminal_controls`
- Owner: `crates/supervisor/src/process_os/provider.rs`, `crates/run/src/launch.rs`

## Immutable attempts, no writer replay

Terminal attempts are immutable; a retry or continued turn is a new attempt
linked by `retry_of`, and the latest is the highest attempt number, never
the latest timestamp. Read-only fallback needs explicit opt-in and a typed
transient failure. A writer (an attempt that may edit files) is never replayed
over its own edits: only the graph's stop decision restarts it, as a fresh
attempt that continues in the same worktree once custody is released, and two
writers never share a worktree.

Every admitted attempt carries its `WorkKey`: a first attempt names it in its
task context and a launch that names none is refused, so the store never
derives one. A continuation takes its parent's graph and node with the next
attempt number, and each terminal attempt is continued at most once.
- Why (default): evidence must not be rewritten, and replaying a writer clobbers edits.
- Test: `crates/run/src/dispatch_lifecycle_tests.rs::resume_continues_a_terminal_attempt_once_with_its_route`;
  `crates/fabric-runtime/src/graph/graph_delivery_and_routes_tests.rs::writer_limit_after_launch_restarts_in_place_on_the_second_route`
- Owner: `crates/run/src/store.rs`, `crates/run/src/launch.rs`

## Publication order

1. One transaction reserves the operation, digest, IDs and tickets.
2. The scheduler samples resources outside the transaction, takes writer custody
   and passes the plan to supervisor. A lost acknowledgement records
   `launch_unknown`, keeps capacity and the reservation, and is never replayed.
3. Supervisor observes provider exit and releases host build capacity. Once
   cleanup proves quiescence, it retains the worktree writer lock through settlement.
   Read-only attempts may release their attempt-local lock.
4. Settlement validates exit and decision facts, then seals the result and scoped
   decision before acquiring `publication.lock`. The transaction commits or
   replays those exact bytes. Applicable effects are driven from durable
   receipts, and custody is released only after verified import completion or
   the existing explicit refusal/recovery path.
5. A run closes only when every task and registered child is terminal.

`settlement-progress.json` binds the attempt scope, pinned branch base and head,
current phase, native process owner, retry deadline and one import attempt counter. The last settlement error is retained in `blocking_reason` and reported by reconcile in text and JSON; advancing a phase clears it.
Each driver holds a kernel lease for its lifetime. A concurrently live driver
excludes a second claimant even in the same process. A claimant holding the
lease can disregard an abandoned same-process journal owner after the existing
retry deadline, including when Drop could not lock, read or write the journal.
An unknown process observation keeps the recorded owner active until the ten-minute
uncertainty bound expires. Refusal observations distinguish journal contention,
live ownership and the retry deadline. The replacement continues the recorded
phase without restarting the provider or decoding its output again. Journal
locks cover only short reads and durable updates; they never span an external
command. Terminal effects have a separate kernel exclusion. The worktree
writer flock spans import and publication, so lifecycle mutations cannot race
terminal settlement.

| Phase | Durable effect and replay |
| --- | --- |
| `import` | Import the pinned head through Git's quarantined receive path. Repeating the same staging ref is harmless. |
| `publish` | Compare-and-swap the real branch from its pinned base; the same head or a descendant already published counts as success, without moving the branch. |
| `record` | Seal the result and terminal decision before acquiring the publication lock and committing the attempt, summary, operation state and event. A lock timeout leaves the exact scoped decision replayable. Retention-ref cleanup follows the transaction outside the publication lock. |
| `release` | Verify the committed outcome and release evidence, then remove the settling reservation. Repeated release accepts the same committed attempt. |
| `complete` | Settlement is finished; ordinary terminal cleanup may reclaim disposable scratch. Import refusals retain private Git for host recovery. |

A worktree-write lane edits the bound worktree's files while its commits stay
in private Git, so until publication the files are ahead of HEAD and the index.
When settlement cannot publish, it moves back every file that still holds the
lane's own content to the branch the host holds, keeps any other file as found,
and names the lane's commit and branch in the settlement reason. A later
successful publication moves those files forward again, so files, index and HEAD
always describe one commit.

A chair can release an unsettleable writer fence with
`provenant reconcile --abandon NAME/NODE#N --reason TEXT`. Ordinary reconcile
continues retrying by default. A never-dispatched queued attempt can also be
abandoned when it has no process evidence, writer fence or worktree custody;
it is withdrawn as cancelled before recording abandonment. Other abandonment
requires a terminal attempt,
exclusive settlement and writer locks, and proven absence of custody. It records
an explicit abandoned settlement outcome with the chair, time, reason and prior
phase before releasing the reservation. It preserves the last error, provider
output and private Git, and excludes the attempt from automatic recovery and
retention cleanup. Repeating the command with the same reason completes an
interrupted release; a different reason refuses to replace the evidence.

Settlement permits three attempts that reach object import. Contention and
pre-import checks do not consume this counter; resuming a later phase does not
consume another import attempt. Import claims back off for 5 seconds, 30 seconds
and 2 minutes instead of retrying immediately. Exhaustion records explicit
import-refusal evidence naming the limit and
retained private Git. If an import fails before writer facts can be verified,
the validated terminal decision is `failed`, with a retryable `GitImport`
failure. Its private inputs, reservation and custody fence remain retained.
A retry can complete the import receipt or reach explicit refusal, but cannot
upgrade or replace that sealed outcome. When reconcile proves the guardian is
absent, it finalises a failed import receipt without retrying publication,
releases custody while preserving the private Git and lane evidence, and clears
its cleanup debt. This also applies to already sealed unsettled decisions. Status
and output project the final import receipt as failed,
keep the import error as its cause, and report the pinned head and retained
object directory when available. The sealed decision stays unchanged.
Rejected objects and failed publication
never certify successful writer publication. A pending terminal commit remains
`settling`
with `publication_pending: true` and keeps the worktree reservation, while its
provider has already stopped consuming host build capacity and live custody.

An import failure preserves an already established failed, timed-out or lost
provider outcome and its reason. A potential success with unverified writer
facts becomes `failed`; even after import recovery it requires a fresh attempt
or manual host recovery to obtain success or delivery eligibility. Older sealed
`unsettled` decisions remain immutable and project as failed through the final
import receipt.

Unresolved imports also seal a scoped `import-retained.json` marker beside the
custody reservation. Run checks predecessor markers after acquiring the normal
writer lease, while holding `writer.lock`, and withdraws a new unlaunched lease
on refusal. This protects shared registries across different run-state roots
after the original recovery process exits. Predecessors without an import marker
keep their existing admission semantics. Reservation release retires the marker's
active fence; its evidence may remain beside the retired reservation.

If launch preparation fails after acquiring custody, it withdraws the
unlaunched reservation before reporting a local refusal. A failed withdrawal
keeps the reservation and durably blocks the operation with the preparation
and withdrawal causes. A reservation publication whose durability is uncertain
also blocks; an unconfirmed guardian handoff remains launch-unknown. Neither
case authorises a route switch. If the blocked-operation write fails, the
storage error is returned and the reservation stays in place.

An attempt record version 2 carries the terminal evidence and distinguishes
`unsettled` from `lost`. Effect receipts are stored independently of the
immutable decision. A terminal wait view includes `custody_pending`; a pending
fence still prevents unsafe writer admission. A cancellation determines the
execution outcome only when delivery to the provider was recorded before its
exit was observed. A later request is acknowledged as ineffective. A current exit
record can explicitly mark delivery ordering unobserved, which retains uncertainty;
a missing ordering member is invalid and keeps custody unresolved.

A verified clean provider success is preserved when the current guardian explicitly
marks cancellation ordering unobserved; the cancellation receipt remains `unknown`. A host-observed
cancellation during Git import has separate `import_cancellation` evidence and
an `import_effective` receipt: it does not invent delivery to the provider before
exit. Cancellation during delivery leaves the terminal decision unchanged and
records `delivery_effective` plus a retryable cancelled delivery effect. Its start
marker remains durable; automatic recovery cannot duplicate an uncertain remote
effect, while explicit host pickup may retry it. A cancelled delivery does not
seal a permanent `delivery.json` failure.

Deferred effect dispositions are sealed in scoped `terminal-effects.json`
before publication and inserted with the terminal transaction, including on
replay. Post-commit receipt-write failures cannot withhold the committed decision
or skip temporary cleanup. Admission is freed when settlement starts, including
for upgraded guardians without the exit hook. Delayed Git cleanup always removes only its own attempt-scoped base pin.
It removes a head-keyed import pin only while holding an idle writer slot with
no competing predecessor reservation; a late retry cannot delete an active
lane's staging evidence.

The decision is sealed (`terminal-pending.json`) before the revision-checked
transaction and replayed unchanged after a crash or `SQLITE_BUSY`; one decision
exists per attempt generation and a conflicting sealed decision is kept beside
it as `terminal-pending.conflict-*.json`, never replacing it. Lost recovery
converges on any scoped sealed decision instead of overwriting it with `lost`.
Git import, manifest sealing, remote delivery and cleanup/custody release are
independent dispositions (`pending`, `done`, `failed{reason,retryable}`).
Initial failures are sealed with the intent and created transactionally as
receipts; subsequent failures update only receipts. Reconciliation discovers
applicable pending and retryable effects even after temporary-data cleanup and
restart. Intended manifest bytes are sealed separately as
`quarantine/manifest-intended.json`; a conflict stays visible and its evidence
is never overwritten. Recovery seals or confirms the manifest only when it
matches that intended disposition. Delivery reuses the completed receipt or
start marker. A start without a known result remains uncertain and is never
automatically repeated. An unstarted delivery uses the sealed writer head and
pinned base and verifies the current worktree before starting. A failed Git pin
cleanup is recorded as non-retryable in the effect ledger; retained temporary
data remains discoverable for reconciliation. Git pin cleanup requires the
writer lock. The internal finish result
distinguishes retained custody from permission to release; live, recovered and
landed-writer paths all enforce that distinction. A stop accepted between the last cancellation read and
the commit is acknowledged ineffective from the host-sealed request after the
commit; a lane-rewritable record never decides whether a held handle stops.

Every SQLite store (`run`, `admission`, `work-index`, `pr-contributions`,
the workflow log, and Fabric's `fabric` and `host`) is created from one schema
file at one version. A store at any other version, older or newer, is refused
with `STATE_RESET_REQUIRED`, which names `install --reset-state`, and its bytes
are left unchanged. There is no migration, snapshot or older-shape reader.

`stop` durably records the request and signals the supervisor without waiting
for a writer lock. The settlement driver checks it between phases and polls it
every 50 ms during import and optional automatic delivery,
stopping the owned Git child when required.
Public `reconcile` starts the same binary's detached `internal settle` worker
only when a non-terminal attempt needs settlement. A run-scoped queued claim
and worker exclusion deduplicate concurrent requests; a queued claim expires
after ten minutes if no worker starts. Reconcile promptly reports pending
settlement. Neither CLI waits for object transfer
or an existing settlement owner; the supervisor owns this work after return.
- Why (default): each phase records enough evidence to resume without holding a writer
  lock through Git, and cancellation must remain available during import.
- Test: `crates/run/src/settlement.rs::tests`;
  `crates/run/src/objects_tests.rs::cancellation_kills_a_blocking_import_promptly`;
  `crates/run/src/tests.rs::publication_crashes_preserve_committed_records_and_allow_retry`;
  `crates/run/src/tests.rs::prelaunch_refusal_publishes_durable_never_launched_evidence`.
  No run-level test asserts that `launch_unknown` is never replayed (gap).
- Owner: `crates/run/src/settlement.rs`, `crates/run/src/holder.rs`,
  `crates/run/src/launch.rs`, `crates/run/src/store.rs`

## Success needs proof

Success needs a supervisor-observed exit 0, a matching provider identity and
a complete, validated, non-empty result; capture bytes are never completion
evidence. A dead owner is not a failed provider. A writer with only uncommitted edits ends `uncommitted` and does not deliver,
unless those paths are review-exempt. Paths in the project's
`[domains.run] review_exempt` list (release-note fragments, specs, lock files
and `.tracker-input` by default) never count toward either, and Git metadata,
hooks and provider control roots can never be listed. Run reads no provider
transcript except Codex's post-exit identity projection. Sandboxed Codex
projects the exact stream thread from its candidate-local rollout. Native
Codex uses the launch-time home selector in the bounded
`provider-state/codex-observation-candidate-N.json` record, outside the
lane-writable candidate subtree. Settlement reads that same home and
requires the same exact thread, including after recovery. A read-only lane (no
`writer`) whose complete, validated, non-empty answer is present settles
`succeeded` without a handoff; only a writer must also report one.
- Why (default): a crash, truncation or impostor must never read as success.
- Test: `crates/run/src/dispatch_custody_tests.rs::publication_holder_commits_decoded_success_only_with_exit_zero_and_observed_identity`;
  `crates/run/src/dispatch_settlement_tests.rs::a_read_only_answer_without_handoff_settles_succeeded`;
  `crates/run/src/tests.rs::protected_review_retains_candidate_and_paths_without_delivery`;
  `crates/confinement/src/git_evidence.rs::exempt_paths_left_uncommitted_do_not_count`
- Owner: `crates/run/src/holder.rs`, `crates/run/src/writer.rs`, `crates/confinement/src/review_exempt.rs`

## A partial finish is visible

An attempt that settles `succeeded`, `review_required` or `uncommitted` carries
`partial_signals` when there is structured evidence that part of the brief is
undone: `declared_stop` (its final handoff, defined below, is a valid `handoff`
schema v2 block with status `stopped_early` or `blocked`),
`provider_stop` (the provider reported a stop reason other
than a finished turn, such as Claude's `stop_reason` `max_tokens` or a
`terminal_reason` other than `completed`), `uncommitted_work`, and
`handoff_missing` (a writer on a model route, meaning any route other than a
model-less adapter such as `command`, settled without a valid final handoff;
a read-only lane never carries it). A failed
lane never carries `handoff_missing`; a redaction failure flips the outcomes
that would into `failed`. Prose is never read, and a quoted, repeated or
trailing-text block is no flag.

The final handoff is one `handoff` fence with nothing after it in the turn it
ends. A decoder that sees provider turns records where each completed turn
ends in the result (Claude: each `result` event) and whether a tool ran in
that turn; a plain text user message, such as a background task notification,
is not tool work. `Handoff::from_turns` (`crates/records/src/handoff.rs`) reads
the turns from the last: a later turn's own valid or malformed fence decides;
a later turn with neither a fence nor tool work, such as a reply to a late
notification, leaves the earlier turn's handoff standing; a later turn that ran
a tool without declaring leaves no handoff. A fence quoted or drafted mid-turn
never ended its turn and is not final. Without turn boundaries, or with text
that no turn ended (an appended diagnostic, an interrupted answer), the whole
answer must end with the fence. Settlement and the `handoff_missing` check share
this one rule. Review verdict and findings fences keep their final-position
rule over the whole answer.

A declared stop (`stopped_early` or `blocked`)
and `handoff_missing` both change an otherwise `succeeded` outcome to
`review_required` (the latter with the reason "no handoff: the lane ended
without reporting results"); other outcomes remain unchanged. Every terminal
record, including a refusal before launch, carries a `settlement_reason`
(`completed`, `handoff_missing`, `declared_stop`, `provider_stop`,
`uncommitted_work`, `pre_launch_refusal`, `cancelled`, `lost`, `unsettled`,
`failed`), derived once where the terminal decision is committed
(`RunStore::commit_terminal`, or `commit_lost` for an attempt whose record was
lost). Lane rows expose that settlement classification as `cause`. When a
specific typed terminal cause was recorded, `terminal_cause` identifies it and
structured `reason` fields retain the full safe cause, remedy and note in every
lane response. A writer's declared stop with no new commit records
`terminal_cause = "stopped_no_work"`: settlement compares its pinned base and
effective committed head, keeps `cause = "declared_stop"` and `state = "review_required"`, and says
that it stopped with no work and acceptance remains unmet. The result output reports the settlement base and committed head separately from changed paths and publication; unavailable evidence remains unavailable. The residual tree state is a current observation only when the worktree still names that head; otherwise it is unavailable. Missing Git evidence,
uncommitted edits and other failure causes do not receive this classification.
Only human `status` rows use short labels pointing to
`provenant output ID`. Pre-launch causes distinguish an
unsupported platform, an unknown pacing account, an unregistered selected
account, a credential-helper failure at delivery Prepare, a configured command
that cannot be resolved, an unconfigured host-native route and a provider
identity that was not observed. A terminal record may also carry a
`terminal_note`, a bounded sentence Provenant composes from fixed text and typed
values: the configured executable of an unresolved command, or a policy retry
decision such as "retrying this session (attempt 2 of 2)". Structured listings,
launch replies and output retain that note and the full remedy. Human row labels
omit them. Disk-wait observations remain available before a result exists.
These projections never copy raw preparation errors or provider output. A refused resume retains
the specific preparation reason without consuming a continuation or creating a
new run.

A lost or unsettled outcome names its reason whatever evidence it
carries; the process may have run even though custody was not proven. A failed, cancelled or lost
lane whose final answer declares a stop records `declared_stop` (with any
provider stop it already reported) and no other partial signal. The settled outcome is
what the graph reads: a settled `review_required` attempt stays visible while
its graph node succeeds and dependants proceed. It needs no acknowledgement;
any acknowledgement is bookkeeping and grants or releases nothing. Explicit
unfinished writer handoffs still follow the stop policy: committed `done`
succeeds, `blocked` preserves the lane's stated cause, and no work without a
handoff stays `stopped_no_work`. Result integrity and review contracts still
apply before a node succeeds.
Delivery is gated on the outcome before the stop, so a lane that succeeded
before stopping still delivers its partial work once its publication is
durable. The reason carries a `partial finish (...)` note, and `status` shows
`(partial)` beside the state, the signals, and a `provenant retry` hint. Every
launch prompt asks for the same final `handoff` block on every finish. Its JSON
has `schema_version: 2`, `status` (`done`, `stopped_early`, `blocked`), `note`,
`remaining[]`, `decisions[]` and `verify[]`. Unknown fields are retained. The
JSON is bounded to 12288 bytes, the note and each list entry to 4096 bytes,
and each list to 64 non-empty strings.

The launch prompt instructs a lane to atomically update `handoff.json` in its
private provider state when the plan has a private-state grant. The update is
best effort; a provider may refuse it. This path is outside the worktree and commit set. At
settlement the host saves the valid final handoff as the attempt's immutable
`handoff.json`. If the provider dies without a final fence, the host reads
progress only from the exact private-state grant of the applied candidate.
A completed malformed answer never falls back to stale progress; an interrupted
final fence uses the last valid progress snapshot. Missing, oversized,
linked or malformed progress files supply no handoff and do not block
settlement. Known secret shapes are redacted from every snapshot field,
including forward fields, before retention or reuse; composed retry prompts pass the disclosure
scan. The host snapshot survives provider-state and temporary cleanup.
Recovery and fresh manual continuation carry this bounded snapshot with the
original brief and worktree state, rather than a result transcript. A handoff
declares `stopped_early` when work remains; `blocked` needs a decision or
external change. A run never restarts itself: what follows a stop is the
graph's, under [What follows a stop](#what-follows-a-stop). A graph writer's
declared stop, whether the writer committed, left nothing or left uncommitted
edits, restarts the node in place on its retained worktree with its edits
intact. The restart's brief carries the original brief and the handoff, and a
pool draw excludes previously used models. A dirty settlement without a
declared stop still fails to the chair. A `blocked` handoff means an owner
decision, a missing capability or an external change and never launches a
successor. The `decisions[]` array records decisions already made; its
presence alone does not block a restart.
- Test: `crates/fabric-runtime/src/graph/retry_tests.rs::stopped_writer_restarts_fresh_on_another_route_with_its_handoff`;
  `tests/graph_provider.rs::graph_writer_restarts_fresh`;
  `crates/fabric-runtime/src/graph/retry_tests.rs::stopped_writer_without_another_route_returns_to_the_chair`;
  `crates/fabric-runtime/src/graph/retry_tests.rs::writer_owner_decision_or_missing_capability_returns_to_chair_without_launch`;
  `crates/fabric-runtime/src/graph/plan_tests.rs::declared_writer_stop_restarts_with_handoff_and_excludes_the_used_pool_model`;
  `crates/fabric-runtime/src/graph/plan_tests.rs::pending_writer_continuation_reopens_and_excludes_its_predecessor_model`
- Why (default): a lane that did part of its brief must not read as one that did all of it.
- Test: `crates/run/src/dispatch_settlement_tests.rs::a_declared_stop_settles_review_required`;
  `crates/run/src/completion.rs::tests`;
  `crates/providers/src/tests_stream_outcomes.rs::claude_success_that_stopped_for_another_reason_is_an_early_stop`
- Owner: `crates/run/src/completion.rs`, `crates/run/src/holder.rs`

## A writer stops on a token budget

A writer's fresh-token target defaults to 1,000,000 (0 disables it). Cache
reads do not count. Each field resolves independently, nearest first: run
(`--set attempt.token_budget=N`, `--set attempt.on_budget="warn"`,
`--set attempt.budget_grace=0.10`), template attempt fields, tier,
project, host, then the compiled default. `lanes.token_budget`,
`lanes.budget_action` and `lanes.budget_grace` are the harness knobs. Host and
project domain settings are `writer_token_budget`, `writer_budget_action` and
`writer_budget_grace` under `domains.run` and `domains.run.projects.PROJECT`;
`domains.run.writer_budget_tiers.TIER` supplies `token_budget`, `on_budget`
and `budget_grace` for a routing tier. `run --explain` and the attempt's status
limits retain the winning source for the target, action and grace separately.
Sealed records carry grace as exact decimal text (`"0.1"`), because canonical
records hold no floats; configuration accepts the fractional `budget_grace` value.

The default action is `wrap_up`, with grace 0.10. On a crossing while work is
dirty, the host ends the provider invocation and resumes the same provider
session once through the existing resume path. The short continuation prompt
asks the writer to commit, push and report, and states its remaining fresh-token
allowance: target × (1 + grace), less fresh tokens already spent. The resumed
attempt has a hard allowance and cannot request another budget wrap-up.
Admitting the successor precedes custody release; launching it follows release
through the ordinary settlement reaction, including recovery replay. Graph
observations follow that successor. No live steer or fresh graph restart is
claimed. If the reported usage has already exhausted grace, the hard stop
applies immediately. Provider turns may therefore overrun the target or grace.

`warn` records the crossing on the attempt, exposes it in status and notifies
the submitting chair when one is bound; it never stops the lane for tokens.
`stop` preserves the hard-stop behaviour for dirty unfinished work. At either
hard boundary, the typed stop is `token_budget` and an interrupted attempt
settles `timed_out`. After custody proves the provider absent, the host attempts
a bounded snapshot of remaining edits and publishes the resulting local branch.
A Git operation in progress is preserved, and a failed or skipped snapshot is
reported without replacing the budget outcome. The host does not push a budget
snapshot to a remote.

Natural completion takes precedence over every action. A clean point, meaning
no uncommitted paths and a locally recorded upstream equal to the effective
lane head, stops at that boundary using the lane's recorded handoff and
outcome. A recorded done handoff keeps completion; an incomplete report follows
ordinary review settlement. Neither case creates a budget snapshot or
reclassifies completion as `timed_out`. Unknown
Git state is not proof of a clean point. The first wrap-up boundary preserves
edits without a snapshot and settles as a resumable intermediate outcome; the
successor's natural completion is the lane's result.

The guardian counts one fresh usage figure per provider call. Claude reports
live usage; Codex reports completed-turn usage and cannot be interrupted by
tokens mid-turn. Providers without live usage get an advisory target only.
At 90% the host records the warning through the existing control capability
matrix; current lane adapters cannot receive that warning as a live steer.
Time queued for a build slot stays off the wall deadline, up to
`lanes.slot_wait_seconds` (default 7200) in total.
- Why (default): preserve a writer's valuable wrap-up while bounding extra spend.
- Test: `crates/supervisor/src/guardian/tests.rs::budget_actions_preserve_clean_points_and_natural_completion`;
  `crates/run/src/limits.rs::every_budget_field_resolves_independently_and_zero_remains_an_override`;
  `crates/run/src/holder.rs::budget_wrap_up_resumes_once_with_remaining_grace_and_natural_completion_wins`;
  `crates/run/src/holder.rs::writer_token_budget_stop_commits_uncommitted_work_and_publishes_branch`;
  `crates/run/src/token_budget.rs::fresh_tokens_count_towards_the_budget_and_cache_reads_are_ignored`
- Owner: `crates/supervisor/src/guardian.rs`, `crates/run/src/token_budget.rs`,
  `crates/run/src/holder.rs`, `crates/run/src/limits.rs`

## A writer hands off at a context threshold

A writer can be restarted fresh before its context grows costly, because cost
is context length multiplied by turns. `lanes.context_handoff_tokens` (default
0, off; suggested 200000) sets the context, in tokens one provider call sent
(fresh input, cached reads and cache writes), at which the guardian stops a
writer with the typed stop `context_handoff`. Only a provider that reports each
call's context is stopped: Claude reports it on every assistant message; a Codex
turn sums several calls, so it reports none, and providers without live usage
report none. A lane with no context figure keeps today's behaviour. The attempt
settles `review_required` with `terminal_cause: context_handoff` and a declared
stop. Its result carries the lane's latest progress handoff, marked
`stopped_early`. A done claim in that handoff loses its status, and a missing
one becomes a stopped-early handoff naming the gap. The host commits nothing;
commits and uncommitted edits stay on the worktree. The graph then restarts
the node in place, fresh, on the same worktree with that handoff in its brief,
on another eligible route as for any stopped writer; each restart spends the
graph's continuation budget. A dirty-work budget boundary takes precedence
when both are due. Budget wrap-up resumes the same provider session and is
separate from this fresh context handoff. During its bounded continuation the
context threshold is deferred so a fresh restart cannot reset the remaining
allowance; `warn` never requests a token stop.
- Why (default off): whether a fresh restart keeps enough working state is
  untested, so restart stays off until a trial shows it does not lower
  completion.
- Test: `crates/supervisor/src/guardian/tests.rs::context_handoff_is_due_only_at_a_reported_context_past_a_set_threshold`;
  `crates/providers/src/claude.rs::live_usage_counts_each_call_once_with_fresh_input`;
  `crates/run/src/holder.rs::writer_context_handoff_stop_settles_as_a_declared_stop_with_its_handoff`;
  `crates/fabric-runtime/src/graph/retry_tests.rs::stopped_writer_restarts_fresh_on_another_route_with_its_handoff`
- Owner: `crates/supervisor/src/guardian.rs`, `crates/run/src/holder.rs`,
  `crates/fabric-runtime/src/graph/next_step.rs`

## Host inputs fail before launch

A host failure to read the stored prompt, or to stage or read back a context
file, ends the attempt `rejected` before launch with its cause. `--base`
context is captured on the host with fsmonitor, external diff, textconv,
mailmap and fetching disabled, limited to 10 s per read and 1 MiB per file; a timeout keeps
partial output and is recorded. The capture holds the log, the whole patch,
a `--stat` summary and one patch slice per changed file (at most 400, in
`git-diff-slices/` with an `index.txt`), and the prompt points a reviewer at the
stat and the slices before the whole patch. When `slices_complete` is false the
prompt says so and lists the changed files with no slice. Reviewers must report inputs they
could not read, truncated captures and `slices_complete: false`.
- Why (default): repository config is hostile input, and a lane must not run on input
  the host could not verify; slices are navigation aids into the patch, and a
  reviewer still reads the surrounding code in the checkout.
- Test: `crates/run/src/dispatch_custody_tests.rs::missing_stored_prompt_ends_the_attempt_rejected_with_the_cause`;
  `crates/run/src/git_context.rs::git_context_pins_hardening_flags_and_writes_prompt_paths`;
  `crates/run/src/git_context.rs::slices_cover_every_file_and_the_index_names_them`;
  `crates/run/src/git_context.rs::slices_stop_at_their_limit_and_say_so`;
  `crates/run/src/git_context.rs::an_incomplete_prompt_names_the_changed_files_without_a_slice`;
  `crates/run/src/git_context.rs::real_git_stat_and_slices_match_the_whole_patch`
- Owner: `crates/run/src/git_context.rs`

## The pinned source view names its failure

An isolated review lane reads a pinned view of the subject commit,
built in the attempt's temporary directory from the shared object store: one
`ls-tree` listing, blobs read through `cat-file --batch` in groups of at most
32 MiB and 4096 objects, then a synthetic index. The subject need not be the
checked-out HEAD of a linked worktree, and may be a full SHA or a
remote-tracking ref resolved to one. One deadline bounds the whole build; no
process gets a smaller fixed share.

Every failure ends the attempt `rejected` with `terminal_cause =
"source_view_refused"` and a note of the form `pinned source view failed at
STEP: CAUSE`. STEP is one of `repository check`, `tree listing`, `tree
validation`, `object read`, `temporary directory` and `index build`. CAUSE is
fixed-form text: timed out, which stream exceeded its output limit, the Git exit
status or signal, a named refusal (unsupported entry, unsafe symlink, a limit),
or a short description of an internal failure. It never carries Git output, file
contents or credential values. `status` and `output` show the note, and the
public text does not point at route or account configuration.

Limits are `domains.run.source_view_seconds` (default 300),
`source_view_entries` (100000) and `source_view_bytes` (512 MiB). One file is
limited to 60 MiB, which is not configurable because it is the single-read
capture ceiling.
- Why: a refusal that discards its cause sends the reader to the wrong place,
  and a process per blob made a large tree exhaust a fixed time share.
- Test: `crates/cli/src/run/provider/tests_source_view.rs`
  (`source_view_builds_from_a_linked_worktree_behind_the_subject`,
  `source_view_names_the_failing_step_and_cause`,
  `source_view_of_twenty_thousand_files_uses_batched_reads_within_budget`,
  `source_view_batch_reader_decodes_in_order_and_names_missing_objects`)
- Owner: `crates/cli/src/run/provider/source_tree.rs`

## Host-native launch rows

The optional `host-native` adapter selects a named
`domains.fabric.host_native` entry. Its `AdapterSpec::launch` row refuses task
model and effort selectors and session resume; model and effort are configured
on the entry. `claude-agent` is the first kind and delegates preparation to
Claude before adding its inline `--agents` definition and `--agent` selector.
The same provider admission, accounts, network/access confinement, prompt
transport, decoder and writer delivery apply. Applied route provenance retains
the entry name and configuration source alongside the actual provider route.
Host-native attempts retain model-policy result refusals but do not automatically
retry as ordinary model routes or resume a session. Explicit resume may fall
back to a fresh named launch under the existing resume policy.
- Why (default): optional host features must declare their lane contract and keep its
  confinement and provenance through execution and recovery.
- Test: `crates/records/src/adapters.rs::host_native_declares_a_fresh_named_writer_contract`;
  `tests/graph_provider.rs::host_native_preparation_uses_the_provider_contract`
- Owner: `crates/records/src/adapters.rs`, `crates/cli/src/run/provider/mod.rs`

## Launch prompts

Launch prompts are bounded to 1 MiB. Argv adapters take up to the
`domains.run.argv_prompt_limit_bytes` threshold (64 KiB by default); a
larger top-level prompt is sealed as `prompt.md` and replaced by a pointer to it, while
nested argv attempts refuse it because the parent lane can write their
directory. Prompt digests cover the full text in `prompt.txt`.
- Why (default): a spill file a lane can rewrite would change its child's task.
- Test: `crates/cli/src/run/provider/tests_provider_auth_setup.rs::launch_prompt_spills_only_top_level_argv_and_refuses_conflicting_file`;
  `crates/cli/src/run/provider/tests_provider_auth_setup.rs::nested_argv_prompt_respects_configured_byte_limit`;
  `crates/cli/src/run/provider/tests_provider_auth_setup.rs::launch_prompt_refuses_symlinked_attempt_ancestors`
- Owner: `crates/cli/src/run/provider/observation.rs#launch_prompt`

## Read-only write exceptions

`projects/<ProjectId>/read-only.json` may grant read-only runs write
exceptions for existing directories, given relative to the verified repository
root.
Absolute paths, traversal, symlinks and Git or provider configuration are
refused, and the attempt records every granted path.
- Why (default): only the user widens a read-only lane.
- Test: `crates/confinement/src/plan/read_only.rs::policy_defaults_and_refuses_escape_control_paths_and_symlinks`;
  `crates/run/src/dispatch_recovery_tests.rs::read_only_exceptions_are_recorded_and_temporary_paths_are_private_per_attempt`
- Owner: `crates/confinement/src/plan/read_only.rs`

Read-only Codex and Claude lanes with open network access can use the host
GitHub login. The host asks `gh auth token` outside confinement, including
keychain-backed logins, and passes the result only in the live child
environment; no token enters setup files, prompts, receipts or logs.
Shell here-documents use the attempt scratch directory through `TMPDIR` and
`TMPPREFIX`. Apple’s `/bin/bash` 3.2 ignores `TMPDIR` for here-documents, so
profiles also allow its `/private/var/tmp/sh-thd*` files. Denial hints name the
reported operation and point to the captured output, which holds the path,
without claiming which sandbox or filesystem rule caused the refusal.

Commit-pinned source views permit execution while staying immutable. Read-only
lanes set `CARGO_NET_OFFLINE` only when their effective network is restricted.

Read-only runs use their bound repository with its Git metadata read-only.
Their PATH selects the host-resolved Git executable; the sandbox grants that
executable and its linked libraries read access and grants only that executable
permission to run.
Lane Git output is never trusted host evidence; the host uses its own pinned
Git and environment for evidence and settlement.
- Why (default): a read grant must not permit mutations of shared Git metadata.
- Test: `crates/providers/src/seatbelt_snapshots.rs::agy_read_only_linked_worktree_git_directories_are_not_writable`
- Owner: `crates/providers/src/launch.rs`, `crates/confinement/src/permissions.rs`

## Lane Git and import

Every writer gets a complete Git directory at `scratch/git-common`: its own
index, HEAD, refs, config, temporary files, merge and rebase state. The host
copies the initial linked index and ref snapshot before launch. Plain
`GIT_DIR` and `GIT_COMMON_DIR` select that directory, `GIT_WORK_TREE` selects
the lane checkout, and `GIT_OBJECT_DIRECTORY` selects `scratch/git-objects`.
The lane's ambient environment exports none of these selectors, nor the
`GIT_AUTHOR_*` and `GIT_COMMITTER_*` identity. The Git shim on `PATH` sets them
only when Git acts on the checkout: its physical working directory (after any
leading `-C`) is inside the checkout and the nearest `.git` entry walking up
from there is the checkout's own marker. `init` and `clone` never bind. In every
other case the shim unsets those variables before running Git, so a binding
inherited from a parent process, such as a hook, cannot leak. The shim skips
Git's documented global options, with their separate values, to find the
subcommand; an option it does not recognise means it does not bind. Git
children inherit the bound selectors, so a hook that changes directory and
calls Git by absolute path stays bound to the lane. The threat model is
accidental fixture commits; for a sandboxed lane the sandbox is the boundary,
and a lane can only affect its own branch. A fixture that
runs `git init` and commits in a temporary directory, even one under the
checkout, therefore acts on that directory with its own `-c user.name` and
cannot read or write the lane's index or branch. That private object
store reads the shared store through
`objects/info/alternates`. Stash, rebase, merge and garbage collection operate
on private Git state without individual metadata grants. Launch pins the base
as `refs/provenant/lane-base/<attempt>` until terminal publication, so shared
garbage collection cannot discard a base that a private rebase no longer names.

The original worktree gitfile stays unchanged so the host retains its verified
repository identity. Seatbelt denies writes throughout both the shared Git
directory and the linked admin directory, including refs, config, hooks, info
and the original index; an unsandboxed lane has no such denial and is trusted
not to bypass its shim. Inside the checkout the shim's selectors intentionally
select the lane repository. For a sandboxed lane the sandbox, not the shim, is
the hard boundary:
Git run by absolute path inside the checkout bypasses the shim, finds the
original gitfile and can only read the shared metadata, never write it, so it
cannot reach other lanes or change the host repository. Outside the checkout
`git` discovers its repository from the directory as usual.
Writer launch sets `GIT_CONFIG_GLOBAL` to a private file that includes the host's
existing global Git configuration and adds the resolved global identity and
`user.useConfigOnly=true`. System Git configuration remains enabled. Repository
identity overrides the global identity; absent identity cannot fall back to a
generated hostname identity. Identity values containing control characters
refuse launch. The included host files remain readable in the writer sandbox.
Command-scope Git configuration resets credential helpers and supplies the lane's
GitHub token helper; the wrapper restores that precedence when a subprocess
clears the environment but retains `GH_TOKEN`. Other host Git settings remain in
force. Explicit command-line Git configuration can override these settings.

The small Git wrapper has one source and one installation path for the pinned
executable and its configuration. It applies the lane binding and the lane's
commit identity inside the checkout, whether or not a subprocess cleared its
environment, and supplies publishing credentials when the lane may publish. It
reads only the leading options needed to find the directory Git will act on and
does not route objects. It stays because environment variables alone do not
survive `env -i`-style subprocesses. The restore needs `git` to resolve to the
wrapper: a subprocess that also drops `PATH` reaches system Git, which finds
the shared repository through the worktree's `.git` file. That Git can read
shared metadata, which the sandbox grants read-only, but cannot write it, and
it loses wrapper-provided publishing configuration when its environment is cleared. Per-adapter tests for every writer adapter
(Claude, Codex, Cursor, Agy and OpenCode) prove the pinned executable, the
private refs, index and objects, the identity scrub and publishing credentials
under a cleared environment that keeps `PATH`. The command adapter does not
support writer mode.

When a run operation returns `RunError` because the host cannot find a Git
executable, it reports "Git executable unavailable" with the `GIT` code.
Launch handoff converts this error to a handoff refusal with a reason string.

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
same Git receive boundary. The host publishes the real branch with a
compare-and-swap against the pinned launch base. Replaying the same head is
idempotent; a branch already containing that head also satisfies publication. A diverged shared ref refuses publication. A published tip
must descend from the lane base or the current `origin` default branch, which
allows rebases while refusing unrelated and orphan histories. Success,
cancellation and failure remove `refs/provenant/import/<head>` and the lane's
base pin. Host integration and delivery check connectivity with
`rev-list --objects --missing=error` excluding the lane base, then read those
delta objects through `cat-file --batch` to refuse corrupt reachable payloads.
They do not rescan unrelated shared history.

[Drain writers before upgrading](../runbooks/install.md#bootstrap-and-upgrade).
A format-2 interrupted settlement with its bound private object directory can
resume through this import path. An attempt without that source is refused
with instructions to recover its commits on the host and start a fresh run.
There is no alternate verifier or separate settlement path. Terminal refusals
release live custody and the reservation while preserving private Git evidence
for host recovery.
- Why (default): Git already owns the receive boundary; private Git state removes the
  fragile list of individual write exceptions under the shared repository.
- Test: `crates/confinement/tests/lane_git_conformance.rs`;
  `crates/run/src/objects_tests.rs`
- Owner: `crates/confinement/src/permissions.rs`, `crates/providers/src/launch.rs`,
  `crates/providers/src/lane.rs`, `crates/run/src/objects.rs`,
  `crates/run/src/writer.rs`, `crates/run/src/delivery.rs`

## Project GitHub policy

`projects/<ProjectId>/github.json` holds an optional host-owned advisory deny
list of GitHub CLI verb prefixes and labels for writer lanes. Missing policy
defaults to empty. When publishing is available, the lane's `gh` shim refuses
matching calls with the policy key on standard error. This is an accidental-use
guard, not a security boundary: a lane can invoke the real `gh` directly.

- Why (default): projects can reserve GitHub actions for people or workflows without
  relying on instructions in a lane's prompt.
- Test: `crates/config/src/tests.rs::github_policy_is_scoped_to_project_and_defaults_only_when_absent`;
  `crates/providers/src/lane.rs::github_policy_guards_labels_and_verb_prefixes`
- Owner: `crates/config/src/github_policy.rs`; `crates/providers/src/lane.rs`

## Lane GitHub credential, gh budget and CI scope

Lane GitHub authority is the user's authority. Launch selects the credential
through one seam (`CredentialSource`); today the only source is the user's own
`gh` login, source `default`. A later minted App token is a new source, and a
failed mint refuses rather than falling back to the user's login. Every lane's
provider receipt records `github_credential`: identity, source, delivery
(`injected` with the token's origin, `ambient` for an unsandboxed lane reading
the host login, or `none` with the reason) and whether the `gh` shim mediates
calls. Token bytes are never recorded.

Unsandboxed writers with publishing also get the `gh` shim, so the deny list and
the budget apply there too. The shim counts each call and warns on standard
error at every multiple of `lanes.gh_budget` (default 100; 0 turns it off). It
never stops a call. Settlement records `gh_invocations` (count, budget, source,
over-budget) in usage. The lane can write its counter and can call the real
`gh` or the API directly, so the count is advisory.

`lanes.ci_scope` (`none`, `targeted` or `full`; default `none`, override with
`--set attempt.ci_scope=...`) tells a writer what CI to run or wait for. The
writer preamble states it, and the provider receipt and status show its value
and source. It grants and removes no authority. When a repository's workflows
run on label events, the shim warns before a `pr create` or `pr edit` that
changes labels, because that change starts CI.

CI cost is controlled in the workflow triggers, not by refusing lane calls. CI
runs on pull requests, `main`, the landing owner's `land/**` candidates and
manual dispatch, and superseded pull request runs are cancelled. Runs on `main`
are not cancelled, because a cancelled run reads as a failed `ci-status` to
post-merge follow-up.

- Why (default): lanes spend the user's CI minutes and API quota under the
  user's login; a receipt and an early warning keep that visible without new
  refusals, and one seam lets a narrower credential replace the login later.
- Test: `crates/cli/src/run/provider/gh_auth.rs::selection_receipt_names_identity_source_delivery_and_mediation`;
  `crates/providers/src/lane.rs::gh_budget_counts_and_warns_without_stopping`;
  `crates/run/src/ci_scope.rs::the_scope_and_its_source_come_from_the_sealed_profile`;
  `crates/config/src/github_policy.rs::label_triggers_are_found_in_either_list_form_and_nothing_else`
- Owner: `crates/cli/src/run/provider/gh_auth.rs`,
  `crates/cli/src/run/provider/preparation.rs`, `crates/providers/src/lane.rs`,
  `crates/run/src/ci_scope.rs`, `crates/run/src/gh_usage.rs`,
  `.github/workflows/ci.yml`

## Writer hard-link boundary

The sandbox profile carries `(deny file-link)`, so a lane cannot create a hard
link. Launch no longer walks the writable worktree, scratch, provider state or
lane build cache for existing hard links, and no hard-linked build or
dependency file refuses a launch or adds a per-path deny. Launch inspects only
what a second name would let the lane write: the exact write grants (provider
credential files), the selected `HEAD`, and the host Git control files the lane
may not write, `config`, `packed-refs`, `config.worktree`, the linked `index`,
`hooks/*` and `info/*`. Any of those with another hard link refuses the launch
and names the file. A held directory that fails revalidation, a provider executable that is
missing and a scratch directory that is not private each name their path and
check. A grant on a file in a directory that does not exist grants nothing and
needs no anchor. The compiled profile may be up to 512 KiB and the error names
its size.

Accepted risk: a hard link the host itself created inside a lane-writable tree
is writable through its alias, so a write through the alias modifies the file
outside the tree, and launch does not look for it. Host code that seeds a lane tree must therefore copy or clone (`clonefile`), never hard-link,
from a shared store. Cargo target seeds clone and shared lane downloads use symbolic links
with shared locks, retained provider state is copied with hard-link refusal, and the pnpm store is
read-only to the lane, so no host path creates such a link today.
- Why (default): a writable hard link could modify data outside the granted tree, but
  only a link made by something other than the lane can exist, and only the
  Git control files make such a link dangerous; refusing whole build trees cost
  launches without protecting anything.
- Test: `crates/confinement/src/permissions/tests/validation.rs::only_git_control_files_are_checked_for_other_names`;
  `crates/confinement/src/permissions/tests/validation.rs::a_hard_linked_exact_grant_names_its_path`;
  `crates/confinement/src/permissions/tests/validation.rs::held_path_failures_name_the_path_and_the_check`;
  `crates/confinement/src/permissions/tests/validation.rs::a_grant_in_a_directory_that_does_not_exist_needs_no_anchor`
- Owner: `crates/confinement/src/permissions.rs`

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
`publication.lock` (one per attempt directory) and the run store lock keep their inodes.
- Why (default): unlinking an owner lock lets two owners lock different inodes, and
  deleting under a reservation destroys a writer's work.
- Test: `crates/run/src/holder.rs::writer_recovery_accepts_only_matching_terminal_absence`;
  `crates/run/src/dispatch_temporary_tests.rs::reconcile_preserves_protected_locks_outside_temporary`;
  `crates/run/src/dispatch_temporary_tests.rs::reconcile_leaves_temporary_data_behind_a_writer_reservation`;
  `crates/run/src/dispatch_temporary_tests.rs::reconcile_unreadable_reservation_only_retains_existing_temporary_data`;
  `crates/run/src/dispatch_temporary_tests.rs::reconcile_temporary_removal_never_follows_symlinks`
- Owner: `crates/run/src/controls.rs`

## Store

The run store is SQLite with write-ahead logging (WAL), foreign keys and `synchronous=FULL`.
Its unit is the attempt row, unique by work key and by attempt ID; a run's
record is read from its attempt rows rather than stored. On disk an attempt
lives at `attempts/<AttemptId>/` under the store root, and receipt paths are
relative to that root. No run or node identifier appears in a path; the
publication lock is the attempt's own. The files that belong to a whole
submission (`request.json`, the settlement queue and locks) sit in the
directory of its first attempt, until graph records replace the run level.
Each admitted record persists its work key; reads reject a missing key or one
that disagrees with the indexed project, graph, node or attempt number.
The store opens from one schema file at one version and refuses any other
version (`STATE_RESET_REQUIRED`, naming `install --reset-state`) without modifying it; it also
refuses unsafe companion files beside the database, and caps (1 MiB record or prompt, 64 MiB stream,
4 MiB result) make an oversized result a typed failure, never a truncation.

Every file and directory sync made by `provenant-os` flushes the storage device
(`F_FULLFSYNC` on macOS, `fsync` elsewhere), once per sync. `PROVENANT_DURABILITY=relaxed`,
read once per process and passed on to the guardian, makes macOS use plain `fsync`
instead. Relaxed state survives a process crash but not power loss; it is for
disposable test and CI state, and any other value or none keeps the full flush.
- Why (default): a corrupt or foreign store must never be repaired into evidence.
- Test: `crates/run/src/tests.rs::opens_refuse_unsafe_database_companions`;
  `crates/run/src/tests.rs::run_store_keeps_no_run_or_operation_tables`;
  `crates/run/src/tests.rs::oversize_is_refused_before_publication_or_read`
- Owner: `crates/run/src/db.rs`

## Reads and waits

### Output redaction and identity

Known credential shapes are filtered at three sites. First, the guardian
applies independent streaming redactors to stdout and stderr before retaining
bytes or writing captures to disk. Split credential markers remain buffered
across chunks. A filter failure discards that stream's remaining output.
Provider lifecycle frames are decoded before filtering.

Second, at settlement the holder filters the decoded result again before
sealing `result.md`, including credential markers revealed by JSON escapes. It
also rewrites provider-owned session text in the attempt's private provider
state in place. The scrubbed stores are Codex `sessions` and
`archived_sessions` `.jsonl` rollouts, Codex `history.jsonl` and
`session_index.jsonl`, Claude `claude-config/projects` (`.jsonl` transcripts
plus the `.txt` and `.json` tool-result files Claude keeps beside them), and
Claude `claude-config/history.jsonl` and `session_index.jsonl`. The scrub accepts
at most 64 `candidate-N` directories, descends at most eight levels, and lists
at most 100,000 entries per directory. A file is rewritten only when a shape
matches; a clean file is read but never copied, staged or synced. Links, hard
links, files that change while they are read, a `://` authority longer than the
64 KiB frame bound, and anything past those bounds are refused. A refused entry does not stop
the scrub of the others, and the scrub reports failure once every entry has
been visited. Masking a shape keeps the surrounding transcript usable for resume.
Earlier run files and provider history archived by an earlier settlement are not
rewritten.

Provider stores outside that set are not scrubbed, and keep whatever a lane
printed until provider state expires or is archived as-is:

- Codex SQLite databases (state, thread-history and log stores) and their WAL,
  SHM and journal companions.
- agy's `agy-home` copy of its history and conversation summaries, including
  `conversation_summaries.db`.
- Every `claude-config` entry outside `projects/` other than `history.jsonl` and
  `session_index.jsonl`, such as Claude state directories beside `projects/`.
- Files under the scrubbed trees with any other extension, and `.txt` or `.json`
  files outside `projects/`. Binary files, credentials, cache trees and links
  are never touched.

Third, result-only orphan recovery preserves the sealed bytes and their digest
instead of rewriting an answer it did not filter. It reports `redacted: false`
when those bytes still contain a known credential shape or cannot be safely
filtered, so such an orphan keeps its plaintext until retention prunes the
attempt's evidence and is never presented as redacted.

A redaction failure at settlement does not return a validation error or block
terminal publication, and it cannot repeat itself. Two failures are distinct.
A result failure replaces the result with the fixed placeholder
`[result withheld: redaction failed]`. A session transcript failure keeps the
filtered result as sealed, since only the transcript scrub failed. Both append
`redaction failed; result withheld` to the lane's reason. The text is the same
for both, so a session-only failure's reason still says "result withheld" while
the result stays unwithheld. Either failure overrides the outcome only for a
lane that would succeed, needs review or is uncommitted, which settles `failed`.
A cancelled, timed-out, rejected or lost lane keeps its outcome, and any reason
it already has (such as a leftover-process note) is kept before the redaction
text. No delivery starts after a redaction failure. A result-only orphan is the
exception named above: recovery keeps its sealed bytes with `redacted: false`
rather than substituting the placeholder.

Result artifact `digest` and `bytes` identify the stored, redacted bytes;
there is no raw result copy or raw-result digest. Newly sealed holder results set
`redacted: true` to mean that the stored bytes are a filtered rendering or the
fixed placeholder, even when no credential matched. It is not a count of
replacements or a guarantee against unknown secret formats.
- Test: `src/compose_tests.rs::guardian_capture_masks_split_credentials_before_retention`;
  `crates/run/src/dispatch_settlement_tests.rs::holder_seals_redacted_results_and_digests_the_stored_bytes`;
  `crates/run/src/dispatch_settlement_tests.rs::settlement_scrubs_provider_session_history`;
  `crates/run/src/tests.rs::result_orphan_redaction_flag_describes_the_preserved_bytes`;
  `crates/confinement/src/redact.rs::settlement_scrub_removes_tokens_from_session_text_and_keeps_binaries`;
  `tests/graph_provider.rs::printed_credentials_leave_no_plaintext_in_run_files`
- Owner: `src/lib.rs`, `crates/run/src/holder.rs`, `crates/run/src/store.rs`,
  `crates/confinement/src/redact.rs`

`output` scans the whole artifact for known shapes before paging, including
older artifacts. Its offsets and digest refer to those filtered bytes. Prompt
reads are lossless and need dispatch-equivalent authority. A terminal attempt with no requested
output is `NOT_FOUND`. `output` and `wait` take `NAME[/NODE[#N]]` targets, as
in [CLI](cli.md#targets), which also owns wait budgets and reader cancellation
([Waits are readers](cli.md#waits-are-readers)). A wait with no target selects
the project's running graphs; inside a lane a wait names its targets, so a
lane never waits on itself or its ancestors by default.
- Why (default): attention must never wait behind a running sibling.
- Test: `crates/run/src/tests.rs::create_publish_and_read_exact_bounded_facts`;
  `crates/fabric-runtime/src/runs.rs::input_required_wakes_all_without_waiting_for_a_sibling`
- Owner: `crates/run/src/reader.rs`

## Nested submission

A lane's `run` adds a child to its host-owned graph through the attempt-bound
relay. Children share graph budget, cancellation and trace; attached live
children are cancelled when the parent settles. Use `provenant wait NAME` to
collect them before exiting, or explicitly detach work that should outlive its
parent. Inside a lane `--name` names the child node, and a named child replays
by name and input like a top-level launch; an unnamed child gets a fresh
handle each time.

`status`, `output` and `trace` inside a lane use the same relay. The
host resolves the caller from its custody-bound attempt, accepts only targets
in that graph, and invokes the ordinary readers. With no target, `status`
selects the caller's graph. Node and attempt selectors from child launch
receipts work without journal or credential grants in the lane. Output ranges
remain bounded; `--to` writes returned bytes in the lane. `--path` cannot grant
access to a host artefact and is refused with a content-reading remedy.

A child can select another project with `--project`, including a writer in
that project's registered worktree. The graph retains parent attribution while
writer locks, retained import fences and recovery use the selected project's
custody namespace. A host-owned `writer-projects/<parent>/<source>/` reference
lets recovery inspect those exact namespaces without scanning unrelated projects.
Every writer request records its selected custody namespace, and recovery uses that
exact registered project. Changing provider cwd never changes which graph its
attempt-bound relay serves.

The host preserves typed admission causes in refusals so the caller can repair
its launch request. Credential roots, state roots and overlapping worktree
write roots retain the ordinary launch protections.
- Why (default): any agent must be able to launch children and collect their results and
  failure causes without gaining direct access to host journals or credentials.
- Test: `src/fabric/interface.rs::explicit_writer_children_use_their_selected_project_custody`
- Owner: `crates/cli/src/fabric/graph_relay.rs`, `src/fabric/unified.rs`

## Not provided

- Watchdog silence as an outcome, `fallback:any` or silent effort downgrade.
- Alias coercion, old-root scans, record migration or a state deletion
  command.

## Lane-owned worktrees and Git membership

Writer access requires an explicit verified worktree; explicit read-only
access may bind a worktree without writer authority. Git's inventory and
common-directory/backlink checks admit existing linked checkouts without an
adoption step. Registry reservations do not establish Git membership, and
ownership metadata controls lifecycle cleanup rather than admission authority.
Per-attempt supervisor evidence still controls launched-work recovery.

Worktree creation/reuse belongs to the
[worktree owner](../../tools/front-door/crates/confinement/src/worktrees/owner.rs).
An existing tree must be clean with free custody. Continuation retains its
sealed owner and lineage; a changed owner or a live predecessor refuses reuse.
An uninitialised gitlink is acceptable, while an initialised nested repository
requires review because Git could follow lane-controlled configuration.

Owned settlement records `cleanup_deferred: true`, unless a worker already
began pruning, and then spawns one detached collection. A failed spawn records nothing.
It performs no network operations, recursive output survey or worktree removal.
Deferred-intent receipts are never final. A detached worker can remove only its
own clean checkout after successful
execution or delivery, with the exact head proven retained by its creation base
or delivered ref. Branch deletion also requires landed integration; push and
open PRs retain branches. Ownership, terminal state, absent live work, custody,
registration, sequencer state and head are checked under removal locks.
Only declared regenerable output may be discarded; other ignored files retain
the checkout. Durable removal intent permits replay when the checkout is gone.

Tracking-ref refresh in the worker is best effort, bounded and before item custody.
Failure cannot change the attempt outcome or an adopted tree's receipt. The
[lifecycle policy](../worktrees.md) owns operational guidance.

- Why (default): cleanup may reclaim only owned work proven retained elsewhere.
- Test: `crates/cli/src/run/worktree.rs::worktree_access_preserves_writer_and_implicit_reader_rules`;
  `crates/confinement/src/worktrees/lifecycle_tests/gc_preview.rs::git_inventory_accepts_unregistered_and_recreated_worktrees`;
  `src/fabric/peer_writer_delivery.rs::settlement_defers_tracking_ref_refresh`
- Owner: `crates/confinement/src/worktrees/owner.rs`, `crates/cli/src/run/worktree.rs`, `src/fabric/settlement.rs`

## Ref ownership and the landed check

The ref ledger lives in host state at `<state>/worktrees/<project>/refs/`, one
bounded atomic `RefRecord` per endpoint and full ref, shared by every writer
for the repository. A record holds the repository's project ID, the endpoint
(`local`, or `remote` with the remote name and push URL from the bound delivery
endpoint), the full ref, a generation, the origin (`created` or `adopted`), the
tip it saw, the time and optional attempt and operation IDs. Worktree creation
records a branch it made as created and an existing branch as adopted, under
the registry lock; a graph's local integration branch is recorded the same
way. A delivered push records its remote branch as created when the remote
did not have it, otherwise adopted. Integration targets are never claimed.
Every creation or adoption is a new generation, so a ref recreated at the
same commit never inherits an earlier proof. An adoption that finds the ref
still at the recorded tip, or an attempt replaying its own observation, keeps
the current generation. A ref with no record is unobserved and retained. A
failed record write is a warning, never a creation failure.

One landed check, `worktrees::landed`, decides whether a commit has landed. It
is landed when it is an ancestor of the freshly read target tip, or when a
chain of integration receipts carries it there: each delivered head contains
the previous link, each receipt delivers the ref the previous one integrated
into, each integration commit is the delivered head, a merge of the previous
tip and the head, or a single commit on the previous tip, and the chain ends at
the authorised root, which contains the last integration commit. A rewritten
target, a missing commit or any broken link is unproven, which retains. Tree
and patch-id equality are never evidence. Named and merged removal, GC, owned
lane pruning and delivery's already-integrated test all use it.
- Why (default): cleanup may only remove refs Provenant made, and only once their work
  is proven to have landed.
- Test: `crates/confinement/src/worktrees/lifecycle_tests/ref_ownership.rs`;
  `crates/confinement/src/worktrees/landed.rs::tests`;
  `crates/run/src/git_context.rs::recorded_local_branch_is_created_once_then_adopted`.
- Owner: `crates/confinement/src/worktrees/refs.rs`,
  `crates/confinement/src/worktrees/landed.rs`

## Removal on completion

Settlement and `gc` remove what a finished lane leaves behind: its worktree,
local branch, remote branch and remote-tracking ref. Only refs whose current
ledger record is `created` are candidates; for a worktree owner that links a
generation, the record must still be that generation. A candidate goes once
the landed check proves it landed and its tip matches the ledger's recorded
tip. Unlanded refs and refs with an unknown landing state stay regardless of
age and are reported by GC. A branch any
worktree has checked out, the landing target itself, dirty worktrees,
worktrees with a live writer and declared keep paths are retained. Local refs
are deleted with `update-ref -d` on the tip just read and remote branches with
`git push --force-with-lease=REF:TIP` to the push URL the ledger recorded, so
a ref that moved is kept. A removed or absent ref's record is forgotten only
while it is still the generation acted on. `prune_owned` deletes an
integrated lane's local branch only when it is created; settlement then
removes an integrated lane's own created remote branch on the receipt's
chain. `gc` lists the ledger, fetches each bound endpoint's default branch
once, and judges local branches on those targets and the fetched default.
Each artefact reports one `ArtefactOutcome` (`artefact`, `name`, `remote`,
`outcome`, `reason`, `head`): under `artefacts` in the settlement receipt and
`worktree_cleanup[].refs` in `gc --json`. A failure warns and never fails the
settlement or the pass. Completion cleanup is one detached attempt per
settlement and keeps no queue or retry state. A transient failure leaves a
non-final receipt. The next applied `gc` sweep runs the same owner-bound
collection, trying each item's custody without a collection-wide lock, for each terminal lane whose
receipt is deferred, retryable or pruning and whose worktree exists, landing
against the fetched default branch (falling back to the owner's base when the
fetch fails, there is no origin, or the sweep is a pressure sweep, matching
settlement); dirty trees and unlanded work are retained by both.
- Why (default): finished lanes leave nothing behind once their work has landed, and
  never take a ref Provenant did not make.
- Test: `crates/confinement/src/worktrees/lifecycle_tests/ref_ownership.rs`
  (`leased_ref_deletion_keeps_a_moved_or_checked_out_branch`,
  `only_a_created_branch_at_its_linked_generation_is_a_removal_candidate`,
  `ledger_lists_current_records_and_forgets_only_the_generation_acted_on`);
  `src/lane_cleanup.rs::tests`.
- Owner: `crates/confinement/src/worktrees/removal.rs`, `src/lane_cleanup.rs`,
  `src/fabric/settlement.rs`

## Delivery modes and settled pickup

A template delivery setting resolves to the existing
explicit writer destination contract. Omission keeps delivery off. Push and
PR modes use `origin` and the lane branch; `integrate:BRANCH` selects an
existing local branch, and `integrate:origin/BRANCH` selects an existing
remote integration branch. Read-only lanes may select only none.
Direct push and integration refuse publication to the default branch recorded
by any configured remote and the primary checkout branch. If no remote HEAD
is available, both `main` and `master` are protected. PR delivery publishes a
feature branch and may target a protected branch as its PR base; its published
head branch is subject to the same protection.
Dispatch checks publication targets before admission for concise modes
and manifests, so a refused target leaves no admitted run.
Destinations supplied in flags are also checked before creating an owned tree.
Accepted operations compare and replay their immutable inputs before checking
current branch protection; later protection changes do not invalidate replay.
Actual delivery still rechecks protection.
Host Git output is bounded per call. A `worktree list` listing may use the
8 MiB capture default, so a repository with many linked worktrees can still be
checked; other calls keep 16 KiB, and `--show-origin` keeps 1 MiB. Output over
its bound is refused at the stage that read it, and the error names the
command and the limit. The graph edge keeps that text. A truncated
listing is never treated as complete.

Integration prepares objects without holding an application lock through Git.
It fast-forwards when possible, otherwise uses merge-tree and commit-tree to
prepare a merge commit without switching checkouts. A conflict or configured
custom merge driver refuses before publishing a ref. A local integration branch
checked out in any worktree is refused. Publication uses expected-old ref
comparison locally or an explicit remote lease after an ancestry proof.
A concurrent publisher can win; the losing delivery records an integration
failure for explicit host pickup rather than adding another retry counter.
A graph worktree whose node integrates into a local branch must sit on a
different branch; graph admission refuses a worktree on its own integration
target. The batch worktree sits on a separate branch and is synced by the
graph's launch fast-forward, so integration never checks out a branch for it.
Before resolving an origin integration target, the host refreshes the exact
remote branch and refuses delivery if the refresh fails. Final graph PR
delivery follows the same refresh rule.

`lane deliver ATTEMPT [--deliver MODE]` uses the recorded destination, or
origin push when the settled lane recorded none. An explicit `integrate:`
override naming the recorded target keeps a recorded fast-forward-only guard,
so pickup cannot merge a refused loop round. It requires a succeeded
writer, released custody, no new reservation, a clean tree and the exact
receipt head. Delivery persists a
keyed host pickup receipt separately from the immutable terminal record.
Successful pickup replays without repeating
external effects, while failures remain explicitly retryable. A worktree
already pruned by its own lifecycle may replay its successful receipt.
PR bodies use commit subjects and bodies with run and attempt IDs, capped at
60,000 bytes with an explicit truncation note and supplied to `gh` through stdin;
PR delivery queries the repository's default base and lists the branch's PRs
before creation. A successful empty list permits creation with that base. An
open or merged PR is reused only at the delivered head and expected base. A
failed lookup, ambiguous list, base mismatch or unmerged closed PR stops
delivery; a moved head refuses verification. The failure gives an inspection
action where the host has an identifiable PR.
- Why (default): delivery is host authority attached to a verified result, and a later
  pickup must not silently deliver unrelated edits.
- Test: `crates/run/src/delivery_tests/remote_delivery.rs::local_integration_fast_forwards_merges_and_replays`;
  `crates/run/src/delivery_tests/remote_delivery.rs::local_integration_conflict_dirty_and_checked_out_refuse_without_partial_state`;
  `crates/run/src/delivery_tests/remote_delivery.rs::remote_integration_refreshes_the_leased_destination`;
  `crates/run/src/delivery_tests/settlement.rs::settled_delivery_refuses_dirty_or_changed_head_and_replays_success`;
  `crates/run/src/delivery_tests/host_commands.rs::existing_pr_delivery_only_creates_after_positive_absence`
- Owner: `crates/run/src/delivery.rs`, `crates/records/src/run.rs`, `crates/cli/src/run.rs`

## Fresh continuation fallback

This section covers a manual `provenant retry` of a terminal attempt. What follows a stop without
one is decided under [What follows a stop](#what-follows-a-stop). A retry always
starts a fresh attempt; it never continues the provider session, so no
continuation state need be retained. The
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
scanning and prompt limits apply to the combined text. The unique
`continues` column on the successor's attempt row permits one successor, and operation replay retains the
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
replaced by adoption; a current foreign lane owner always refuses. Missing or
reassigned worktrees require a new owned checkout for the fresh attempt, and the
retry is refused without one.
- Why (default): a missing provider handle should not require the chair to reconstruct
  context or bypass immutable attempt records and writer custody.
- Test: `crates/run/src/dispatch_temporary_tests.rs::fresh_resume_preserves_lineage_and_authority_without_provider_state`;
  `crates/run/src/dispatch_lifecycle_tests.rs::resume_continues_a_terminal_attempt_once_with_its_route`;
  `crates/run/src/controls.rs::resume_preserves_explicit_worktrees_and_settled_lane_ownership`;
  `crates/run/src/controls.rs::resume_refuses_a_pruned_worktree_reused_by_another_lane`
- Owner: `crates/run/src/controls.rs`, `crates/run/src/launch.rs`, `crates/cli/src/run.rs`

### Alias quota recovery

Named route pools skip members with fresh, unreset exhaustion. Stale spent
observations rank a member behind other ready members, but never refuse a launch.
When every member has fresh exhaustion, refusal identifies the members
and their earliest known resets. An exact model request keeps its model.
A quota stop may retry the same alias even after generated work, within
`settlement_retries` and the attempt limit. The graph archives the prior attempt and seals its reference in the successor;
spent members remain in routing exclusions without a second run-policy lineage. Writers keep
the same worktree, including uncommitted edits whose paths are listed in the
successor's brief. The successor is instructed to inspect its files and commits
and continue the work rather than restart. The original source pin, settled
predecessor head and worktree ownership must still match. Read-only lanes retry
the original task.
Graph writer retries seal `context.writer_predecessor` in their launch request.
After acquiring writer custody, launch rechecks that predecessor's terminal
state, released custody, graph and node identity, source pins and ownership.
This check does not create a run-policy `retry_of` relationship.

## A stop is sealed once

Work has one identity, the `WorkKey`: project, graph, node and
one-based attempt number, written `GRAPH/NODE#N`. A plain lane is a one-node
graph. An attempt that ends without finishing its brief seals one `Stop` at
settlement: a typed `cause` and the writer's `handoff`, decoded once by
`Handoff::from_turns` and stored typed, so no reader parses result text again.
A finished attempt seals no stop. A cause is one fact. Facts about the
worktree, such as whether it holds a new commit, sit beside the stop, so a
writer that stopped early with no commit is a `stopped_early` stop, not a
separate cause. Settlement takes the cause from a recorded terminal cause,
then from a host outcome the writer cannot know (lost or unsettled custody,
cancellation, a deadline, a depleted route), then from the handoff. An
`uncommitted` outcome is a worktree fact, so a declared stop that left edits
keeps its declared cause.

A `stopped_early` or `blocked` stop needs the handoff that declared it, and a
`done` handoff declares no stop. `blocked` is a stop cause, not a plain
failure (#3154): the stop carries its handoff, and `status` and `output` show
the handoff note as the one-line reason it is blocked. A blocked node still
does not satisfy its dependants. Settlement seals the stop once, from the
handoff it decoded; the graph reads the sealed fact, never result text. A host
refusal before launch seals a specific cause, never a generic one, and its
message, redacted and bounded, is the stop's one-line reason unless a fixed
note names the cause. `TerminalCause` maps to `StopCause` exhaustively
(`From<TerminalCause> for StopCause`), and `Stop::declared` returns an error
for a handoff that cannot be sealed. Each record keeps fields it does not know, so
a newer writer's additions survive a rewrite; a `WorkKey` and a `NextStep`
ignore them, because their identity is their known fields.

A lane's limits are one `LaneLimits` record of `Limit`s, resolved when the
attempt is admitted to launch (token budget, context handoff threshold, deadline;
`crates/run/src/limits.rs`). A `Limit` is a value, its `source` and its
`strength`. The source is the nearest setting: `launch`, then `template`, then
`project`, then `host`, then `default`. The strength is `default` (the host stops
or refuses at the value, and `retry --extend` may raise it), `ceiling` (the same,
and nothing raises it, like a provider's host runtime cap) or `advisory` (the host
reports the value and does not act, like a token budget for a provider that
reports no live usage). The enforcement, the lane preamble's limits block and the
`limits` of `status --json` all read that one record, so the preamble states only
what the host enforces. A review finding's
`FindingDecision` records its disposition (`act`, `decline` or `replace`), a
reason, and the reviewed head and base, so a review binds to the change
rather than the head commit (#3132). A graph `Pause` records a typed cause
(`stop` with its stop cause, `continuation_budget`, `invalid_plan_expansion`,
`invalid_loop_decision`, `loop_ask_closed`, `loop_paused` or
`structural_limit`), the node that paused it and one bounded sentence.
- Why: a stop was spread over a terminal cause, an attempt outcome, partial
  signals and a handoff re-parsed from result text; one sealed record leaves
  one place to read it.
- Test: `crates/records/src/format2_tests.rs::each_record_round_trips_and_keeps_an_unknown_field`;
  `crates/records/src/limit.rs::tests::the_nearest_source_wins_and_a_ceiling_cannot_be_raised`;
  `crates/run/src/limits.rs::tests::each_limit_reports_the_winning_source`;
  `crates/run/src/limits.rs::tests::the_preamble_and_the_capture_plan_read_the_same_numbers`;
  `crates/records/src/format2_tests.rs::a_declared_stop_needs_the_handoff_that_declared_it`;
  `crates/records/src/format2_tests.rs::a_declared_stop_survives_uncommitted_edits_but_not_uncertain_custody`
- Owner: `crates/records/src/stop.rs`, `crates/records/src/work_key.rs`,
  `crates/records/src/limit.rs`, `crates/run/src/limits.rs`, `crates/records/src/finding_decision.rs`,
  `crates/records/src/pause.rs`

## What follows a stop

One table,
`crates/records/src/stop.rs#STOP_TABLE`, gives every stop cause exactly one
default next step: `same_route`, `new_route` or `chair`. A same-route step
carries its continuation, `resume` (the stopped provider session continues)
or `fresh` (a new session on the same worktree, briefed with the handoff). A
session never crosses routes, so `new_route` is always fresh: another declared
route, or a redraw of an alias pool without the routes that stopped.

The override point is the node's `on_stop` entry for a cause in the graph
document, applied by `StopCause::next_step`. A pinned cause ignores it. A graph
restart is always a fresh session, so validation refuses an `on_stop` entry
that asks for `resume`.

The graph's one decision, `crates/fabric-runtime/src/graph/next_step.rs`, reads
the stop of a failed node's last launched attempt: its sealed cause, or the
cause its outcome names. It applies the table as follows.

- A cause with retries left goes back to the route that stopped, then takes
  its default. The count is this node's same-route restarts for that cause.
- A `lost`, `unsettled`, `settlement_failed` or `quarantined` stop may still
  hold its worktree, so custody is not proved released and the stop goes to the
  chair whatever `on_stop` says. Uncertain custody never starts another writer.
- A depleted route (`usage_limited`, `model_unavailable`, `auth_required`)
  hands the work at once to the next rung of the routing depletion ladder
  (`crates/routing/src/ladder.rs`) through `depletion_successor`. The host
  supplies the ladder's facts through `AttemptOwner::depletion_facts`: the
  readiness the stopped attempt's preparation probed, the routing
  configuration, the host-native routes and the exclusions the stopped attempt
  carried. No route the node already ran is offered again. The work waits for
  the stopped route's reset only when `routing.depletion_wait_seconds` is set
  and the reset falls inside it. The ladder reads readiness only for the
  adapters the stopped attempt probed, so when no rung yields a route the
  node's own untried declared routes go first, and only when none is left does
  the stop go to the chair. A host that keeps no routing facts restarts on the
  declared routes in the same way, as does a facts read error. Ladder availability
  observations are used for at most one hour. The ladder preserves a pinned
  account and does not raise the tier. A chair-bound stop is fenced once.
- A restart patches the node in place: the worktree and its edits stay, prior
  attempts are archived, and the patch record names the node, the cause and the
  step. The stop is saved in one checkpoint with its restart or its chair pause,
  so no reader sees a stop without what follows it.
- Bounds can only turn a step into `chair`: a `new_route` with no other route,
  the graph-wide `limits.continuations` (default 2), which every restart spends,
  and, after a provider stop, the node's sealed settlement retry allowance. A
  declared writer stop then pauses the graph with the reason; any other stop
  leaves the node failed for the chair.
- A refusal before launch follows the preparation fallback decision. A launched
  stop, including a reader stop, follows the graph stop decision.

A junior writer that reports unfinished work continues at the senior tier.

So by default a writer that stops early, reaches its context threshold or loses
its lane token restarts fresh on another route (or its own route for a
single-route context handoff or lost lane token); a transient failure retries
once on its route, then moves; a depleted route hands over at once; everything
else returns to the chair.

| Cause | Default | Pinned | Retries |
| --- | --- | --- | --- |
| `stopped_early` | `new_route` | no | 0 |
| `blocked` | `chair` | yes | 0 |
| `handoff_missing` | `chair` | no | 0 |
| `empty_result` | `new_route` | no | 1 |
| `uncommitted` | `chair` | no | 0 |
| `input_required` | `chair` | yes | 0 |
| `context_handoff` | `new_route` | no | 0 |
| `token_budget` | `chair` | no | 0 |
| `deadline` | `chair` | no | 0 |
| `cancelled` | `chair` | yes | 0 |
| `provider_stop` | `new_route` | no | 1 |
| `process_crash` | `new_route` | no | 1 |
| `auth_required` | `new_route` | no | 0 |
| `usage_limited` | `new_route` | no | 0 |
| `model_unavailable` | `new_route` | no | 0 |
| `adapter_unavailable` | `new_route` | no | 0 |
| `permission_denied` | `chair` | no | 0 |
| `identity_unobserved` | `chair` | no | 0 |
| `lane_token_lost` | `new_route` | no | 0 |
| `source_view_refused` | `chair` | no | 0 |
| `route_refused` | `chair` | no | 0 |
| `access_policy_refused` | `chair` | no | 0 |
| `disk_floor` | `chair` | no | 0 |
| `disk_unobserved` | `new_route` | no | 1 |
| `prompt_unreadable` | `chair` | no | 0 |
| `cloud_placement_removed` | `chair` | no | 0 |
| `write_grants_refused` | `chair` | no | 0 |
| `worktree_busy` | `chair` | no | 0 |
| `launch_handoff_refused` | `chair` | no | 0 |
| `retained_writer_import` | `chair` | no | 0 |
| `unsupported_platform` | `chair` | no | 0 |
| `unknown_pacing_account` | `chair` | no | 0 |
| `unregistered_account` | `chair` | no | 0 |
| `credential_helper_failed` | `chair` | no | 0 |
| `command_unavailable` | `chair` | no | 0 |
| `host_native_unconfigured` | `chair` | no | 0 |
| `spawn_failure` | `new_route` | no | 1 |
| `settlement_failed` | `chair` | no | 0 |
| `lost` | `chair` | no | 0 |
| `unsettled` | `chair` | no | 0 |
| `quarantined` | `chair` | no | 0 |

`blocked`, `input_required` and `cancelled` are pinned because each already
names the person who acts next: the owner who decides, the owner who answers
or the person who cancelled.
- Why: what followed a stop was decided in four places (the run-level
  automatic continuation, the graph's stopped-writer restart, next-attempt
  fallback and plan continuations), and most stops ended with the chair. One
  table with one override point makes a policy change one edit, and handing
  depleted work over at once keeps it off the chair.
- Test: `crates/records/src/format2_tests.rs::every_stop_cause_has_exactly_one_default`;
  `crates/records/src/format2_tests.rs::the_spec_table_matches_the_stop_table`;
  `crates/records/src/format2_tests.rs::an_override_moves_only_an_unpinned_cause`;
  `crates/fabric-runtime/src/graph/next_step_tests.rs::a_transient_stop_retries_once_on_its_route_then_takes_a_new_route`;
  `crates/fabric-runtime/src/graph/next_step_tests.rs::a_depleted_writer_moves_to_a_new_route_at_once_in_its_worktree`;
  `crates/fabric-runtime/src/graph/next_step_tests.rs::a_writer_that_lost_its_lane_token_continues_from_its_handoff_on_a_new_route`;
  `crates/fabric-runtime/src/graph/next_step_tests.rs::on_stop_sends_a_stop_to_the_chair_and_unproved_custody_always_goes_there`;
  `crates/fabric-runtime/src/graph/next_step_tests.rs::restarts_stop_at_the_graph_wide_continuation_budget`
- Owner: `crates/records/src/stop.rs`, `crates/fabric-runtime/src/graph/next_step.rs`
