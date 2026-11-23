# Using Provenant 2 from another repository
Owner: repository maintainers
Anchor: [consumer contract](../specs/consumer-contract.md)

For a repository that runs Provenant lanes, or whose scripts and CI run beside
Provenant's tooling. The [consumer contract](../specs/consumer-contract.md) lists
what you may rely on and how stable each item is (stability tiers, command
grammar, run files, environment, state root, exit codes); this page is the
procedure. Check an item's tier there before you build on it, and read the release
notes on each minor upgrade. Moving a host from 1.x is in the
[install runbook](install.md#migrating-from-1x).

## Adopt with no repository changes

A repository needs no Provenant files, scripts, build files or instruction files to
run a lane. A lane adds nothing to your checkout except the Git-excluded
`.provenant/scratch` and the lane worktrees under the worktree-layout root
(`.worktrees/` by default). If your tooling scans the checkout, ignore both.

Some providers apply the parent checkout's ignore files to a nested worktree:
Cursor reads `.cursorignore`, so a `.worktrees/` line there hides each Cursor lane's
own worktree from it. `provenant hosts doctor` (run in the checkout) and launch
preparation warn when an adapter-declared ignore file hides the worktree root; they
never edit it. To keep worktrees out of IDE indexing without hiding them from
lanes, list them in the indexing-only file (Cursor: `.cursorindexingignore`).
Alternatively keep the line and move lane worktrees to a root it does not list with
the `worktree_layout` port (`default` and `roots`). Check what Provenant will use:

```sh
provenant config ports
provenant config show --full
```

## Opt into ports

Ports are where Provenant hands work to your repository: verify, landing, CI
observation, worktree layout, branch naming, post-create setup, tracker, release
checks and admission. Each has a backend: `builtin`, `command` (your argv) or
`none`. Declare only the ports you want to change in `.provenant/ports.json`; it
layers over the host and builtin tables ([layers](configuration.md#port-table)).

1. Run `provenant config ports` and note each port's backend and source.
2. Create `.provenant/ports.json` with the current `schema_version` and only the
   ports you are changing, copying each shape from that output.
3. Declare `verify` first. The builtin verify backend targets Provenant's own
   layout, so a repository that verifies through Provenant should point `verify`
   at its own check command.
4. Rerun `provenant config ports` and confirm each changed port's source is
   `project`.

Port commands receive the contract's `port_command` variables (such as
`PROVENANT_VERIFY_TIER` and `PROVENANT_ADMISSION_ATTEMPT`); treat them as evolving.
Repository launch policy goes in `.provenant/config.toml` under `[launch_policy]`
(`confidential`, `protected_paths`), and landing defaults under
`[domains.pr.land]` (`provenant config schema` lists its keys).

## Run beside your own tooling

Your scripts can tell when they run inside a lane and adapt:

| Need | Read |
|---|---|
| Am I in a lane? | `PROVENANT_LANE_DEPTH` is set and above `0` |
| Am I sandboxed? | `PROVENANT_SANDBOX` is `on` |
| A lint run that does not queue behind builds | point `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` at `PROVENANT_LANE_CARGO_LINT_TARGET` |
| Ports for a dev server | the block from `PROVENANT_LANE_PORT_BASE`, `PROVENANT_LANE_PORT_COUNT` long |
| Memory of my own process tree | `provenant process-tree --json` when `PROVENANT_PROCESS_TREE` is set; omit `--root` for the supervised command's full subtree, or pass a sampled descendant PID for a smaller one |

When your tooling and Provenant share a checkout:

- Do not read or write Provenant's state root or run directories; use
  `provenant status --json`, `output` and `trace`.
- Parse `--json`, not human output; printed next-step lines are evolving.
- A lane's `HOME` is chosen during provider launch preparation: host access uses
  the native provider home by default, a selected account may use its own home, and
  isolated sandboxed lanes use private homes. Provider caches and credentials may
  live elsewhere, so check the provider's section in [providers](providers.md)
  before relying on a path.
- Use `PROVENANT_GIT` for Git in a lane when it is set.
- Treat an unset variable as "not in a lane", never an error: lanes started outside
  a project, or read-only lanes, set fewer variables (the contract's `When` column
  says which).
- Set `PROVENANT_*` variables yourself only where the contract's channel is
  `lane_or_caller` or `caller`.

## Verify

- `provenant config ports` shows the backend and source you expect for every port.
- In a lane, `env | grep '^PROVENANT_'` lists only names the contract declares,
  plus internal ones you do not depend on.
- Your CI passes without Provenant installed; nothing in it should require
  Provenant files.
