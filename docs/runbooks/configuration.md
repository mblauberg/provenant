# Harness configuration
Owner: provenant-config
Anchor: [`config/harness-knobs.json`](../../config/harness-knobs.json); [`LaneBuildSettings`](../../tools/front-door/crates/config/src/gc.rs); [`lane_history.rs`](../../tools/front-door/crates/config/src/lane_history.rs)

Settings live in the selected state root's `<state>/config.toml`. The binary is
the reference: `provenant config schema [--all]` lists every key with its table,
default, bounds and effect, `config show --full` the effective values and their
sources, and `config check` every warning with its file and key. Edit schema keys
with `config set|unset|apply` (`apply --expect-revision` commits a whole patch or
none). `[admission.<class>]` and the port tables are outside the schema; edit
them by hand. This page covers only what those commands cannot: operator
decisions and the rules behind them. Boundaries are in the
[supervisor spec](../specs/supervisor.md); retention is in [GC](gc.md).

## Port table

Ports are where Provenant hands work to a repository (verify, build, worktree
layout and others); `provenant config ports` shows each one with its source.
Layers apply lowest to highest: `builtin` < `detected` < `host` (`<state>/ports.json`)
< `host-project` (`<state>/projects/<project_id>/ports.json`) < `project`
(`.provenant/ports.json`). Builtins are in
[`ports.json`](../../tools/front-door/crates/config/assets/ports.json). The
host-project directory is the ProjectId binding directory that
`provenant config path` prints, so a repository can be configured from
host state with no files in its tree. A missing layer is skipped silently. The
binding record can be forgotten when its common directory is gone; the
host-project directory and its `ports.json` stay for a later bind of the same
location.

- A layer sets only the fields it changes. Objects deep-merge, so
  `{"verify": {"base": "origin/master"}}` keeps the other verify fields. Arrays
  replace. `schema_version` is optional in a layer; the current port-table
  version is 3. A repository `.provenant/ports.json` still declaring 2 is read
  with the current schema after a `CONFIG_MALFORMED` warning whose repair is to
  set `"schema_version": 3` in that file (or rerun init to regenerate it).
- Provenance is per dotted leaf (for example `build.prepare`, `verify.base`):
  each leaf reports the highest layer that set it, in `config show --full`
  and `config ports --json`.
- `config check` warns, naming file and key, when a port leaf sits in
  `.provenant/config.toml` instead of a ports layer, or when a host-project
  `ports.json` sits under a 64-hex target-seed directory instead of the binding
  directory. These warnings never refuse a command.
- A malformed layer names its file and key. A bad project layer is warned and
  skipped so commands that do not need it still run; an unreadable host layer
  still refuses lane identity.
- `git.remote`, `git.default_branch` and `git.protected` declare the one Git
  context every forge call shares. Declare them in any ports layer, for example
  `{"git": {"remote": "upstream", "default_branch": "main", "protected": ["release"]}}`.
  An omitted `git.remote` is detected from the current branch's upstream, then
  the sole remote; a refusal lists the remotes seen with close matches instead
  of assuming `origin`. An omitted `git.default_branch` is resolved from
  `refs/remotes/<remote>/HEAD`, then the forge API. An omitted `git.protected`
  is the default branch plus the forge-reported protected branches. These ports
  report their layer and key in `config ports --json`; detected values report
  `detected` with the branch key or ref that produced them.
- `verify.base`, when no layer sets it, is inferred from
  `refs/remotes/<remote>/HEAD` for `verify.remote` and shown as inferred. If no
  remote HEAD resolves, the refusal shows an example to put in a ports layer.
- A tier command is an object with `stack`, `cwd` (relative to the verify
  workspace, no `..`) and a non-empty `argv`. The whole elements `{packages}`,
  `{integration_args}` and `{quarantine_args}` expand to zero or more arguments;
  other elements are literal and never shell split. An invalid tier command
  warns with its layer and tier, and that tier falls back to the next lower layer.
