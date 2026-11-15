# Using Provenant 2 from another repository
Owner: repository maintainers
Anchor: [consumer contract](../specs/consumer-contract.md)

For a repository that runs Provenant lanes, or whose scripts and CI run beside
Provenant's tooling. The [consumer contract](../specs/consumer-contract.md) is
the list of what you may rely on and how stable each item is; this page is the
procedure.

## What 2.0 changed for consumers

- **Commands.** A removed spelling exits 2 and prints its replacement, so a failing script
  tells you what to change. There are no aliases. The complete migration list follows.

### Removed commands

<!-- REMOVED_CLI_START -->
| Removed spelling | Replacement |
|---|---|
| `provenant dispatch --prompt-file` | provenant run TEMPLATE --brief-file PATH |
| `provenant dispatch --prompt-stdin` | provenant run TEMPLATE --brief-file - |
| `provenant resume --no-fallback` | provenant retry NAME/NODE#N |
| `provenant run --prompt-file` | provenant run TEMPLATE --brief-file PATH |
| `provenant run --prompt-stdin` | provenant run TEMPLATE --brief-file - |
| `provenant retry --no-fallback` | provenant retry NAME/NODE#N |
| `provenant retry --prompt-file` | provenant retry NAME/NODE --brief TEXT |
| `provenant retry --prompt-stdin` | provenant retry NAME/NODE --brief TEXT |
| `provenant retry --operation-id` | no direct equivalent; use `provenant retry NAME/NODE#N` |
| `provenant output --host` | no direct equivalent; use `provenant output NAME` |
| `provenant dispatch` | provenant run TEMPLATE BRIEF |
| `provenant lanes` | provenant status |
| `provenant resume` | provenant retry NAME/NODE |
| `provenant cancel` | provenant stop NAME |
| `provenant continue` | provenant retry NAME/NODE#N |
| `provenant redo` | provenant retry NAME/NODE |
| `provenant preset` | provenant templates |
| `provenant review` | provenant run review BRIEF |
| `provenant stats` | provenant telemetry cost --by node |
| `provenant pr land` | provenant land NUMBER --wait |
| `provenant fabric claim` | provenant adopt KIND:ID or provenant release KIND:ID |
| `provenant land --batch` | provenant land NUMBER [NUMBER ...] |
| `provenant fabric graph` | provenant status NAME |
| `provenant fabric mcp` | provenant fabric |
| `provenant fabric_graph` | provenant run TEMPLATE BRIEF |
| `provenant quota usage` | provenant telemetry cost --by account |
| `provenant config migrate` | provenant config check |
| `provenant telemetry backfill` | nothing; this binary reads no format-1 store |
| `provenant run --use` | provenant run TEMPLATE BRIEF |
| `provenant run --tier` | provenant run TEMPLATE BRIEF --route ROUTE |
| `provenant run --operation-id` | provenant run TEMPLATE BRIEF --name NAME |
| `provenant run --wait` | provenant wait NAME |
| `provenant status --wait` | provenant wait NAME |
| `provenant status --target` | provenant status NAME or provenant wait --pr NUMBER |
| `provenant status --until` | provenant wait NAME --any |
| `provenant status --follow` | provenant wait NAME --follow |
| `provenant status --stats` | provenant telemetry cost NAME --by node |
| `provenant status --by` | provenant telemetry cost NAME --by node |
| `provenant status --id` | provenant status NAME |
| `provenant stop --operation-id` | provenant stop NAME |
| `provenant stop --wait` | provenant wait NAME |
| `provenant retry --node` | provenant retry NAME/NODE |
| `provenant retry --wait` | provenant wait NAME |
| `provenant output --trace` | provenant trace NAME |
| `provenant reconcile --id` | provenant reconcile NAME |
| `provenant telemetry stats --run-id` | provenant telemetry cost NAME |
| `provenant telemetry stats --by` | provenant telemetry cost --by route |
| `provenant telemetry stats --detail` | provenant telemetry cost NAME |
| `provenant telemetry trace --run` | provenant telemetry trace NAME |
| `provenant pr status --wait` | provenant pr watch NUMBER |
| `provenant steer --text-stdin` | provenant steer TARGET --brief-file - |
<!-- REMOVED_CLI_END -->
- **Environment.** `PROVENANT_SESSION_STATE` was removed in 1.x: session
  checkpoints are keyed by the host session ID. `PROVENANT_BUILD_WAIT_SECS`
  and `PROVENANT_BUILD_WAIT_MARKER` are removed because build slots no longer
  wait. `PROVENANT_SANDBOX`, `PROVENANT_PROCESS_TREE` and
  `PROVENANT_LANE_CARGO_LINT_TARGET` keep their names and are stable.
  `PROVENANT_LANE_DEPTH` keeps its name and is evolving.
- **Tiers.** Most of the contract is `evolving`: it may change in a minor
  release, and the release notes say how. Only what a consuming repository
  relies on today is `stable`, which changes only in a major release. Check an
  item's tier in the [consumer contract](../specs/consumer-contract.md) before
  you build on it, and read the release notes on each minor upgrade.
  `PROVENANT_TEST_*` names are Provenant's own and never part of the contract.
