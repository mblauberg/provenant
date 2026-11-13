# Command surface

Owner: `crates/cli` (grammar, rendering), `src/` (composition, hooks)
Anchor: `tests/parser.rs`, `crates/cli/src/render.rs`, `tests/catalogue_read_only.rs`

One executable, `provenant`, serves people, skills, hooks and CI. The `cli`
crate parses arguments, calls typed library operations and renders typed
results. This spec owns the envelope, exit codes and wait budgets. The machine-readable help
catalogue owns command syntax: `provenant help` covers the core verbs and
global flags, each command's help shows every non-hidden flag, and
`provenant help --all` renders every public command and its non-hidden flags.
The `--json` catalogue lists every public leaf, flag and bound.
Paths are relative to `tools/front-door/`.
Numeric range and requirement builders in `crates/cli/src/metadata.rs` install
clap rules and retain their catalogue metadata from the same declaration;
clap does not expose those rules through public getters. Compact help uses
command markers and the grammar's descriptions. Graph submission uses the same
template compiler for CLI and MCP.

Waits, lane watches and time-based `--since` filters share a whole-number span
grammar: bare seconds or the suffix `s`, `m`, `h` or `d`. Waits and watches
retain their four-hour runtime limit; since filters also accept dates and
RFC3339 timestamps. Git-revision and console-cursor `--since` options keep
their own types.

## Attempt projections

`status`, `output`, `stop` and `land` use the shared lane row for
attempt identity, state, typed settlement cause and retry lineage. A missing
historical fact remains unavailable. A host path is the tagged structured path
used by `worktree list --json`, including its native-byte encoding when needed.
Land reports retained local contributors when that evidence is available.
Generated schemas describe the machine envelopes and are checked against the
current lane field declarations.

## Agent output

Ordinary human results default to a compact summary. The command's owner
supplies a bounded typed summary: counts by state, action rows first, an explicit
omission count and one typed `next` command. A command without an owner
summary falls back to a generic truncator that shows text of up to 2 KiB whole
and otherwise says how many lines it omitted. `next` is the only source of the
next command: when a command has none, none is printed. Errors keep their
repair command and are never compacted.

The default `status` listing never omits an active or needs-attention lane;
only settled lanes are left out, with their count. The `run` receipt prints one
`lane: GRAPH/NODE/ATTEMPT` line per launched node (`--json`: `lane_ids`). A
launch refused before admission prints its recorded route and reason.

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

The core verbs are `run`, `status`, `output`, `steer`, `stop`, `retry`,
`edit`, `templates`, `land` and `quota`. `run [BRIEF] --use TEMPLATE` submits a
graph; `--file` accepts a graph document or text brief and `--explain` inspects
compilation without starting work. `--wait` observes settlement. Inside a lane,
`run` adds a child node. `retry` continues or redoes a target; `stop` requests
cancellation. `output --trace` shows the timeline and `edit --file PATCH`
applies a revisioned graph patch. The help catalogue owns exact arguments.

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

- Test: `crates/cli/src/console/peer.rs::peer_board_coverage`
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

- Why (default): nested work must remain visible without inventing edges or complete totals.
- Test: `crates/cli/src/console/tree.rs::host_tree_preserves_lineage_detail_and_usage`
- Owner: `crates/cli/src/console/tree.rs`, `crates/run/src/board.rs`

### Board snapshot and cursor contract

`console board` reads indexed host-local attempts in descending immutable
sequence order, preserving retries as separate rows. A cursor fixes the first
page's high-water ceiling and binds the state root, host, project and filters.
It is opaque and detects edits; it grants no authority. New attempts cannot
enter an existing walk. Each matching attempt appears once as its range is
read, but later state changes may remove it from an active-only selection.
Replaying a cursor re-reads current facts rather than a frozen snapshot.
Invalid or incompatible cursors require a fresh walk.

Indexed rows, ceiling validation and generation come from one SQLite read
transaction. Every board-visible write appends an event transactionally;
pruning preserves the latest event per attempt. Generation discovery uses
indexed lookups, not history-blob scans. Ancillary observations cannot replace
indexed execution facts, and a damaged observation must not hide a valid row.

Rows preserve unknowns and complete owner metadata. Chair associations are
immutable host facts checked against the request digest; provider sessions
never imply a chair. Usage totals distinguish known zero from missing data.
No Git probing, issue/PR inference, nested-store traversal or body reads occur.
Artifact descriptors locate bounded reads and never prove the body is present.