- `verify.format` declares the command approval carry runs to prove a delta is
  formatting-only, in the same string or table form as a tier command, for
  example `{"verify": {"format": "cargo fmt"}}`. It runs exactly as declared
  from the verify workspace joined with its `cwd`. When no layer declares it,
  stack detection supplies a write-mode default and the carry receipt names
  whether the command came from `detected` or from a configured layer. The
  literal string `"none"` turns format carry off and says so in the warning.

This repository keeps its ports at `config/ports.json` and links the optional
`.provenant/ports.json` to it, so direct CI reads it without Provenant. Put
repository scripts such as `scripts/verify-nextest` there; the builtin tiers are
neutral cargo commands.

### Stack discovery

`provenant config detect` shows what discovery found, with proposed commands and
warnings, and writes nothing. Detection fills only a gap below every configured
layer. When a stack is detected its tier table replaces the builtin one.
Discovery rules (Rust, Node, mixed tiers) are in
[`detect.rs`](../../tools/front-door/crates/config/src/detect.rs).

### Build port

`build` configures the chair's host-owned warm check. Lane checks are declared
separately in `verify.lane`. `strategy` accepts only `slots`, with
`[admission.build].slots` as the host-wide concurrency cap; a leftover
`lane_seed`, `lane`, `wrapper` or `command` value, or a `build.wrapper`,
`build.command` or `build.seed` key, now warns as unknown.

- `prepare` is an ordered list of argv arrays run from the checkout root before
  a slot job's first step. A failing command fails that job with
  `prepare_failed`, recording the command and the last 2 KiB of its output. An
  unreadable table warns and runs nothing. Lane worktree creation and refresh
  run the same commands once per declaration digest; failures warn and are
  retried on the next provisioning. Arrays replace across layers.
- `offload = "off"` runs every job locally with exception `offload_off`.
  `overflow`: `auto` queues under slots and waits a bounded time otherwise;
  `queue` always waits; `local` runs locally after the admission wait.
- `clean` before a slot job: `none` keeps files, `untracked` runs
  `git clean -ffd`, `ignored` runs `git clean -ffdx`.
- `priority` maps a job class to a queue level and orders waiters; it never
  preempts a running job. `queue_aging` raises a waiter's effective level to
  bound lower-priority waits.
- `snapshot_retention`, `canary_every`, `slot_idle` and `soak.*` govern host slot
  hygiene and the build report. Leaves and defaults are in `ports.json`, and an
  unknown value warns with the leaf and its source and falls back to the
  builtin.

### Lane checks

Declare `verify.lane` as an array of `{ "tier": "T0", "argv": ["command", "arg"],
"scope": "touched files" }` entries. The writer preamble renders every valid
entry with its source. An empty or absent list defaults to the repository's
formatter and declared pre-commit hooks; invalid entries warn and are omitted.

### Affected-package selection

`verify.affected` (`enabled`, `global_paths`, `full_every`, `full_after`) applies
a selection policy to verify tiers run without `--affected`; `global_paths` globs
force a full workspace selection.

## Provider model scope

`providers.{provider}.models` is a list of model-name patterns (`*` matches any
run of characters). `[providers.cursor]` ships `["auto", "composer-*", "grok-*"]`;
other providers are unrestricted unless set. An explicit list restricts every
automatic choice: shipped and owner pools, routes, preferences, fallbacks and
depletion hand-offs. An empty list excludes every model from automatic routing.
Host settings replace the shipped list.

An explicit primary-model pin outside the scope is launched regardless.
`out_of_scope_pin` (`warn`, `refuse`, `allow`) only changes the warning:
`warn` and `refuse` warn with the setting's source, `allow` stays silent.
`auto` can be pinned but is in no shipped pool: Cursor, not this scope, selects
its underlying model.

## Lane skills and project paths

Set `domains.lanes.user_skills` at host or project scope, or per route in
`domains.routing.model_policy`. Configure project `shared_paths` and
`exec_roots` for writer access, or `domains.run.writer_writes` for host-wide
write grants. `domains.lanes.projects.{project}.grants` names the capabilities
(currently `postgres`) granted to that project's writer lanes; node settings
and invocation flags may override them, and read-only lanes inherit none.
Install renames a stored `capabilities` key to `grants` once; there is no alias.
`provenant fabric identity` prints the project ID.

