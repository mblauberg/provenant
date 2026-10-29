# Command surface

Owner: `crates/cli` (grammar, rendering), `src/` (composition, hooks)
Anchor: `tests/parser.rs`, `crates/cli/src/render.rs`, `tests/hooks.rs`, `tests/catalogue_read_only.rs`, `crates/cli/src/console/board.rs`, `crates/run/src/board.rs`

One executable, `provenant`, serves people, skills, hooks and CI. The `cli`
crate parses arguments, calls typed library operations and renders typed
results. The [cross-cutting decisions](README.md#cross-cutting-decisions)
define the envelope, exit codes and wait budgets. The machine-readable help
catalogue owns command syntax; the generated [CLI reference](../reference/cli.md)
lists every leaf, flag and bound. Paths are relative to `tools/front-door/`.

## Agent output

Ordinary human results default to a compact summary. The command's owner
supplies it as typed data: a headline with counts by state, then action rows
first (failed, review required, uncommitted, running) with IDs and states, at
most 12 rows of 200 bytes, a count of rows held back, and one typed `next`
command. `status`, `lanes`, `route list`, `quota show`, `pr status`,
`accounts list` and `fabric graph status` do this. A command without an owner
summary falls back to a generic truncator that shows text of up to 2 KiB whole
and otherwise says how many lines it omitted. `next` is the only source of the
next command: when a command has none, none is printed. Errors keep their
repair command and are never compacted.

Global `--full` means complete detail for the command and nothing else; it
does not widen `status` to `--all`. `--json` keeps the machine envelope and
takes precedence over human presentation flags. Explicit help and protocol
transports retain their complete output. Streaming commands keep their
bounded state-change output. Every bound lives in one module
(`crates/cli/src/limits.rs`); help constraints and this page quote it.

`output` shows the whole final result up to 12 KiB. Above that it keeps the
head and tail and names the omitted bytes and `--full`. It adds a settlement
line and the changed files from a local writer's recorded base and final head
when readable (at most eight paths of 120 bytes); missing evidence, including
peer diffs, is null. `--tail N` retains at most 500 lines within `--max-bytes`,
locally or over the existing peer paging protocol. Peer tail scans share a
30-second budget and reject changed artifact digests. The JSON envelope keeps
`bytes`.

The core verbs remain `run`, `status`, `land` and `quota`. `dispatch` is the
advanced submission-only operation; `run` adds waiting and a result summary.
Route resolution accepts a pool or exact model positionally. Account selection
uses `--account` on `run` or `dispatch`; peer output uses
`output ATTEMPT --host HOST`. Nearby command and flag spellings receive a
scoped suggestion, and common malformed forms receive an invocation example.

## Console catalogue and peer coverage

`console board` and `console tree` default to local reads. `--hosts all` or a
comma-separated list of peer labels opts into peer observations. Reads use the
existing bounded peer request boundary and do not allocate state. The local
observation is retained when a peer is unreachable, lacks the read, or returns
partial data. Coverage reports these as `unreachable`, `unsupported` and
`partial` respectively; a successful complete observation is `complete`.
Peer reads run concurrently, with at most eight in flight and one shared
`limits.peer_seconds` deadline. Hosts still waiting at that deadline, including
those not yet started, are unreachable; later replies are ignored. Peer output
follows configured host label order regardless of completion order.
An older relay that closes without a protocol refusal is reported as
`unreachable`, since that response cannot distinguish an unsupported action
from a lost reply without another potentially state-creating request.

Board composition retains the local page and cursor. It qualifies deduplication
by host, project and attempt, validates the host on each peer row, and reports
duplicate, invalid and truncated observations as partial coverage. A peer
continuation indicates partial coverage; the local cursor continues the local
walk and does not page the peer.

- Test: `crates/cli/src/console/peer.rs::tests::peer_board_coverage`
- Owner: `crates/cli/src/console/peer.rs`

### Nested tree

`console tree` reads host-local attempts, including retained terminal attempts,
and follows each attempt's isolated `children/<attempt_id>` store. It uses the
board's validated indexed reader and creates no store, identity or index.
`--project` restricts records to the selected local project. Depth includes the
host store; depth and examined-row limits bound the walk, and duplicates consume
the examined-row limit. Each store read has its own five-second budget.
`children_absent` counts ordinary absent child stores. An absent store with
recorded child evidence is an `unfetched_children` gap, as are failed reads and
depth or node limits. At the depth frontier an absent store is counted without
truncating. Corrupt host state fails the read; child failures preserve the
remaining observations. A missing store is not proof that no child ever existed.

Nodes contain host/project-qualified lane identities, status, parents, usage and
lists of observed child attempt IDs with edge kinds. `--detail` adds complete
owner records. Edges and gaps carry project IDs, nullable only for an unfiltered
host-store gap. Reconstructing a forest must account for missing parents and
multiple edge kinds.
Dispatch (`parent_attempt`), continuation (`parent`) and retry (`retry_of`)
edges remain distinct; each reports whether its project-qualified parent was
observed. Missing may mean outside the bounds, not necessarily nonexistent.
Attempts are counted once per project and attempt ID, with duplicate observations
and conflicting run/task identities or parent fields reported separately.
If a duplicate observation has its own child store, it is reported as
unavailable rather than silently omitted.
Nested coverage is partial for gaps, truncation, conflicts or missing parents.
Usage totals contain a
known sum and a missing count per token category; they describe observed nodes,
not an estimate of unseen work. Provider-internal children are not inferred.

- Why: nested work must remain visible without inventing edges or complete totals.
- Test: `crates/cli/src/console/tree.rs::tests::nested_tree_once`
- Owner: `crates/cli/src/console/tree.rs`, `crates/run/src/board.rs`

### Board snapshot and cursor contract

`console board` must return current host-local facts in descending immutable
attempt-sequence order, with retries as separate rows. The default selection
must exclude terminal attempts; `--all` must include them. `--project` must
restrict the existing binding; otherwise the board must span local projects.
The generated reference owns syntax and the default 50 / maximum 100 row bound.

The version 2 payload must identify UTC `observed_at`, effective `filters`,
`high_water`, `snapshot`, current `generation`, `lanes`, `next_cursor`,
`truncated` and `coverage`. The first page must fix `high_water` to the store's
maximum attempt sequence (zero when empty). A continuation must retain this
inclusive ceiling and use an exclusive descending position. New attempts above
the ceiling must not enter that walk. Each attempt below the ceiling matching
the filter when its sequence range is read must appear exactly once in a walk
that follows each returned cursor once. Later state changes must not invalidate
the cursor. A row becoming terminal before its range is read may disappear from
an active-only walk. A valid continuation with no remaining matches must return
an empty final page. `truncated` must mean more matching rows existed at the page
read; `next_cursor: null` must end the walk.

`snapshot` must be the CanonicalV1 record digest for domain `console-board`,
version 2, of `(high_water,filters)`. It identifies a selection, **not frozen
facts**. Filters must bind the resolved state-root native-path file digest,
host identity (nullable), selected project (nullable), active/all flag and limit.
A cursor must have shape `2.<high_water>.<before>.<checksum>`, using canonical
decimal positions and a CanonicalV1 `console-board-position` version 2 digest
of `(snapshot,before)`. Positions must satisfy `0 < before <= high_water <=
i64::MAX`. Clients must treat cursors as opaque. Edited positions, checksums,
filters, root bindings, unsupported versions and ceilings exceeding the current
store maximum must fail with exit 2, `CATALOGUE_INPUT` and instructions to restart
without `--cursor`. A missing store must also invalidate a continuation.
The checksum detects accidental edits; it is public and supplies no authority
or security boundary. Replaying a valid cursor re-reads its range.

Each page must obtain indexed records, ceiling validation and generation in one
SQLite read transaction. `generation` must be `events:<N>`, where N is the
store-wide maximum committed event sequence (`events:0` for an initialised
store with no events). Every board-visible run/attempt write must append an event in
the same transaction, and pruning must preserve the latest event per attempt.
Generation discovery must use primary-key end lookups, never history blob
hashing. Unrelated project writes may advance the generation. Clients may use
it to detect change but must not require it to stay fixed across pages.
Missing or uninitialised run state must return `generation: "empty"`,
`high_water: 0`, empty lanes and no cursor; `empty` is a literal, not a digest.
Unsafe, corrupt or incompatible indexed state must fail through the error
envelope. Reads must create no product state, identity, cache or migrations;
SQLite WAL/shared-memory bookkeeping remains permitted.

Rows must preserve complete owner metadata and unknown facts:

- `lane_id: {host,run_id,task_id,attempt_id}`; unknown host identity is null.
- `metadata: {attempt,run}` containing the complete owner records: title,
  state and terminal reason, all four routes (including effort and account),
  timings, usage, trace, delivery and write-grant facts, and artifact
  descriptors. Run metadata distinguishes continuation `parent`, nested
  `parent_attempt` and `children`; attempt metadata retains `retry_of`.
- `chair`, from the immutable host-owned index, checked against the run request
  digest. A missing binding is null; provider sessions never imply a chair.
- `wait`, a best-effort admission observation, and `attention`, true for
  terminal outcomes other than success or cancellation.
- `usage_coverage: {known,missing}` listing the five token categories. Null
  remains unknown, including reasoning; a reported zero is known. Source and
  context coverage remain in the attempt usage record.
- `observation_coverage`, `complete` when the owner observation read succeeded
  for this generation, otherwise `unavailable`. Failed observations must leave
  last-output time and priority null and must not discard indexed row facts.
- `estimated_cost` and `verified_links`, null with unavailable coverage until
  their owners expose these facts. Worktree delivery metadata stays in the
  attempt delivery receipt; no Git probing or inferred issue/PR links.

Chair bindings must remain immutable, digest-checked host facts. Wait reasons,
process-checkpoint last-output time/priority and observation time are ancillary
observations outside the SQLite transaction; they must not replace indexed
execution facts. Checkpoints must match the indexed attempt generation.

Successful page coverage must use these exact values:

| Field | Values and meaning |
|---|---|
| `scope`, `host` | `local`; nullable host ID, never allocated by a read |
| `runs` | `complete` for an opened owner store, including zero matches; `empty` for missing/uninitialised state |
| `chairs` | `complete` when the index opened and every requested association read was valid or unbound; `partial` for lookup errors/digest mismatches; `unavailable` when the index cannot open, including missing/incompatible state |
| `observations` | `complete` when all rows' owner reads succeeded (including an empty page); `partial` when some succeeded; `unavailable` when none succeeded on a nonempty page |
| `nested_runs`, `estimated_cost`, `verified_links` | `unavailable`; no isolated child-store/peer traversal or inferred facts |

Artifact descriptors must be bounded locators, never assertions of present file
bodies. Board reads must never read or return prompt, result or transcript
bodies. The existing 1 MiB envelope must refuse oversized pages without writing
overflow artefacts and advise reducing the limit. If one row still exceeds the
cap at limit 1, clients must use bounded `provenant output ATTEMPT` reads; this
surface cannot render that row. A 100-row page must meet p95 ≤1 second, including
with at least 100,000 retained attempts. Tests measure the indexed reader at
100,000 attempts and the full console page on a small fixture. The full board
at 100,000 attempts, including per-row observation lookups, still needs a
measurement on a real host before this bound counts as verified.

- Why: an active board must remain pageable while work changes, without blob
  scans, invented usage or one damaged ancillary checkpoint hiding other lanes.
- Test: `crates/cli/src/console/board.rs::tests::board_paging_end_to_end_and_100_row_p95`;
  `crates/run/src/board.rs::tests::board_paging_snapshot_and_metadata`;
  `tests/catalogue_read_only.rs::board_read_only`;
  `src/console.rs::tests::oversized_single_board_row_explains_recovery`
- Owner: `crates/cli/src/console/board.rs`, `crates/run/src/board.rs`, `src/console.rs`, `src/lib.rs`

### Project and session catalogue

`console projects` lists registered project IDs and their canonical Git common
directories. Linked worktrees share a binding. `console sessions [SESSION]`
lists only explicitly registered chairs, with the discovery record and its
checkpoint consistency. Summary is null with `summary_status: unavailable`;
provider, lifecycle and lane facts are not inferred. Legacy checkpoints without
discovery remain readable through `workflow session show` and are not chairs
inferred by the catalogue. Both commands cover local state only. `--project`
selects an existing binding by checkout path; without it all registered local
projects are included. Reads create no state or identity and do not repair
stale discovery. The registry is limited to 16,384 entries (excluding its
lock); an invalid project entry name fails the whole catalogue. Valid
in-progress binding stages are skipped.

`chair summary show SESSION` reads the current checkpoint summary without
allocating identity. `chair summary update SESSION --file PATCH.json
--expect-revision N` applies selected fields under the checkpoint save lock.
Revision 0 creates a summary; stale edits return `SUMMARY_STALE` with the
current summary revision. The returned checkpoint includes publication warnings.
`chair summary diff SESSION --from N [--to N]` compares retained summary revision
numbers, defaulting to the current revision and reporting unavailable revisions
if pruned. Summary show includes a
bounded inbox comparison for the local host, project and chair. Coverage marks
incomplete scans and unavailable host identity; references attributed elsewhere
or not accounted for by an incomplete scan are unknown.

`chair viewer` stores local presentation state under an explicit viewer name.
Request state is separate from resolution and blocking. A rendered summary
acknowledgement names the revision displayed; comparison uses retained summary
history and reports a pruned baseline as unavailable. Acknowledged revisions are
monotonic; `chair viewer forget VIEWER SESSION` clears a baseline when a session
has restarted at an earlier revision. Summary responses include `to`, `summary`
and `coverage`: an unavailable session has no summary, and a baseline ahead of
the current revision reports `baseline_ahead` while returning the current summary.
An explicit `--to` remains visible even when the summary is unavailable.
Closed or expired requests refuse presentation-state writes. Closed and expired
request state is reclaimed on successful writes for that viewer. Each viewer is limited to 10,000 request
rows and 10,000 summary baselines per project; the project also has a combined
limit of 100,000 viewer rows. New rows are refused at capacity, preserving
existing baselines; forget an unneeded baseline to free space.

The console may list, show and answer requests on a configured owner host
using the inbox CLI's `--host` or a `host:request` ID. Answers are human acts
recorded by the owner with source host and peer actor provenance. Operation IDs
bind retries and changed content conflicts. Close and acknowledgement remain
owner-local operations and remote attempts return `owner_local_only`; run them
on the owner or in the owning chair session. No delegation is implied.

`console overview` combines local pending request titles and recent lane
attention in one bounded read. Project labels use the registered Git common
directory's parent name; unavailable labels and task titles are null. Inbox
requests have no task association, so their task label is always null. Each
request includes its addressed `chair_session_id` from the inbox routing
column, even when the record omits it. These rows remain in the scan and in
the needs-you count. Bodies, prompts and results are excluded. The inbox scan
returns at most 100 requests; the run scan examines at most 128 recent
attempts, deduplicating retries. Both
scans share a 1 MiB budget for stored record bytes, with inbox records first;
reaching the budget marks the affected source partial.
Counts describe the observed rows and are partial when their source reports
`partial`. Running includes queued, starting, running and settling attempts;
attention includes terminal outcomes other than success or cancellation.
Missing or unreadable owner stores report `unavailable` with null
counts; incompatible schemas report `incompatible`. Nested run stores and
peers are not included. The overview never creates, initialises or migrates an
owner database. SQLite may create or update its own WAL and shared-memory
companions when reading an existing database, and committed WAL data is visible.

Session pages default to 50 rows and cap at 100. The cursor binds the selected
project, session filter, page size and observed session records; an invalid or
changed snapshot requires restarting without the cursor. A scan is limited to
16,384 directory entries across projects, including invalid or unregistered
names. Exhaustion conservatively sets `scan_truncated` and `truncated`; it does
not provide a cursor beyond the scanned subset. Directory iteration order is not
guaranteed, so a repeated truncated scan can invalidate its cursor. An oversized
result over 1 MiB is refused without publishing an artefact. Unreadable
owner records return an error rather than implying complete coverage. Directory
paths are tagged as UTF-8 or hex-encoded native bytes.

- Test: `tests/catalogue_read_only.rs::catalogue_read_only`
- Test: `tests/overview_needs_you.rs::overview_needs_you`
- Owner: `crates/cli/src/console.rs`, `src/console.rs`

## Parsing

### Pure entry points, one spelling

`help`, `--version` and `root` resolve no checkout, open no database, start
no thread, probe no provider and create no state; the MCP server starts only through `fabric mcp`. There are no aliases or abbreviated options; unknown,
duplicate or conflicting options fail with exit 2, and paths stay raw bytes.
- Why: scripts call these constantly, and an alias can turn a typo into a
  different authority.
- Test: `tests/parser.rs::help_and_version_need_no_roots_and_catalogue_help_drives_every_leaf`;
  `crates/cli/src/lib.rs::byte_paths_survive_clap_without_text_conversion`
- Owner: `crates/cli/src/lib.rs`

### Targets and manifests never guess

An ambiguous target fails and lists candidates rather than picking the newest.
CLI and Fabric submissions share one typed request model. Manifest validation
rejects conflicting task settings rather than silently choosing a source.
- Why: cancel and resume act on exactly one attempt, and one request shape
  serves CLI and MCP.
- Test: `crates/run/src/service.rs::bare_target_resolves_attempts_and_unambiguous_runs`;
  `crates/cli/src/run.rs::manifest_rejects_flat_route_fields`
- Owner: `crates/run/src/service.rs`, `crates/cli/src/run.rs`

### Status targets

`status` lists active lanes and the twenty most recent settled lanes. An optional
run, qualified task or attempt target uses the shared ID resolver and includes
older terminal records. `--all` includes full records and supports pagination;
`lanes --all --id ID` is also valid. The output ends with the project's
`review-exempt paths:` line (`review_exempt` in `--json`), the paths a writer
may change without ending `review_required`.
- Test: `tests/parser.rs::core_run_reports_generated_operation_and_status_is_newest_first`
- Owner: `crates/cli/src/run.rs`

### Output references and bounded tails

Output resolves the requested attempt on its owning host. Reads are bounded,
and a clamped tail reports its effective bound. The generated
[CLI reference](../reference/cli.md) owns target syntax and read limits.
- Why: a chair can inspect a precise attempt without copying a full target or
  opening a shell on its host.
- Test: `tests/parser.rs::run_leaves_accept_control_and_report_work_refused_per_task`;
  `tests/fabric_transport.rs::read_only_dispatch_places_on_a_peer_and_forwards_by_id`
- Owner: `crates/cli/src/run.rs`, `crates/cli/src/run/remote.rs`

### Settled attempt provenance receipts

`output TARGET --part receipt` prints one local, settled attempt's applied
route, resolution source and canonical reviewer attribution. An attempt target
is exact; `RUN` or `RUN/TASK` is accepted only when it selects exactly one
attempt. Ambiguous targets,
nonterminal attempts and peer transport are refused. `--trace` conflicts with
`--part receipt`.

The full human output has this shape (compact output adds a next command):

```text
Reviewer: codex/gpt-6.1-sol@high (openai; resolved from explicit), run RUN/TASK/ATTEMPT
Reviewer provenance: observed, actor ACTOR, model codex/gpt-6.1-sol, family openai
```

A canonical task with a writer request uses `Writer:` instead of `Reviewer:`.
The resolution source is `explicit`, `pool NAME`, `fallback` or `unknown`.
A matching recorded pool pick proves `pool NAME`; an applied model matching the
requested model proves `explicit`. A different applied model proves `fallback`
only when it matches a canonical task fallback and the requested route was
recorded as skipped. Other records print `unknown`.

A settled attempt without a recorded route still produces a receipt. Missing
route fields, unrecorded effort and uncatalogued model family print `unknown`;
no provider default effort is inferred. Model names are qualified with their
adapter before family lookup. The second line reports `AttemptFact.reviewer`
provenance and actor identity, with its observed model and catalogue family.
Missing reviewer fields print `unknown`.

`--json` returns `run_id`, `task_id`, `attempt_id`, `adapter`, `model`, `effort`,
`family`, `role` (`reviewer` or `writer`), `resolved_from`, the exact first
`receipt` line and second `reviewer_provenance` line without trailing newlines,
and `reviewer` with `provenance`, `actor_id`, `model` and `family`.
- Why: chairs can cite grep-able provenance without calling a writer a reviewer
  or guessing provider defaults.
- Test: `tests/dispatch_provider.rs::receipt_output_resolves_run_task_and_attempt`;
  `::receipt_output_classifies_writers_and_refuses_ambiguous_or_running_attempts`;
  `::receipt_output_reports_unknown_route_for_settled_lost_attempt`;
  `crates/cli/src/lib.rs::bounded_waits_counts_and_byte_windows`
- Owner: `crates/cli/src/run.rs`

### Positional route resolution

Route lookup resolves only a named route or an unambiguous catalogue model.
Unknown or ambiguous names report candidates and examples.
- Why: interactive route lookup should explain how to select a specific match.
- Test: `tests/route.rs`
- Owner: `crates/cli/src/route.rs`

## Results

### One bounded envelope, never truncated

The renderer checks result size before writing. An oversized result fails
with `RESULT_TOO_LARGE` (exit 3) and a pointer to a published file or paged
read; it never truncates a result
or reruns a mutation. Paged lists bind their cursor to the input digest and
preserve findings on every page. Human-readable output escapes terminal
control sequences, including in peer text. Envelope and exit-code contracts
belong to the [cross-cutting decisions](README.md#cross-cutting-decisions).
- Why: a result must be whole or absent, and a lane must not forge terminal
  output.
- Test: `crates/cli/src/render.rs::oversize_is_rejected_before_any_stdout_and_lf_counts`;
  `tests/checks.rs::cursors_recheck_all_inputs_and_later_pages_keep_findings_exit`;
  `crates/cli/src/render.rs::terse_quotes_and_escapes_anything_that_could_mislead_a_terminal`
- Owner: `crates/cli/src/render.rs`

### Each refusal has a stable code

`error.code` names the refusal, not only its category: a writer lane cap is
`LANE_CAP`, a disk below `gc.free_floor` is `DISK_FLOOR`, and so on. `status`
and the exit code carry the category. The run owner declares each code once,
with its meaning, in `ErrorKind`; `help --all --json` lists them under
`error_codes` and the generated [CLI reference](../reference/cli.md#error-codes)
renders that list. Tests assert the code and stable identifiers such as a
configuration key or repair command, never message prose. Other owners and
per-attempt launch refusals still use coarser codes.
- Why: agents and tests act on the code, and a reworded message must break
  neither.
- Test: `crates/run/src/service_tests.rs::each_refusal_has_its_own_stable_code`;
  `crates/cli/src/lib.rs::help_all_lists_each_error_code_once_with_its_meaning`;
  `tests/fabric_transport.rs::writer_disk_refusal_keeps_its_reason_through_graph_mcp_and_dispatch`
- Owner: `crates/run/src/service.rs`

### Waits are readers

Every wait has an explicit budget. A lane needing input can satisfy a wait.
Neither expiry nor cancelling a wait stops the work. The generated
[CLI](../reference/cli.md) and [MCP](../reference/mcp-tools.md) references
own the accepted bounds.
- Why: an impatient reader must never kill a healthy lane.
- Test: `crates/cli/src/lib.rs::bounded_waits_counts_and_byte_windows`;
  `crates/supervisor/tests/custody.rs::cancelled_reader_preserves_healthy_custody_and_cancel_escalates`
- Owner: `crates/cli/src/lib.rs`

### Lane waits count only new settlements

An unqualified lane wait counts a lane only if it was unsettled when the wait started
or settled since, so a settled older attempt never satisfies a wait on a newer
one. The wait starts at command entry, not at the first answer, so a lane
that settles while a peer is unreachable still counts when the peer returns.
Time-filtered waits select by settlement time rather than queue order. An
exactly named attempt counts whenever it settled. If every named lane has
already settled, the wait returns their terminal rows immediately, including
rows older than `--since`. Mixed pending and settled selections keep the
pending-only rule. Session selection narrows
listings and waits consistently, including on peers. An unqualified wait
resumes after the calling session's last acknowledged settlements. A committed
terminal attempt remains `settling` in local and peer wait views until its
custody release is durable; a terminal wait result therefore permits host pickup.

Each poll shares one deadline across all peer reads, including lookups of
previously observed running lanes that have moved off the page. Reads consume
the remaining budget, including child termination and reap time. A zero-budget
wait still makes its first bounded read. Failed or incomplete bounded scans
refuse rather than silently omitting lanes.
- Why: a stale settlement must never wake a chair that waits for newer work,
  and selecting by session removes hand-copied ID lists.
- Test: `crates/cli/src/run/watch.rs::a_settled_older_attempt_never_satisfies_a_wait_on_a_newer_one`;
  `::a_named_attempt_counts_whenever_it_settled`;
  `::since_counts_settlements_before_arming`;
  `::already_settled_named_lanes_return_their_terminal_rows`;
  `::a_fetch_gets_the_time_left_and_none_starts_after_the_deadline`;
  `::arming_stops_retrying_at_the_deadline_with_the_last_cause`;
  `::since_selects_by_settlement_time_not_queue_order`;
  `::since_refuses_a_scan_past_its_cap_instead_of_missing_lanes`;
  `::a_lane_that_settles_during_initial_peer_failures_still_counts`;
  `::a_running_lane_pushed_off_the_page_keeps_the_wait_open`;
  `::a_lane_that_settles_off_the_page_is_kept_in_the_result`;
  `crates/cli/src/run/remote.rs::a_peer_call_is_capped_by_the_time_left_and_the_peer_limit`;
  `::wait_starts_no_fetch_after_its_deadline`;
  `::every_read_of_a_fetch_gets_only_the_time_the_earlier_ones_left`;
  `::no_read_starts_at_or_after_the_deadline`;
  `::a_zero_second_budget_still_makes_its_one_read`;
  `::the_missing_lane_lookup_shares_the_first_reads_deadline`;
  `crates/cli/src/lib.rs::lanes_session_and_watch_have_bounded_shapes`;
  `tests/parser.rs::lanes_watch_and_session_selection_report_an_empty_store`
- Owner: `crates/cli/src/run/watch.rs`, `crates/cli/src/run.rs`, `crates/cli/src/run/remote.rs`

### Lane watch is one stable line per event

A lane watch uses the same selection rules as a wait and emits each observed
state transition once. Event lines contain stable identifiers rather than free
text and are flushed immediately. Newly dispatched lanes join the watch.
The final event distinguishes settlement from timeout.
- Why: a chair must be told when work settles without polling for it.
- Test: `crates/cli/src/run/watch.rs::watch_emits_each_state_once_and_ends_when_all_settle`;
  `::watch_times_out_with_pending_lanes_and_names_the_host`
- Owner: `crates/cli/src/run/watch.rs`, `src/lib.rs`

### Status selection

Status prioritises active work and recent settlements. Human and structured
output use the same selection; historical reads are paged.

## Guards

### Merge guard

Landing merges only the head it verified. When the caller pins a reviewed
head, a mismatch refuses before checks or merge. Otherwise, verification
captures the current head. Reports retain the full verified object ID.
`pr watch` and `land` need at least one passing check (the rest passing or
skipped), refuse a head change during verification and fail closed on
incomplete evidence.
- Why: a merge must land exactly the head that CI verified.
- Test: `tests/pr.rs::land_refuses_head_changes_before_and_after_checks`;
  `::land_refuses_unpassed_missing_and_malformed_checks`
- Owner: `src/pr.rs`

### Landing adds no merge authority

`land NUMBER` checks CI once; `--wait` waits up to one hour and
`--wait=30m` selects a bounded limit up to four hours. Landing verifies
required-check registration using check runs and legacy statuses addressed by
the pinned commit SHA, reading every page, and checks the exact head around
each sample. A required check matches by name and, when the ruleset or
protection names an app, by that app too. A skipped required check is
unavailable, not passing. When no check is required, landing needs every
reported check run and status to be complete and passing and waits a 60-second
settle interval after the newest registration with nothing new registering.
Transient `gh` failures (5xx, rate limit, network) are retried with backoff
until the wait deadline; other failures fail closed. Progress is capped at
eight stderr lines.

After any merge error or timeout, and after a failed post-merge read, landing
re-reads the PR. If it is merged, the report is landed (with a `merge_note`),
never `MERGE_FAILED`. A merge that is accepted but not yet effective (merge
queue or auto-merge) is outcome `pending`, `result: pending` and exit code 5.
A merge with cleanup pending exits 0 and says so in the compact output.

`pr status` reads head and checks from one snapshot per PR; `--wait` blocks
until that snapshot changes and exits 124 if unchanged at the limit, returning
the latest good snapshot. A transient error row is neither a change nor a
replacement for a good row. A failed lookup preserves other PR rows.

After the guarded merge, landing follows up locally: fetch the base, fast-forward the
local base only in a checkout with no changes, delete declared build output
and remove the branch's registered worktree through `worktree remove`, and
delete the merged branch. The two cleanup steps act only
on a local branch of the pull request's name whose tip is exactly the head the
pull request merged: a head in another repository (a fork) leaves a same-named
local branch and its worktree alone, so does a branch recreated or advanced to
any other tip (even one the base contains), and a local branch whose tip the
fetched base does not contain is refused and keeps its worktree as well as the
branch. Removal carries that head and refuses, under the worktree lock, if the
worktree's HEAD has moved since it was checked. When the selected project is
that worktree, cleanup uses the verified primary checkout as its repository
anchor, process cwd and output project. Canonical paths and NUL-delimited Git
inventory handle symlink aliases and quoted paths. When Git lists a separate
administrative directory instead of the primary checkout, `land` also checks
`core.worktree` or a containing checkout. If neither identifies the primary
checkout, landing reports an inventory error. The landing worker census
ignores itself and only its ancestor chain through the session leader; other
observable live processes inside the lane refuse removal. An incomplete census
or unknown ancestor hop refuses landing with a retry message; each exempted
ancestor requires a strictly older native start stamp. Landing holds the writer
flock while checking dirty and ignored output and
removing the checkout. Lock contention reports busy. Per-attempt recovery
records remain for Run and do not gate removal.
The branch deletion carries the head too:
with no registered worktree it refuses a branch that any worktree has
checked out, is rebasing (paused or not) or is bisecting, as `git branch -d`
does (read from Git's administrative directories, so a missing or unmounted
checkout still holds its branch and an unreadable entry refuses), then deletes
with `git update-ref -d <branch> <head>`, which compares
the branch against the merged head and deletes in one ref transaction, so a
branch moved after the check is kept. (A checkout, rebase or bisect of the
branch that begins between that test and the deletion is not caught.) A failure
before the merge changes nothing. After it, each step refuses and reports
instead of forcing. A successful merge exits 0 with `merged: true`; incomplete
cleanup adds `cleanup_pending: true` and keeps each refused step's reason.
Retrying an already-merged PR bypasses CI and merge and retries safe cleanup.
- Why: a landing is four routine steps, and none of them may widen what was
  reviewed or delete work that is not merged.
- Test: `src/pr_land_tests.rs::pr_land_merges_then_fetches_fast_forwards_and_prunes`;
  `::pr_land_prunes_the_selected_merged_worktree`;
  `::pr_land_from_lane_ignores_only_its_callers`;
  `::pr_land_refuses_incomplete_worker_census`;
  `::pr_land_canonicalises_quoted_worktree_paths`;
  `::pr_land_uses_writer_lock_and_preserves_attempt_recovery_evidence`;
  `::pr_land_preserves_terminal_writer_evidence_in_parent_and_child_stores`;
  `::pr_land_primary_with_separate_git_directory`;
  `::pr_land_refuses_each_unsafe_step_and_reports_the_rest`;
  `::pr_land_changes_nothing_when_the_merge_does_not_happen`;
  `::pr_land_leaves_a_local_namesake_of_a_fork_branch_alone`;
  `::pr_land_leaves_a_reused_branch_name_alone`;
  `crates/confinement/src/worktrees.rs::named_removal_refuses_a_head_that_moved_after_the_callers_check`
- Owner: `src/pr/land.rs`, `src/worktrees.rs`, `crates/confinement/src/worktrees.rs`

### Pull request bodies come from receipts

Pull request creation fills the repository template from a local settled
attempt and its delivery receipt. The recorded head must match the delivered
remote branch. The body identifies the head reviewed, route provenance and
gate evidence; absent evidence is stated explicitly. Missing required template
sections, unsuitable attempt states and peer-owned records refuse creation.
The generated [CLI reference](../reference/cli.md) owns invocation syntax.
- Why: the chair rebuilt the same body by hand for every pull request, and a
  hand-built body can state a head or route the receipts do not hold.
- Test: `crates/cli/src/pr_body.rs::the_repository_template_is_filled_from_the_receipts`;
  `::a_review_of_another_head_or_no_review_is_stated_not_hidden`;
  `::a_template_missing_a_section_is_refused`;
  `src/pr_land_tests.rs::pr_open_refuses_what_the_receipts_cannot_support`;
  `tests/pr.rs::pr_status_prints_one_compact_line_per_pull_request`
- Owner: `crates/cli/src/pr_body.rs`, `src/pr/open.rs`, `src/pr/status.rs`

### Chair switch quoting

`chair switch` single-quotes each handoff argument and writes apostrophes
and backslashes as double-quoted characters, so `sh`, `bash`, `zsh` and `fish` read
them literally.
- Why: a path or session ID must never become shell syntax.
- Test: `src/chair.rs::chair_quote_encodes_apostrophes_and_backslashes`;
  `::chair_command_and_hook_run_the_same_in_every_shell`
- Owner: `src/chair.rs`

### Bash guard fails closed

One static `HookSpec` table drives hook dispatch, fixtures and the
installer; hook stdout carries only the protocol response and no hook
launches a provider. The pre-tool-use guard reads at most 1 MiB in one
second; oversized, malformed, unreadable or timed-out input is denied, and if the
denial cannot be written it exits 2. It never echoes or persists input.
- Why: a guard that allows on error is no guard.
- Test: `tests/hooks.rs::unreadable_and_stalled_input_cannot_allow_bash`;
  `::denial_write_failure_uses_recorded_exit_two_fallback`
- Owner: `crates/cli/src/hooks.rs`, `src/hooks.rs`

### Bash guard policy

The guard refuses only irreversible actions, naming the alternative with an
accepted one-line example: a force
push to the default branch, and recursive `rm` of the home directory, the
repository root or the Provenant state directory (or a parent of any). It
applies the configured rule IDs `force-push`, `rm-root` and `rm-state` to every
simple command, looking through wrappers and shells. Process control, dynamic
command names, `sleep`, `git stash`, `reset --hard`, `clean -f` and branch
changes are allowed. Unreadable or invalid config keeps the default rules on.
The guard is advisory: confinement and custody stay authoritative.
- Why: an aligned model is trusted with everything it can undo; config damage
  must not disable protection.
- Test: `tests/hooks.rs::unreadable_guard_config_keeps_the_default_rules_on`;
  `crates/confinement/src/guard.rs::force_pushes_to_the_default_branch_are_refused`;
  `::every_refusal_shows_an_accepted_example`
- Owner: `crates/confinement/src/guard.rs`, `src/hooks.rs`

### Pre-push refuses data loss

`hook git pre-push` refuses default-branch deletion, non-fast-forward
updates, pushes from linked worktrees to the default branch and conflict
markers, each with an accepted example on stderr. It identifies the default
branch from the remote's advertisement, refuses when any inspection fails,
disables inherited redirects and never fetches.
- Why: the last local guard before shared history changes.
- Test: `tests/hooks.rs::pre_push_uses_real_git_to_refuse_data_loss_and_conflict_markers`
- Owner: `src/hooks.rs`

### Claude worktree hooks go through the worktree owner

`hook claude worktree-create` and `worktree-remove` call `worktree create
claude/<name>` and `worktree remove <name>`; they create and delete nothing
themselves. Create prints only the new absolute path on stdout and accepts a
plain slug only. Remove acts only on a linked worktree the owner lists for
the project and that this hook created (both ownership records present, equal
and naming the path; a later commit does not matter), never passes `--discard`, and treats any owner refusal (dirty,
live writer, primary) as a failure that leaves the worktree. Both exit 2 with
a bounded stderr cause on refusal.
- Why: a worktree Claude creates must have an owner from the start, and a
  hook must not widen what removal may delete, so a clean tree a person made
  is never removed on Claude's say-so.
- Test: `tests/hooks_session.rs::worktree_hooks_create_and_remove_through_the_worktree_owner`;
  `::worktree_hooks_refuse_bad_names_the_primary_and_foreign_paths`;
  `::worktree_remove_keeps_a_listed_clean_tree_the_hook_did_not_make`;
  `::worktree_remove_accepts_an_owned_tree_after_a_commit`
- Owner: `src/hook_worktree.rs`

### Checks report, never repair

`check` exits 1 on findings, 2 on malformed input and 3 on unreadable
evidence. `check release` reads Git with redirects, replacements,
fsmonitor, textconv and lazy fetch off, and a missing object never yields a
clean result. No check fetches or rewrites.
- Why: a release scan that passes on missing evidence would publish secrets.
- Test: `tests/checks.rs::release_ignores_grafts_and_rejects_missing_ancestry_evidence`;
  `::release_scans_added_then_deleted_blobs_and_commit_identity_bytes`
- Owner: `crates/cli/src/check/`, `src/checks.rs`

## Not provided

- Separate `batch`, `reap` or `fabric dispatch` commands: run submission covers them.
- `worktree remove --force` or remote branch deletion.
- `commit-msg`, Codex or OpenCode hooks: no recorded protocol.