Reads allocate no product state or migrations; SQLite bookkeeping is permitted.
Missing state is empty; unsafe, corrupt or incompatible state fails explicitly.
Coverage distinguishes missing stores, partial observations and unavailable
facts. The result cap refuses an oversized page without writing an overflow
artifact; a single oversized row must be inspected through bounded output reads.
[Board projection](../../tools/front-door/crates/cli/src/console/board.rs#read)
owns payload and cursor encoding; [indexed reader](../../tools/front-door/crates/run/src/board.rs)
owns selection and generation. A 100-row full-board page must meet p95 ≤1 second with at least 100,000
retained attempts. That full-board target has not been verified on a real host.

- Why (default): an active board must remain pageable while work changes, without blob
  scans, invented usage or one damaged ancillary checkpoint hiding other lanes.
- Test: `crates/cli/src/console/board.rs::board_paging_end_to_end`;
  `crates/run/src/board.rs::board_paging_snapshot_and_metadata`;
  `tests/catalogue_read_only.rs::board_read_only`;
  `src/console.rs::oversized_single_board_row_explains_recovery`
- Owner: `crates/cli/src/console/board.rs`, `crates/run/src/board.rs`, `src/console.rs`, `src/lib.rs`

### Project and session catalogue

`console projects` lists existing Git-common-directory bindings; linked
worktrees share one project. Session catalogues list only explicitly registered
chairs and report checkpoint consistency. Legacy checkpoints remain readable
without being inferred as registrations. Reads allocate no identity, repair
nothing and report unreadable records and scan exhaustion explicitly.
Session cursors bind the selection and observed records; changed snapshots
require a fresh walk. Tagged paths preserve native bytes.

Workflow owns [summary revisions and inbox coverage](workflow.md#structured-summary-revisions-and-inbox-coverage).

Remote request answers are human acts committed by the configured owner with
source-host and actor provenance. Retry IDs bind content. Closing and
acknowledgement stay owner-local and grant no delegation.

The overview combines bounded request titles and recent attempt attention,
excluding bodies, prompts and results. Counts describe observed rows; partial,
unavailable and incompatible sources remain distinct. Missing labels and task
associations stay null. It creates no owner state or migrations. The
[console owner](../../tools/front-door/crates/cli/src/console.rs) owns payloads,
scan budgets and pagination.

- Test: `tests/catalogue_read_only.rs::catalogue_read_only`;
  `tests/overview_needs_you.rs::overview_needs_you`
- Owner: `crates/cli/src/console.rs`, `src/console.rs`

## Parsing

### Pure entry points, one spelling

`help`, `--version` and `root` resolve no checkout, open no database, start
no thread, probe no provider and create no state; the MCP server starts only through `fabric mcp`. There are no aliases or abbreviated options; unknown,
duplicate or conflicting options fail with exit 2, and paths stay raw bytes.
- Why (default): scripts call these constantly, and an alias can turn a typo into a
  different authority.
- Test: `tests/parser.rs::help_and_version_need_no_roots_and_catalogue_help_drives_every_leaf`;
  `crates/cli/src/lib.rs::byte_paths_survive_clap_without_text_conversion`
- Owner: `crates/cli/src/lib.rs`

### Targets and manifests never guess

An ambiguous target fails and lists candidates rather than picking the newest.
CLI and Fabric submissions share one typed request model. Manifest validation
rejects conflicting task settings rather than silently choosing a source.
- Why (default): cancel and resume act on exactly one attempt, and one request shape
  serves CLI and MCP.
- Test: `crates/run/src/service.rs::bare_target_resolves_attempts_and_unambiguous_runs`
- Owner: `crates/run/src/service.rs`, `crates/cli/src/run.rs`

### Status targets

Status selection follows [status selection](#status-selection). An exact run,
task or attempt target uses the shared resolver and may select older terminal
records. The output reports the project's review-exempt paths from configuration;
it never invents an uncommitted-work exemption.

- Test: `src/fabric/unified.rs::unified_status_uses_one_row_contract_and_selects_attempts`
- Owner: `crates/cli/src/run.rs`

### Output references and bounded tails

Output resolves the requested attempt on its owning host. Reads are bounded,
and a clamped tail reports its effective bound. `provenant help --all` owns target syntax and read limits.
- Why (default): a chair can inspect a precise attempt without copying a full target or
  opening a shell on its host.
- Test: `crates/cli/src/run/tests_access.rs::paged_output_never_splits_a_character`;
  `src/fabric/unified.rs::peer_output_follows_host_qualified_subtree_attempts`
- Owner: `crates/cli/src/run.rs`, `crates/cli/src/run/remote.rs`

### Settled attempt provenance receipts

An output receipt identifies one local settled attempt's applied route,
resolution source and canonical reviewer attribution. Ambiguous, nonterminal
and peer-owned targets refuse rather than guess. A canonical writer request
is labelled as a writer, never a reviewer.

Only a matching recorded pool pick proves a pool selection; only a matching
applied request proves an explicit model. Fallback requires both a canonical
fallback match and evidence that the requested route was skipped. Missing
route, effort or family evidence remains unknown: no provider default is
inferred. Reviewer identity comes from `AttemptFact.reviewer`, independently
of the route projection. The
[receipt renderer](../../tools/front-door/crates/cli/src/run.rs) owns field and
text encoding.

- Why (default): chairs can cite grep-able provenance without calling a writer a reviewer
  or guessing provider defaults.
- Test: `tests/graph_provider.rs::receipt_output_resolves_run_task_and_attempt`;
  `tests/graph_provider.rs::receipt_output_classifies_writers_and_refuses_ambiguous_or_running_attempts`;
  `tests/graph_provider.rs::receipt_output_reports_unknown_route_for_settled_lost_attempt`;
  `crates/cli/src/lib.rs::bounded_waits_counts_and_byte_windows`
- Owner: `crates/cli/src/run.rs`

### Positional route resolution

Route lookup resolves only a named route or an unambiguous catalogue model.
Unknown or ambiguous names report candidates and examples.
- Why (default): interactive route lookup should explain how to select a specific match.
- Test: `tests/route.rs`
- Owner: `crates/cli/src/route.rs`

## Results

### One bounded envelope, never truncated

The renderer checks result size before writing. An oversized result fails
with `RESULT_TOO_LARGE` (exit 3) and a pointer to a published file or paged
read; it never truncates a result
or reruns a mutation. Paged lists bind their cursor to the input digest and
preserve findings on every page. Human-readable output escapes terminal
control sequences, including in peer text. The [renderer](../../tools/front-door/crates/cli/src/render.rs#exit_status)
owns exit mapping: success/waiting 0, failure/refusal 1, invalid input 2,
internal error 3, unavailable 4, pending 5, timeout 124 and cancellation 130.
The versioned JSON envelope carries command, status, data, warnings and error;
unknown facts remain null and commands never silently truncate machine results.
- Why (default): a result must be whole or absent, and a lane must not forge terminal
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
`error_codes`. Tests assert the code and stable identifiers such as a
configuration key or repair command, never message prose. Other owners and
per-attempt launch refusals still use coarser codes.

Run refusals may include `category`, `detail` and `next` alongside the
established error envelope. These are bounded, terminal-safe facts selected by
the owner. The local CLI can name an absolute worktree or refused grant;
remote diagnostics need a repository or home anchor or use an inspection
action. Provider and skipped-route reasons project typed facts, without copying
captured output, account directories or source errors into diagnostic fields.
Unknown account references use `UNKNOWN_ACCOUNT`; pacing profiles
that resolve to the same account use `DUPLICATE_PACING_PROFILE`. Identity or
credential settings that cannot be synced use `SHARED_SETTINGS_NEVER_SYNCED`;
the shared-settings owner supplies the corrective action. Resource
projections name the observed limits and the relevant setting or operating-system
limit; disk-floor, memory-pressure and configured writer-lane refusals identify
the host configuration or shipped default that supplied the limit. Total-lane and writer-lane caps
identify their separate configured settings. A
process-limit projection names the operating-system soft limit and shipped
reserved headroom. Actions name the condition to correct. A writer
with unknown custody remains refused until custody is checked.
A stale selected-account availability observation emits `STALE_READINESS` as
a warning and is re-probed once. A healthy recheck keeps the selected route
and its fallback receipt; account and quota guards still apply.
- Why (default): agents and tests act on the code, and a reworded message must break
  neither.
- Test: `crates/run/src/service_tests.rs::each_refusal_has_its_own_stable_code`;
  `crates/cli/src/lib.rs::help_all_lists_each_error_code_once_with_its_meaning`
- Owner: `crates/run/src/service.rs`

### Waits are readers

Every wait has an explicit budget. A lane needing input can satisfy a wait.
Neither expiry nor cancelling a wait stops the work. `provenant help --all` and the MCP `tools/list` response own the accepted
bounds.
- Why (default): an impatient reader must never kill a healthy lane.
- Test: `crates/cli/src/lib.rs::bounded_waits_counts_and_byte_windows`;
  `crates/supervisor/tests/custody.rs::cancelled_reader_preserves_healthy_custody_and_cancel_escalates`
- Owner: `crates/cli/src/lib.rs`

### Waits count only new settlements

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
- Why (default): a stale settlement must never wake a chair that waits for newer work,
  and selecting by session removes hand-copied ID lists.
- Test: `crates/cli/src/run/watch.rs::a_settled_older_attempt_never_satisfies_a_wait_on_a_newer_one`;
  `crates/cli/src/run/watch.rs::a_named_attempt_counts_whenever_it_settled`;
  `crates/cli/src/run/watch.rs::since_counts_settlements_before_arming`;
  `crates/cli/src/run/remote.rs::a_peer_call_is_capped_by_the_time_left_and_the_peer_limit`
- Owner: `crates/cli/src/run/watch.rs`, `crates/cli/src/run.rs`, `crates/cli/src/run/remote.rs`

### Watch is one stable line per event

A lane watch uses the same selection rules as a wait and emits each observed
state transition once. Event lines contain stable identifiers rather than free
text and are flushed immediately. Newly dispatched lanes join the watch.
The final event distinguishes settlement from timeout.
- Why (default): a chair must be told when work settles without polling for it.
- Test: `crates/cli/src/run/watch.rs::watch_emits_each_state_once_and_ends_when_all_settle`;
  `crates/cli/src/run/watch.rs::watch_times_out_with_pending_lanes_and_names_the_host`
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
- Why (default): a merge must land exactly the head that CI verified.
- Test: `tests/pr.rs::land_refuses_head_changes_before_and_after_checks`;
  `tests/pr.rs::land_refuses_unpassed_missing_and_malformed_checks`
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
- Why (default): a landing is four routine steps, and none of them may widen what was
  reviewed or delete work that is not merged.
- Test: `src/pr_land_tests.rs::pr_land_merges_then_fetches_fast_forwards_and_prunes`;
  `src/pr_land_tests.rs::pr_land_prunes_the_selected_merged_worktree`;
  `src/pr_land_tests.rs::pr_land_from_lane_ignores_only_its_callers`;
  `crates/confinement/src/worktrees/lifecycle_tests/worktree_operations.rs::named_removal_refuses_a_head_that_moved_after_the_callers_check`
- Owner: `src/pr/land.rs`, `src/worktrees.rs`, `crates/confinement/src/worktrees.rs`

### Pull request bodies come from receipts

Pull request creation fills the repository template from a local settled
attempt and its delivery receipt. The recorded head must match the delivered
remote branch. The body identifies the head reviewed, route provenance and
gate evidence; absent evidence is stated explicitly. Missing required template
sections, unsuitable attempt states and peer-owned records refuse creation.
`provenant help --all` owns invocation syntax.
- Why: the chair rebuilt the same body by hand for every pull request, and a
  hand-built body can state a head or route the receipts do not hold.
- Test: `crates/cli/src/pr_body.rs::the_repository_template_is_filled_from_the_receipts`;
  `crates/cli/src/pr_body.rs::a_review_of_another_head_or_no_review_is_stated_not_hidden`;
  `src/pr_land_tests.rs::pr_open_refuses_what_the_receipts_cannot_support`;
  `tests/pr.rs::pr_status_prints_one_compact_line_per_pull_request`
- Owner: `crates/cli/src/pr_body.rs`, `src/pr/open.rs`, `src/pr/status.rs`

### Chair switch quoting

`chair switch` single-quotes each handoff argument and writes apostrophes
and backslashes as double-quoted characters, so `sh`, `bash`, `zsh` and `fish` read
them literally.
- Why (default): a path or session ID must never become shell syntax.
- Test: `src/chair.rs::chair_quote_encodes_apostrophes_and_backslashes`;
  `src/chair.rs::chair_command_and_hook_run_the_same_in_every_shell`
- Owner: `src/chair.rs`

### Bash guard fails closed

One static `HookSpec` table drives hook dispatch, fixtures and the
installer; hook stdout carries only the protocol response and no hook
launches a provider. The pre-tool-use guard reads at most 1 MiB in one
second; oversized, malformed, unreadable or timed-out input is denied, and if the
denial cannot be written it exits 2. It never echoes or persists input.
- Why (default): a guard that allows on error is no guard.
- Test: `tests/hooks.rs::unreadable_and_stalled_input_cannot_allow_bash`;
  `tests/hooks.rs::denial_write_failure_uses_recorded_exit_two_fallback`
- Owner: `crates/cli/src/hooks.rs`, `src/hooks.rs`

### Shell commands use confinement

The pre-tool-use hook validates its bounded event envelope and delivers pending
answers. It does not inspect shell syntax, load a shell-rule policy or refuse
commands by shape. Writer lanes rely on private Git, filesystem confinement,
credential custody, object verification and host authority gates.
- Why: shell command tables block ordinary work without enforcing ADR 0035's
  four boundaries.
- Test: `tests/hooks.rs::shell_commands_are_left_to_confinement_even_with_unreadable_config`
- Owner: `src/hooks.rs`

### Pre-push refuses data loss

`hook git pre-push` resolves pushed remote branch aliases to the advertised
default branch and refuses default-branch deletion, non-fast-forward updates to the default branch, pushes from linked worktrees to the default
branch and, by default, literal conflict markers, each with an accepted example on stderr. The
conflict-marker check is advisory and `[domains.run] push_conflict_markers =
false` turns it off; the other refusals remain. It identifies the default
branch from the remote's advertisement, refuses when any inspection fails,
disables inherited redirects and never fetches.
- Why (default): the last local guard before shared history changes.
- Test: `tests/hooks.rs::pre_push_refuses_non_fast_forward_to_remote_default_alias`;
  `tests/hooks.rs::pre_push_uses_real_git_to_refuse_data_loss_and_conflict_markers`
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
- Why (default): a worktree Claude creates must have an owner from the start, and a
  hook must not widen what removal may delete, so a clean tree a person made
  is never removed on Claude's say-so.
- Test: `tests/hooks_session.rs::worktree_hooks_create_and_remove_through_the_worktree_owner`;
  `tests/hooks_session.rs::worktree_hooks_refuse_bad_names_the_primary_and_foreign_paths`;
  `tests/hooks_session.rs::worktree_remove_keeps_a_listed_clean_tree_the_hook_did_not_make`;
  `tests/hooks_session.rs::worktree_remove_accepts_an_owned_tree_after_a_commit`
- Owner: `src/hook_worktree.rs`

### Checks report, never repair

`check` exits 1 on findings, 2 on malformed input and 3 on unreadable
evidence. `check release` reads Git with redirects, replacements,
fsmonitor, textconv and lazy fetch off, and a missing object never yields a
clean result. No check fetches or rewrites. Each line in
`crates/cli/src/check/release-fixtures.txt` has the form
`<path> <rule> <digest>`, where `digest` is the first 16 lowercase hex characters of
SHA-256 over the exact matched line bytes, excluding its newline. A finding
is suppressed only when all three fields match, in both the working tree and
history blobs. Every suppression is counted in scan evidence. An entry whose
path is tracked but matches no finding in the current tree is reported as a
stale fixture finding and must be removed or updated. After reviewing such a change, list any new
path and rerun the fixture unit test with `PROVENANT_BLESS_RELEASE_FIXTURES=1`; it rewrites
the digests for listed paths. Path and symlink checks remain active for
listed paths, and commit metadata checks are unaffected. Changes to this
allowlist require review.
- Why (default): a release scan that passes on missing evidence would publish secrets.
- Test: `tests/checks.rs::release_ignores_grafts_and_rejects_missing_ancestry_evidence`;
  `tests/checks.rs::release_scans_added_then_deleted_blobs_and_commit_identity_bytes`;
  `crates/cli/src/check/release.rs::reviewed_literal_is_suppressed_but_other_line_at_same_path_is_reported`;
  `crates/cli/src/check/release.rs::different_history_token_at_a_listed_path_is_reported`;
  `crates/cli/src/check/release.rs::stale_fixture_entry_is_reported_and_suppression_count_is_noted`
- Owner: `crates/cli/src/check/`, `src/checks.rs`

## Not provided

- `worktree remove --force` or remote branch deletion.
- `commit-msg`, Codex or OpenCode hooks: no recorded protocol.