## Lane verification

Writer lanes run the commands and scopes declared in the project's `verify.lane`
port. A valid declaration is authoritative over the resolved legacy
`lanes.verification` format-only scope; its tiers, commands, scopes and source
also appear in the launch receipt. With no valid declaration writers run the
repository formatter and declared pre-commit hooks. Git hooks are part of
commit and push. Build admission and shared caches remain subject to host
policy.

## Lane history

A lane owns its own branch and may rebase, merge and force-push it with lease while
it is unmerged. `lanes.history.*` decides how the host treats that history; every
value is read from the normal `[knobs]` layer and every warning and refusal names
the setting and its source.

- `lanes.history.rebase` (`allow` default, `refuse`): `allow` publishes a lane
  whose history diverged from the host lane branch by first merging the host
  branch into the lane tip. A lane rewrite replaces the unchanged custody-pinned
  launch tip with a warning and retains it at `refs/provenant/recovery/<oid>`.
  Pushes and remote-tracking refs are not used as ownership evidence; a host tip
  advanced during the lane takes the merge path. A real conflict stays retained with
  the files named; unrelated histories, a configured `merge.*.driver` and a
  branch rewound behind its launch base are retained with their own typed cause.
  `refuse` retains the lane's private commits and refuses with the divergence
  cause.
- `lanes.history.unrooted_tip` (`warn` default, `off`, `refuse`): a lane tip that
  descends from none of its launch base, the origin default, the launch ref, a
  pushed remote-tracking branch or the host lane branch. The head is judged
  before any host merge, so a merge cannot supply the ancestry it lacks. `warn`
  publishes and names the tip and its launch base; `off` is silent; `refuse`
  stops with `LANE_TIP_UNROOTED`. `warn` and `off` decide only this observation:
  when the host branch has unrelated history, `rebase = allow` still refuses the
  merge as `PublicationUnrelated`.
- `lanes.history.conflict_markers` (`warn` default, `off`, `refuse`): added
  lines in a pre-push check that begin with a literal conflict marker. It
  replaces the former `domains.run.push_conflict_markers` boolean.
- `lanes.history.default_branch_rewrite` (`refuse` default, `allow`): the owner
  gate for a non-fast-forward rewrite of the default branch by a push. It is
  host-only, so a `--knob` or graph node cannot grant it. Deleting the default
  branch and pushing it from a linked worktree stay refused under every value.

Set a value with `provenant config set lanes.history.rebase refuse` (or under
`[knobs]`). A dispatch may set `rebase` and `unrooted_tip` for one lane.
`conflict_markers` and `default_branch_rewrite` are read by the host pre-push
hook from the host configuration and are refused in a dispatch override.
No value in this family is strict: an unrecognised value warns with the layer
that supplied it and keeps the documented default, so a typo neither refuses the
configuration nor silently chooses a policy.

## Git pre-push hook and local gate

Connect the Git pre-push hook so Provenant checks pushes, including its
default-branch protection. Pass both Git arguments and leave standard input
connected: Git gives the remote name and URL as arguments and ref updates on
standard input. A plain `.git/hooks/pre-push` (executable):

```sh
#!/bin/sh
exec provenant hook git pre-push "$@"
```

For Husky, put `provenant hook git pre-push "$@"` in `.husky/pre-push`. For
Lefthook, set `use_stdin`; only one command per hook can take standard input, so
keep other checks in the manager's own hook:

```yaml
pre-push:
  commands:
    provenant:
      run: 'provenant hook git pre-push "{1}" "{2}"'
      use_stdin: true
```

The optional local gate is `[domains.pr.land.local_gate]`, with per-project and
per-tier overrides; `provenant config schema` lists its keys.

## Worktree setup

A new worktree runs the `post_create` port's command, if the port has a `command`
backend, with the branch and worktree path as arguments and the worktree as its
directory. `[worktrees] timeout_seconds` (per project under
`[worktrees.projects."PROJECT_ID"]`) bounds it. If setup fails, read its output and
retry by hand in the new worktree. [Worktrees](../worktrees.md) has the procedure.

