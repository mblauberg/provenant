# Harness configuration
Owner: provenant-config
Anchor: [`config/harness-knobs.json`](../../config/harness-knobs.json); [`LaneBuildSettings`](../../tools/front-door/crates/config/src/gc.rs)

Settings live in the selected state root's `<state>/config.toml`; `provenant config
schema [--all]` lists every key with its table, default, bounds and effect,
`config show --full` the effective values and sources, `config check` every warning
with its file and key. This page covers only what those cannot: safety rules
and operator decisions. Edit with `config set|unset|apply` (whole-file validation and
`apply --expect-revision`); tables outside `[knobs]`, such as
`[domains.fabric.lanes]` and `[gc]`, are edited by hand.

## Precedence and refusals

Use `provenant config show --full` to inspect effective settings and their sources, and
`provenant config check` after editing. Resolve reported errors before retrying
the operation. See the [supervisor spec](../specs/supervisor.md) for boundary
behaviour and [GC](gc.md) for retention configuration.

## Port table

The port table applies layers in precedence order, lowest to highest:
`builtin` < `detected` < `host` < `host-project` < `project`. Compiled builtins
come first, then auto-detected defaults, then host `state/ports.json`, then
`state/projects/<project_key>/ports.json`, and finally project
`.provenant/ports.json`. The keyed host-project layer lets a consumer repository
be configured entirely from host state with no files in its tree; a missing
project key or keyed layer is skipped silently. A layer only needs the fields it
changes: object fields deep-merge over the lower layer, so
`{"verify": {"base": "origin/master"}}` keeps the other verify fields from the
layers below it. `schema_version` is optional in a layer; it defaults to the
current version, and a mismatch warns rather than refuses a compatible shape.
Provenance is tracked per dotted leaf (for example `build.prepare`,
`verify.base`, `verify.tiers.0.commands`): each leaf reports the highest layer
that set it, and a port's label is derived from those leaves. `provenant config
show --full` lists every effective leaf, including build and tier values, with
the layer or inferred ref that supplied it.

When no layer sets `verify.base`, Provenant infers it from
`refs/remotes/<remote>/HEAD` for the effective `verify.remote` and marks the
value as inferred in `config show`. If no remote HEAD resolves, the refusal
names `verify.base` in `.provenant/ports.json` or host `ports.json` and shows an
example value. Set `verify.remote` as well when the project does not use the
default remote. A malformed layer names its file and key; a bad project layer
is warned and skipped so commands that do not need verify still run, while an
unreadable host port table still refuses lane identity.

This repository keeps its source at `config/ports.json` and links the optional
`.provenant/ports.json` path to it. Repository-specific scripts such as
`scripts/verify-nextest` belong in that source (direct CI reads it without
Provenant); the builtin verify tiers are neutral cargo commands.

Each tier command is an object with `stack` (default empty), `cwd` (default
`.` and relative to the effective verify workspace, without `..`) and a non-empty `argv`
array. The whole elements `{packages}`, `{integration_args}` and
`{quarantine_args}` expand to zero or more arguments; other elements are
literal and are never shell split. Arrays replace across layers. Detected Rust tiers 0 and 1 select packages and tier 2 selects all; tiers containing non-Rust commands select all and expand selection placeholders to nothing. An invalid tier command warns with its layer and tier, and that tier falls back to the next lower layer.

The `build` port carries the shared seed-build interface. Its compiled defaults
are `strategy = "lane_seed"`, `prepare = []`, `seed.stale_after = "24h"`,
`wrapper.argv = []`, `command.argv = []`, `offload = "auto"`,
`overflow = "local"`, `clean = "none"`, `stacks = []` and
`snapshot_retention = "7d"`. `strategy` documents `slots`, `lane_seed`, `lane`,
`wrapper` and `command`; only `lane_seed` is consumed today. The strategy,
wrapper, command, offload, overflow, clean, stacks and retention keys only
declare their shape here; later slices consume them. `build.prepare` is an
ordered list of argv arrays: each command runs in order with the checkout root
as its working directory before every seed generation build (and, later, slot
builds), in the same sanitised child environment as the Cargo steps. A failing
command fails that build with a `prepare_failed` outcome that records the
command and the last 2 KiB of the combined output; an unreadable or invalid
table warns and runs no prepare command. Arrays replace rather than concatenate
across layers. `build.seed.stale_after` is the age after which a seed is
considered stale, expressed in the host-gc duration syntax (0 or whole seconds,
minutes, hours, days or weeks, e.g. `24h`, the default); a reader warns and
falls back to `24h` on a bad value, reported with source `default`; a valid
value reports the layer that declared it (`builtin`, `detected`, `host`,
`host_project` or `project`). Every key layers like other port fields, so a
layer can set only the one it changes, and each leaf reports its source. A
writer lane with a private Cargo target warns once when a
project has no complete seed generation, when the newest one is older than
`build.seed.stale_after` (aged from its `complete` marker), or when the last
refresh outcome is `failed` or `prepare_failed`. `provenant worktree doctor`
shows the same generation age, last outcome and reason, and effective knob with
its source. The warning names the outcome, the knob and its value/source, and
says to run `provenant worktree seed-build` in the repository; it never refuses
the lane. Refresh outcomes are stored atomically at
`<state>/cargo-target-seeds/refresh/<project_key>.json` with a schema, instant,
outcome and a reason capped at 2 KiB. A missing, corrupt or newer-schema file
is read as no record. For example, to ensure generated files exist before
seeding:

```json
{
  "build": {
    "prepare": [["node", "tools/ensure-generated.mjs"]]
  }
}
```

Build execution is selected through the layered `build.*` port. Set
`build.strategy` to `slots`, `lane_seed`, `lane` or `command`; the existing
`[admission.build].slots` value remains the shared concurrency limit. For a
repository wrapper, set `build.strategy = "command"` and configure
`build.command` with its argv and exit-code dispositions. The removed
`build_admission` key is treated as an unknown port and produces the normal
unknown-key warning.

## Provider model scope

`[providers.cursor]` defaults to `models = ["auto", "composer-*", "grok-*"]`.
That compiled list limits shipped automatic pools and supplies the default scope
for explicit primary-model pins. Owner-configured pools and routes can select
other catalogued models. Setting `models` explicitly makes that restriction
apply to automatic choices across shipped and owner-configured pools, routes,
preferences, fallbacks and depletion hand-offs. Any provider can set `models` to
a list of model-name patterns: `*` matches zero or more characters, and other
characters are literal. An empty list excludes every model from automatic
routing. Providers without a compiled or configured list are unrestricted. Set
`out_of_scope_pin` to `"warn"` (default), `"refuse"`, or `"allow"` to control
explicit primary pins outside the effective scope. Fallback models are automatic
alternatives and do not use this pin policy. Warnings name the model and setting.
Host settings replace the provider's default list; `provenant config show --full`
displays the values and sources, and `provenant check config` reports invalid
settings.

Auto is available for explicit selection, but is not in a shipped pool: its
underlying model is selected by Cursor and is not established by this scope.

## Lane skills and project paths

Set `domains.lanes.user_skills` at host, project or route scope. Configure
project `shared_paths` and `exec_roots` for writer access, or
`[domains.run] writer_writes` for host-wide write grants. Use
`provenant fabric identity` to find the project ID and `provenant config schema`
to inspect the settings.

## Lane Cargo target

`lanes.cargo_target` picks each writer lane's `CARGO_TARGET_DIR`. The default,
`private`, is the worktree's own target in its lane cache, cloned warm from the
project's host seed (unseeded and cold, with a warning, where the filesystem
cannot clone). `project` uses the worktree's own `target/` and starts cold. An
absolute path is used as given: never point several worktrees of one repository
at it, because Cargo then serves one branch's artifacts to another
([run spec](../specs/run.md#cargo-target-directory)). The lane preamble names
the value and its source. A `shared` value from an older release is reported by
`provenant config check` and the default applies; `provenant gc --apply`
removes its old directories.

The lane's `CARGO_INCREMENTAL` and `CARGO_PROFILE_DEV_DEBUG` defaults come from
one settings source: `cargo_incremental` and `cargo_debug` under
`[domains.fabric.lanes]`. `LaneBuildSettings` resolves them once per launch and
the host's seed build resolves the same values the same way, so both compile
with identical debuginfo and incremental settings. A value already present in
the lane environment takes precedence in both. Changing a default affects only
future builds: an existing target keeps the artifacts it was built with, so the
change reclaims no space there until that target is rebuilt or removed.

To build warm in a checkout outside a lane, seed its private target and use it:

```sh
export CARGO_TARGET_DIR="$(provenant worktree seed-target .worktrees/my-branch)"
```

The command needs no lane environment: unset `RUSTUP_HOME` and `CARGO_HOME` mean
the defaults under `$HOME`. It prints where the Cargo home it verified came from,
since your shell's `CARGO_HOME` can differ from the host a lane runs under. When it cannot seed, it names what differed (for
example the pinned and the seeded compiler) and what to run next.

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
the effective settings with `provenant config show --full`; the boundaries are in
[ADR 0040](../adr/0040-open-lane-policy-with-explicit-boundaries.md) and the
[supervisor spec](../specs/supervisor.md).

## Lane sandbox mode

Use `sandboxed_routes` to select routes for sandboxing and `--no-sandbox` to
request an unsandboxed run. Configure `sandbox_cache_reads` for host caches a
sandboxed lane needs. Explicit network restrictions and host path denials select
an enforcing profile even when the sandbox setting is off. If the backend is
unavailable, a selected enforcing profile fails with a capability error. An
inherited sandbox alone does not establish equivalent protection. Select a
trusted unsandboxed route explicitly when that is the intended authority.
A route that defaults to trusted, or an explicit `--no-sandbox`, retains an
existing OS sandbox without requiring a new profile only when no explicit
restriction or parent floor requires one.
Check the attempt receipt for the selected mode and source.
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

## Named admission classes

Heavy commands coordinate across lanes and processes through named admission
classes configured under `[admission.<class>]`:

```toml
[admission.build]
slots = 2
wait_seconds = 15
memory_per_slot = "4 GiB"
disk_floor = "10 GiB"
trim_action = "cargo clean --package test-artifacts"