- **Run files.** Reading files under a run or attempt directory is
  unsupported. Use `provenant status --json`, `provenant output NAME` and
  `provenant trace NAME`.
- **State root.** 2.x keeps its state in `~/.local/share/provenant/v2` (or
  `PROVENANT_STATE_DIR`). It never writes `~/.local/share/provenant/v1`, and
  run records do not carry over. Drain and land 1.x work before upgrading
  ([install runbook](install.md#upgrade)).
- **Output.** Parse `--json`, not human output. Printed next-step command
  lines are evolving and may change in a minor release.

### Rolling back

Reinstall 1.x: from a checkout of the 1.x release tag, run its
`./install.sh` ([install runbook](install.md#bootstrap-and-upgrade)). Because 2.x
never wrote the 1.x root, 1.x finds its state as it was left. Work started
under 2.x stays in the v2 root; collect it with `provenant output` before
you roll back.

## Adopt with no repository changes

A repository needs no Provenant files, scripts, build files or instruction
files to run a lane. A lane adds nothing to your checkout except the
Git-excluded `.provenant/scratch` and the lane worktrees under the
worktree-layout root (`.worktrees/` by default). If your tooling scans the
checkout, ignore both, but mind providers that apply the parent checkout's
ignore files to a nested worktree: Cursor reads `.cursorignore`, so a
`.worktrees/` line there hides each Cursor lane's own worktree from it. The
adapter declares those files; `provenant hosts doctor` (run in the checkout)
and launch preparation warn when one hides the worktree root, and never edit
the file. To keep worktrees out of IDE indexing, list them in the indexing-only
file instead (Cursor: `.cursorindexingignore`), which does not hide them from
lanes; the warning names it. Or keep the line and move lane worktrees to a root
it does not list with the `worktree_layout` port (`default` and `roots` in
`.provenant/ports.json`). Check what Provenant will use with:

```sh
provenant config ports
provenant config show
```

## Opt into ports

Ports are where Provenant hands work to your repository: verify, landing, CI
observation, worktree layout, branch naming, post-create setup, tracker,
release checks and admission. Each port has a backend: `builtin`, `command`
(your argv) or `none`. Declare only the ports you want to change in
`.provenant/ports.json`; it layers over the host table and the builtin one.

1. Run `provenant config ports` and note each port's backend and source.
2. Create `.provenant/ports.json` with the current `schema_version` and only
   the ports you are changing. Copy each port's shape from that output.
3. Declare `verify` first. The builtin verify backend targets Provenant's own
   layout, so a repository that verifies through Provenant should point
   `verify` at its own check command.
4. Rerun `provenant config ports` and confirm the source of each changed port
   is `project`.

Port commands receive the `port_command` variables in the contract, such as
`PROVENANT_VERIFY_TIER` and `PROVENANT_ADMISSION_ATTEMPT`. Treat them as
evolving.

Set repository launch policy in `.provenant/config.toml` under
`[launch_policy]` (`confidential`, `protected_paths`). Landing defaults go
under `[domains.pr.land]`; `provenant config schema` lists those keys.

## Run beside your own tooling

Your scripts can tell when they run inside a lane and adapt:

| Need | Read |
|---|---|
| Am I in a lane? | `PROVENANT_LANE_DEPTH` is set and above `0` |
| Am I sandboxed? | `PROVENANT_SANDBOX` is `on` |
| A lint run that does not queue behind builds | point `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` at `PROVENANT_LANE_CARGO_LINT_TARGET` |
| Ports for a dev server | the block from `PROVENANT_LANE_PORT_BASE`, `PROVENANT_LANE_PORT_COUNT` long |
| Memory of my own process tree | `provenant process-tree --json` when `PROVENANT_PROCESS_TREE` is set; omit `--root` for the supervised command's full subtree, or pass a sampled descendant PID to select a smaller subtree |

Keep these rules when your tooling and Provenant share a checkout:

- Do not read or write Provenant's state root or run directories; use the
  commands above.
- A lane's `HOME` is selected during provider launch preparation. Unsandboxed
  lanes use the caller's home by default; a provider or selected account may
  use an account-specific or private home in either mode. Provider caches and
  credentials may live elsewhere, so check the provider's runbook before
  relying on a particular path.
- Use `PROVENANT_GIT` for Git in a lane when it is set.
- Treat an unset variable as "not in a lane", never as an error. Lanes started
  outside a project, or read-only lanes, set fewer variables; the contract's
  `When` column says which.
- Do not set `PROVENANT_*` variables yourself except `PROVENANT_STATE_DIR`,
  `PROVENANT_AGENT_LABEL` and `PROVENANT_SESSION_ID`, which are read from the
  caller.

## Verify

- `provenant config ports` shows the backend and source you expect for every
  port.
- In a lane, `env | grep '^PROVENANT_'` lists only names the contract
  declares, plus internal ones you do not depend on.
- Your CI passes without Provenant installed; nothing in it should require
  Provenant files.
