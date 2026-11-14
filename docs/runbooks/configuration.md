# Harness configuration
Owner: provenant-config
Anchor: [`config/harness-knobs.json`](../../config/harness-knobs.json); [`LaneBuildSettings`](../../tools/front-door/crates/config/src/gc.rs)

Settings live in the selected state root's `<state>/config.toml`; `provenant config
schema [--all]` lists every key with its table, default, bounds and effect,
`config show` the effective values and sources, `config check` every warning
with its file and key. This page covers only what those cannot: safety rules
and operator decisions. Edit with `config set|unset|apply` (one lock, whole-file
validation, `apply --expect-revision`); tables outside `[knobs]`, such as
`[domains.fabric.lanes]` and `[gc]`, are edited by hand.

## Precedence and refusals

Use `provenant config show` to inspect effective settings and their sources, and
`provenant config check` after editing. Resolve reported errors before retrying
the operation. See the [supervisor spec](../specs/supervisor.md) for boundary
behaviour and [GC](gc.md) for retention configuration.

## Lane skills and project paths

Set `domains.lanes.user_skills` at host, project or route scope. Configure
project `shared_paths` and `exec_roots` for writer access, or
`[domains.run] writer_writes` for host-wide write grants. Use
`provenant fabric identity` to find the project ID and `provenant config schema`
to inspect the settings.

## Git pre-push hook and local gate

Connect the Git pre-push hook to Provenant so it checks pushes, including its
existing default-branch protection. Pass both Git arguments and leave standard
input connected: Git supplies the remote name and URL as arguments and the ref
updates on standard input. For a plain `.git/hooks/pre-push` (make it executable):

```sh
#!/bin/sh
exec provenant hook git pre-push "$@"
```

For Husky, put `provenant hook git pre-push "$@"` in `.husky/pre-push`.
For Lefthook, configure `use_stdin` so Git's ref updates reach Provenant:

```yaml
pre-push:
  commands:
    provenant:
      run: 'provenant hook git pre-push "{1}" "{2}"'
      use_stdin: true
```

Lefthook sends standard input to only one command configured with `use_stdin`.
Keep any other checks in the manager's hook.

Configure an optional local gate under `[domains.pr.land.local_gate]` and
inspect its settings with `provenant config schema`. Project-specific values can
be set under `[domains.pr.land.projects."ID".local_gate]`.

## Worktree setup

Configure trusted setup commands under `[worktrees]` or
`[worktrees.projects."PROJECT_ID"]`. Find the project ID with
`provenant fabric identity`; see [worktrees](../worktrees.md) for the procedure.
If setup fails, inspect its output and retry manually in the new worktree.

## Run host policy

Set `[domains.run.host_policy]` to restrict host paths or network access. Inspect
the effective settings with `provenant config show`; the boundaries are in
[ADR 0040](../adr/0040-open-lane-policy-with-explicit-boundaries.md) and the
[supervisor spec](../specs/supervisor.md).

## Lane sandbox mode

Use `sandboxed_routes` to select routes for sandboxing and `--no-sandbox` to
request an unsandboxed run. Configure `sandbox_cache_reads` for host caches a
sandboxed lane needs. Check the attempt receipt for the effective mode and source.
See [ADR 0040](../adr/0040-open-lane-policy-with-explicit-boundaries.md) and the
[supervisor spec](../specs/supervisor.md) for boundaries.

Start shared daemons on the host, or close inherited descriptors when a lane
starts one, so the attempt can settle.

## Unreadable process census exceptions

Set `confinement.census_ignore_unreadable` only for signed executables whose
risk you accept. See the [supervisor spec](../specs/supervisor.md#unreadable-processes-need-positive-non-occupancy-evidence).

## OpenCode Go quota calibration

Enter dashboard usage floors under `[domains.quota.calibration.opencode-go]`
and set expiry to the earliest included window end. Inspect available settings
with `provenant config schema` and check quota after changing them.

## Chair account switching

Register the Claude accounts using [Install](install.md#install), then configure switching:

```sh
provenant config set chair.switch_accounts '["work-spare","personal-spare"]'
provenant config set chair.switch_threshold 90
provenant config set chair.auto_switch true
provenant chair switch --dry-run
provenant chair switch --to work-spare --session-id ID
```

An empty list permits no targets. A manual switch bypasses `auto_switch` but
needs an allowed account with headroom and a session ID (`--session-id`, then
`CLAUDE_CODE_SESSION_ID`, then `CLAUDE_SESSION_ID`). The prompt hook reads cached
quota only: stale, missing or unknown quota cannot establish
headroom, so refresh with `provenant quota tick --adapter claude`. The hook
saves a private checkpoint, then prints the launch command, and the source session stops; the new session injects the hand-off once. Read the checkpoint's lane IDs and collect finished results
rather than dispatching them again.
