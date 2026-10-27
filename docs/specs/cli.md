# Command surface

Owner: `crates/cli` (grammar, rendering), `src/` (composition, hooks)
Anchor: `tests/parser.rs`, `crates/cli/src/render.rs`, `tests/hooks.rs`

One executable, `provenant`, serves people, skills, hooks and CI. The `cli`
crate parses arguments, calls typed library operations and renders typed
results. The [cross-cutting decisions](README.md#cross-cutting-decisions) define the envelope, exit codes and wait budgets. `provenant help --json` is the
authoritative catalogue, and the generated [CLI
reference](../reference/cli.md) lists every leaf, flag and bound. Paths are
relative to `tools/front-door/`.

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

`TARGET` is `RUN[/TASK[/ATTEMPT]]`, and an ambiguous name fails and lists the
candidates rather than picking the newest. A manifest must be exactly
`{schema_version:1,tasks:[…]}` with the tagged `route` object shared with
Fabric; flat route fields are rejected, and a manifest conflicts with the
per-task route, access and timeout flags.
- Why: cancel and resume act on exactly one attempt, and one request shape
  serves CLI and MCP.
- Test: `crates/run/src/service.rs::bare_target_resolves_attempts_and_unambiguous_runs`;
  `crates/cli/src/run.rs::manifest_rejects_flat_route_fields`
- Owner: `crates/run/src/service.rs`, `crates/cli/src/run.rs`

### Output references and bounded tails

`output` accepts a bare attempt, `RUN/ATTEMPT`, `HOST/ATTEMPT`, or the
three-part target. `--host` selects a configured peer's own attempt. Local
tails above 500 lines are clamped to 500 and the JSON result reports the
effective tail and whether it was clamped. Peer output retains its existing
part and tail restrictions.
- Why: a chair can inspect a precise attempt without copying a full target or
  opening a shell on its host.
- Test: `tests/parser.rs::run_leaves_accept_control_and_report_work_refused_per_task`;
  `tests/fabric_transport.rs::read_only_dispatch_places_on_a_peer_and_forwards_by_id`
- Owner: `crates/cli/src/run.rs`, `crates/cli/src/run/remote.rs`

### Positional route resolution

`route resolve NAME` accepts a route name or an unambiguous catalogue model
name, alongside the explicit `--route` and `--model` forms. Unknown or
ambiguous names report candidates and examples.
- Why: interactive route lookup should explain how to select a specific match.
- Test: `tests/route.rs`
- Owner: `crates/cli/src/route.rs`

## Results

### One bounded envelope, never truncated

With `--json` every command emits one envelope with area-specific fields inside
`data`. Results are capped at 1 MiB: the renderer checks size before
writing, and an oversized result fails with `RESULT_TOO_LARGE` (exit 3) with a
pointer to a published file or paged read. The renderer never truncates a result or reruns a
mutation. Paged lists bind a cursor to the input digest and keep findings at
exit 1 on every page. Human-readable output escapes terminal control sequences, including in
peer text.
- Why: a result must be whole or absent, and a lane must not forge terminal
  output.
- Test: `crates/cli/src/render.rs::oversize_is_rejected_before_any_stdout_and_lf_counts`;
  `tests/checks.rs::cursors_recheck_all_inputs_and_later_pages_keep_findings_exit`;
  `crates/cli/src/render.rs::terse_quotes_and_escapes_anything_that_could_mislead_a_terminal`
- Owner: `crates/cli/src/render.rs`

### Waits are readers

Every wait takes an explicit budget (1 to 14400 s on the CLI, 1 to 55 s
over MCP). A met condition, including a lane needing input, exits 0. Expiry
exits 124, and neither expiry nor cancelling a wait stops the work.
- Why: an impatient reader must never kill a healthy lane.
- Test: `crates/cli/src/lib.rs::bounded_waits_counts_and_byte_windows`;
  `crates/supervisor/tests/custody.rs::cancelled_reader_preserves_healthy_custody_and_cancel_escalates`
- Owner: `crates/cli/src/lib.rs`

### Lane waits count only new settlements

`lanes --wait` counts a lane only if it was unsettled when the wait started
or settled since, so a settled older attempt never satisfies a wait on a newer
one. `--since TIME` also counts lanes that settled at or after that time. An
attempt named by its exact ID counts whenever it settled. When every lane
named by `--id RUN[/TASK]` settled before the wait started, the wait is
refused (exit 2) with the count, instead of returning at once or idling to
expiry; the result reports `ignored_settled` otherwise. Without `--id`, a wait
covers the calling session's lanes and resumes after the settlements that
session last acknowledged. `--session LABEL` selects the lanes another agent
label submitted on this host, and narrows `--id` and listings the same way.
`--host PEER` waits on or lists the peer's own lanes (its newest 100) and
reads `--session` as an agent label on the peer. `--wait` still excludes
`--state` and `--cursor`.
- Why: a stale settlement must never wake a chair that waits for newer work,
  and selecting by session removes hand-copied ID lists.
- Test: `crates/cli/src/run/watch.rs::a_settled_older_attempt_never_satisfies_a_wait_on_a_newer_one`;
  `::a_named_attempt_counts_whenever_it_settled`;
  `::since_counts_settlements_before_arming`;
  `::nothing_pending_is_refused_for_named_lanes_only`;
  `crates/cli/src/lib.rs::lanes_session_and_watch_have_bounded_shapes`;
  `tests/parser.rs::lanes_watch_and_session_selection_report_an_empty_store`
- Owner: `crates/cli/src/run/watch.rs`, `crates/cli/src/run.rs`, `crates/cli/src/run/remote.rs`

### Lane watch is one stable line per event

`lanes --watch[=SECONDS]` (default and maximum 14400) selects lanes as a wait
does and prints one line per state a counted lane enters, then `end settled
lanes=N`, or `end timeout lanes=N pending=M` with exit 124 when the budget
passes first. A line is `STATE RUN/TASK/ATTEMPT`, with ` host=ID` for a peer's
lane, and carries no free text. Each line is flushed as written, so a
background shell or monitor sees it at once. A lane dispatched during the
watch joins it. With `--json` each line is a JSON object with `event: lane`
and the final line is the usual envelope. `--watch` excludes `--wait`,
`--state`, `--all` and `--cursor`.
- Why: a chair must be told when work settles without polling for it.
- Test: `crates/cli/src/run/watch.rs::watch_emits_each_state_once_and_ends_when_all_settle`;
  `::watch_times_out_with_pending_lanes_and_names_the_host`
- Owner: `crates/cli/src/run/watch.rs`, `src/lib.rs`

## Guards

### Merge guard

`pr merge NUMBER --head SHA` merges only the commit with that full object ID. `pr watch` and
`pr merge` need at least one passing check (the rest passing or
skipped), refuse a head change during verification and fail closed on
incomplete evidence.
- Why: a merge must land exactly the head that CI verified.
- Test: `tests/pr.rs::pr_merge_refuses_head_changes_before_and_after_checks`;
  `::pr_merge_refuses_unpassed_missing_and_malformed_checks`
- Owner: `src/pr.rs`

### Landing adds no merge authority

`pr land NUMBER --head SHA` runs the `pr watch` wait and the `pr merge` at
that head unchanged, then follows up locally: fetch the base, fast-forward the
local base only in a checkout with no changes, delete declared build output
and remove the branch's registered worktree through `worktree remove`, and
delete the merged branch with `git branch -d`. The two cleanup steps act only
on the pull request's own branch: a head in another repository (a fork) leaves
a same-named local branch and its worktree alone, and a local branch whose tip
the fetched base does not contain keeps its worktree as well as the branch. The
head is required. A failure
before the merge changes nothing. After it, each step refuses and reports
instead of forcing, and the command exits 1 with `merged: true`.
- Why: a landing is four routine steps, and none of them may widen what was
  reviewed or delete work that is not merged.
- Test: `src/pr_land_tests.rs::pr_land_merges_then_fetches_fast_forwards_and_prunes`;
  `::pr_land_refuses_each_unsafe_step_and_reports_the_rest`;
  `::pr_land_changes_nothing_when_the_merge_does_not_happen`;
  `::pr_land_leaves_a_local_namesake_of_a_fork_branch_alone`
- Owner: `src/pr/land.rs`, `src/worktrees.rs`

### Pull request bodies come from receipts

`pr open --from-attempt ATTEMPT --summary-file FILE` fills the repository's
`.github/pull_request_template.md` from the local attempt's record and, when
the attempt has one, its delivery receipt: the summary (whose heading or first
line is the title), `Closes #N` for each `--issue`, the head to approve, the
risk tier, the author and reviewer routes with their receipt paths, the head
each review read, and a gate table whose empty cells say `pending`, `none` or
`not recorded` rather than staying empty. Reviews come from `--review` or the
delivery receipt. Only a `succeeded` or `review_required` attempt with a
recorded head can back a pull request; a template missing a section it fills
is refused; the delivered branch must be at the attempt's head on the remote;
and an attempt that ran on a peer is refused, as its record lives there. The
attempt records no issue, so issues are named with `--issue`. `--dry-run`
prints the title and body and opens nothing. `pr status [N...]` prints one
line per pull request: `#N STATE HEAD checks=P/T [fail=F] [pending=W]
merge=STATE review=DECISION`, with `draft` appended for a draft, and this
account's open pull requests when none is named.
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

The guard refuses pattern-based and group process control (a literal
`kill [-SIGNAL] PID` above 1 is allowed), then applies the configured rule
IDs to every simple command. Unreadable or invalid config keeps the default
rules on. The guard is advisory: confinement and custody stay authoritative.
- Why: pattern kills hit sibling lanes, and config damage must not disable
  protection.
- Test: `tests/hooks.rs::unreadable_guard_config_keeps_the_default_rules_on`;
  `crates/confinement/src/shell_command.rs::process_control_never_obtains_authority_from_shell_wrapping`
- Owner: `crates/confinement/src/guard.rs`, `src/hooks.rs`

### Pre-push refuses data loss

`hook git pre-push` refuses default-branch deletion, non-fast-forward
updates, pushes from linked worktrees to the default branch and conflict
markers. It also refuses when any inspection fails, disables inherited redirects
and never fetches.
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

- Top-level `run`, `batch`, `reap` or `fabric dispatch`: the run verbs cover them.
- `worktree remove --force` or remote branch deletion.
- `commit-msg`, Codex or OpenCode hooks: no recorded protocol.