[admission.heavy]
slots = 1
wait_seconds = 30
```

- `slots`: Maximum concurrent processes admitted for the class. For `build`,
  defaults to host hardware capacity derived from CPU cores and RAM. Set it to
  `0` to disable the cap.
- `memory_per_slot`: Optional memory per slot. Admission samples RSS through the
  process-tree sampler. Setting it turns sampling on every 5000 ms unless
  `domains.run.resource_sample_interval_ms` sets another interval; if no sample
  is available the limit is skipped with a warning.
- `disk_floor`: Optional minimum free disk floor required before running.
- `trim_action`: Optional shell command executed to free resources if limits are
  exceeded before re-sampling.
- `wait_seconds`: Maximum wait time before emitting a warning and proceeding
  (default 15 seconds). A saturated class never deadlocks or refuses commands.

Settings layer with provenance: Host `<state>/config.toml`, then project
`.provenant/config.toml`, falling back to compiled defaults.

Commands acquire slots via the CLI:
```sh
provenant admit <CLASS> -- <COMMAND> [ARGS...]
```

Every lane's `cargo`, `cargo-nextest`, and `nextest` commands are automatically
routed through the `build` admission class during both compilation and test
execution phases. Nested invocations within an admitted command detect the
inherited slot lease and pass through without re-acquiring or deadlocking.
Holders, waiters, elapsed wait durations, and admission classes are visible in
`provenant status`.

### Stack discovery

`provenant config detect` reports read-only stack discovery, proposed commands and
warnings. Rust discovery selects the nearest Cargo manifest with a workspace or
package table, at most two directories below the root; equal-depth candidates
warn and skip Rust. Node discovery reads only the root `package.json`: its
`packageManager` takes precedence over pnpm, yarn, npm and bun lockfiles, and
npm is the warned fallback. Only existing lint, typecheck, test and build scripts
are proposed: lint and typecheck in tier 0, test in tier 1, and build in tier 2.
Rust tiers use Cargo fmt, check and lib/bin tests in tier 0, Cargo integration tests in tier 1, and workspace tests in tier 2; detection adds no stress tier. Mixed tiers list Rust commands first, with each command's own
project-relative working directory.

`provenant config show --full` includes detected commands and effective build and
verify tier leaves with their sources. `provenant config path --project` prints
the host-project ports path and whether it exists. Detection never overrides
configured values. Its default tier fills a gap below host, host-project and
project layers. When a stack is detected, its tier table replaces the builtin tier table; when none is detected, the builtin tiers remain. Host,
host-project and project declarations override detected leaves.
