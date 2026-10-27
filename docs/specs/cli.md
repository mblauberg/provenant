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
the project, never passes `--discard`, and treats any owner refusal (dirty,
live writer, primary) as a failure that leaves the worktree. Both exit 2 with
a bounded stderr cause on refusal.
- Why: a worktree Claude creates must have an owner from the start, and a
  hook must not widen what removal may delete.
- Test: `tests/hooks_session.rs::worktree_hooks_create_and_remove_through_the_worktree_owner`;
  `::worktree_hooks_refuse_bad_names_the_primary_and_foreign_paths`
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