## Run host policy

`[domains.run.host_policy]` (`deny_read`, `deny_write`, `network`) restricts host
paths or network access. Boundaries: [ADR 0040](../adr/0040-open-lane-policy-with-explicit-boundaries.md)
and the [supervisor spec](../specs/supervisor.md#permission-policy).

## Lane sandbox mode

A lane runs sandboxed when its route matches `domains.run.sandboxed_routes`, its
parent was sandboxed, `lanes.sandbox` (or `--sandbox`) says so, or an explicit
network restriction or host path denial needs kernel enforcement; `--no-sandbox`
or `lanes.sandbox = "off"` asks for none and overrides the route list with a
warning. `domains.run.sandbox_cache_reads` lists host caches a sandboxed lane may
read. If the backend is unavailable, a lane that needs an enforcing profile fails
with a capability error rather than running unconfined. The attempt receipt
records the selected mode and its source. Choose a trusted unsandboxed route
explicitly when that is the authority you intend.

Start shared daemons on the host, or close inherited descriptors when a lane
starts one, so the attempt can settle.

## Unreadable process census exceptions

By default, `confinement.census_ignore_unreadable` excuses Apple-platform
`/usr/bin/login` only when its kernel start time precedes every scoped root's
creation floor by at least two seconds (`started_before = "root"`). Override or
clear it only for signed executables whose risk you accept; never list shells
or interpreters. See the
[supervisor spec](../specs/supervisor.md#unreadable-processes-need-positive-non-occupancy-evidence).

## OpenCode Go quota calibration

Enter dashboard usage floors under `[domains.quota.calibration.opencode-go]` and
set `expires` to the earliest included window end. Then check `provenant quota`.

## Chair account switching

Register the Claude accounts ([install](install.md#install)), then:

```sh
provenant config set chair.switch_accounts '["work-spare","personal-spare"]'
provenant config set chair.switch_threshold 90
provenant config set chair.auto_switch true
provenant chair switch --dry-run
provenant chair switch --to work-spare --session-id ID
```

An empty list permits no targets. A manual switch skips `auto_switch` but needs an
allowed account with headroom and a session ID (`--session-id`, then
`CLAUDE_CODE_SESSION_ID`, then `CLAUDE_SESSION_ID`). The prompt hook reads cached
quota only, and stale or unknown quota cannot establish headroom, so run
`provenant quota tick --adapter claude` first. The hook saves a private
checkpoint and prints the launch command; the source session stops and the new
one injects the hand-off once. Collect the checkpoint's finished lane results
rather than dispatching them again.

## Named admission classes

Heavy commands coordinate across lanes through classes under `[admission.<class>]`
in the host `<state>/config.toml`, host-project `<state>/projects/<project_key>/config.toml` or
project `.provenant/config.toml` (later wins), resolved by
[`admission.rs`](../../tools/front-door/crates/config/src/admission.rs):

```toml
[admission.build]
slots = 2
wait_seconds = 15
memory_per_slot = "4 GiB"
disk_floor = "10 GiB"
trim_action = "cargo clean --package test-artifacts"
```

- `slots`: concurrent processes admitted. `build` defaults to a count derived from
  CPU cores and RAM; other classes default to 1. `0` removes the cap.
- `memory_per_slot`: turns on RSS sampling (every 5000 ms unless
  `domains.run.resource_sample_interval_ms` says otherwise); with no sample the limit is
  skipped with a warning.
- `disk_floor`: minimum free disk before running.
- `trim_action`: shell command run to free resources when a limit is exceeded,
  before re-sampling.
- `wait_seconds`: bounded local wait (default 15). On expiry the command runs
  without a slot and records an `overflow` exception. With `build.overflow =
  auto`, waiters queue until a slot frees or the job is cancelled. No saturated
  class refuses a command.

`provenant admit <CLASS> -- <COMMAND> [ARGS...]` takes a slot by hand for a
host job such as the chair's warm check; lanes never call it. A nested call
inside an admitted command passes through on the inherited lease.
`provenant status` shows holders, waiters, wait times and classes.
