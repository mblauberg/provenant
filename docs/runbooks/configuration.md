# Harness configuration
Owner: provenant-config
Anchor: [`config/harness-knobs.json`](../../config/harness-knobs.json); [`LaneBuildSettings`](../../tools/front-door/crates/config/src/gc.rs)

Settings live in the selected state root's `config.toml`; `provenant config
schema [--all]` lists every key with its table, default, bounds and effect,
`config show` the effective values and sources, `config check` every warning
with its file and key. This page covers only what those cannot: safety rules
and operator decisions. Edit with `config set|unset|apply` (one lock, whole-file
validation, `apply --expect-revision`); tables outside `[knobs]`, such as
`[domains.fabric.lanes]` and `[gc]`, are edited by hand.

## Precedence and refusals

A knob in `[knobs]` beats the `profile` preset (`balanced` or `economy`), which
beats the registry default; there is no project overlay. Unknown keys and
invalid values warn and fall back, naming the file, line and key; a misspelt
knob is ignored and the warning suggests the nearest name; an unquoted
`lanes.publish = false` is the knob `lanes.publish`. Recovery never widens
a restriction: a `strict` knob (`lanes.publish`, `fabric.accept_handoff`,
`chair.auto_switch`, `chair.switch_accounts`, `lanes.max_depth`,
`confinement.*`, `routing.allow_unknown_partial_headroom`) takes its
restrictive value when its entry is invalid, but one with no restrictive value
(the `routing.*exclude` lists), or a `knobs` that is not a table, refuses the
file, naming the line and key. These also
refuse the whole file: invalid credential, authority, confinement and
Git-boundary settings outside `[knobs]`, enforced model policy, Fabric protocol
caps, `graph.allowed_read_roots`, `home_hosts`, executable overrides, and syntax
or schema-version errors. A malformed model-policy rule or pacing override is
dropped whole.

## Settings with sharp edges

- `lanes.publish = false` withholds the host `gh` login and Git credential
  helper from open-network lanes. If `gh auth token` fails, the lane starts
  without `GH_TOKEN` with a warning and `git push` fails with `could not read
  Username`. `ps`, `top`, `su` and `sudo` always fail in a confined macOS lane;
  use `pgrep` or `kill -0`.
- `chair.refusal_notice = "inbox"` posts a notice when a submission is refused
  pre-admission; if posting failed, run `provenant reconcile` from that session.
- `[domains.run] writer_writes = ["/abs/dir"]` grants worktree-write lanes
  read, write and exec on a subtree. Too-broad or credential entries refuse the
  file; entries overlapping state or Git metadata refuse the launch.
  `review_exempt` paths do not count toward an `uncommitted` outcome.
- Admission: `lanes.max_concurrency` (writers) and `lanes.max_concurrency_read_only`
  cap batches; `[domains.fabric.lanes]` `max_lanes`, `max_writer_lanes` and
  per-project variants cap live lanes (all off unless set); `status` shows what
  a queued lane waits for. `[domains.lanes.projects.ID]` also takes
  `capabilities = ["postgres"]`.
- Build slots throttle compiling `cargo` commands to `max_concurrent_builds`
  per state directory; a waiter fails after `build_wait_minutes` and
  `status` names holders. A build that scrubs its environment must keep
  `PROVENANT_BUILD_SLOT` or it queues behind its own parent. `lanes.local_cargo`
  (`all`, `check`, `none`) refuses compiling cargo commands in lanes; no project
  file sets it, so set `check` under `[knobs]` on each host that builds this
  repository. Lane builds default to `CARGO_INCREMENTAL=0` and line-table debug
  info (`cargo_incremental`, `cargo_debug`).
- Provider updates run daily when idle: `[providers.codex] update = { command =
  ["brew", "upgrade", "codex"] }` or `update = false`. A custom command must not
  daemonise; a busy provider refuses lane preparation as `provider_updating`.
  `runtime_deadline_seconds` under `[providers.ID]` caps a lane's wall clock.
- `[domains.routing.opencode_go] min_monthly_allowance_usd` refuses Go models
  below the floor or with unknown allowance; zero disables.
- Retention and collection (`[retention]`, `[gc]`, `[gc.projects."ID"]`) are in
  [gc](gc.md).

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

The optional local gate runs after the push checks in the selected project
checkout. Configure it in the host `config.toml`:

```toml
[domains.pr.land.local_gate]
mode = "warn"              # warn (default), enforce or off
commands = ["cargo check"] # default: []
timeout_seconds = 60       # per command; positive integer
```

Commands run in order through `sh -c`, with a separate timeout for each.
`warn` reports failures and continues; `enforce` refuses the push at the first
failure; `off` or an empty command list skips the gate. Failure includes a
redacted, bounded tail of combined stdout and stderr. Project-specific values
can override these keys under `[domains.pr.land.projects."ID".local_gate]`.

## Worktree setup

Project setup uses the existing project-ID overrides in the selected state
root's `config.toml`, rather than a repository overlay for `[knobs]`. Find the
project ID with `provenant fabric identity`, then declare shell commands:

```toml
[worktrees.projects."PROJECT_ID"]
setup = ["pnpm install --frozen-lockfile", "./scripts/check-links"]
timeout_seconds = 300
```

`[worktrees]` supplies host-wide defaults for the same fields. Setup is unset
by default (an empty command list); a project's list replaces the default,
and `setup = []` disables it for that project. `config show` reports these
settings and their sources; `config schema` lists their types and bounds.
The total timeout across all commands defaults to 300 seconds and accepts
1 to 3600 seconds.

Commands run in order through `/bin/sh -c`, with the new worktree as their
working directory, after `worktree create` or writer `run --branch/--base`
creates it and before the writer starts. A nonzero exit, timeout or launch
failure stops setup and warns with the captured output tail, leaving the
worktree usable. Reusing an existing worktree never runs setup again; retry a
failed setup manually from that worktree. Configure only commands you trust
to run on the host.

## Run host policy

`[domains.run.host_policy]` (`deny_read`, `deny_write`, `network =
"open|local|provider"`) adds optional restrictions on top of the mandatory
boundaries; absolute paths only, no per-project override. The effective
network is the narrower of the ceiling and the lane's request. Path denials
apply only to sandboxed lanes; keep a route in `sandboxed_routes` to keep a
path from it. A narrowed network sandboxes the lane unless an explicit `off`
overrides that restriction.

## Lane sandbox mode

Each macOS attempt resolves whether it runs under Seatbelt. The registry default is
`auto`: every lane kind (writers and read-only lanes alike) runs unsandboxed unless
a configured floor requires sandboxing. Only an explicit `off` overrides those
floors. In precedence order:

1. **Unsupported backend:** Off macOS every lane resolves as sandboxed (`platform`),
   so Linux keeps refusing lane launch.
2. **Inherited OS sandbox:** A process already running inside an operating-system sandbox
   (e.g. Seatbelt) cannot remove it. An inherited OS sandbox is always reported honestly as
   effective (`parent`), even if unsandboxed execution was requested.
3. **Explicit knob:**
   - `on` / `--sandbox` explicitly sandboxes the lane (`knob`).
   - `off` / `--no-sandbox` explicitly runs the lane unsandboxed (`knob`). An explicit
     `off` takes precedence over route floors (`sandboxed_routes`), narrowed-network
     floors, and parent floors where Provenant decides them, emitting a one-line warning
     naming each restriction dropped.
4. **Default (under `auto` or unset knob):**
   - Floors apply when configured: a lane started by a lane that was itself sandboxed
     by distrust (`parent`), a route id matching `[domains.run] sandboxed_routes` (`route`;
     empty by default), or an effective network of `provider` or `local` (`network`). "Started by"
     covers `run` from inside a lane and every graph node a lane or planner adds or replaces:
     admission names the admitting node in `attempt.admitted_by`, overwriting any value the author
     wrote. A sandbox that only enforces read-only access or a narrowed network does not pass
     on, so a read-only planner's writers stay unsandboxed. An unreadable record, or an admitting
     attempt on another host, counts as sandboxed.
   - When no floor applies, every lane kind runs unsandboxed (`default`).

The lane sees `PROVENANT_SANDBOX=on|off`. `PROVENANT_PROCESS_TREE` is the
path to the supervisor's lane process-tree sample, used by `provenant
process-tree`; preserve it when invoking that tool from a scrubbed environment.
The receipt's `applied_route.sandbox` and `status` record requested and
effective mode and its source.

`[domains.run] sandbox_cache_reads` (absolute directories, replaced per project
by `[domains.run.projects."ID"] sandbox_cache_reads`) gives sandboxed lanes
read-only access to host caches such as an sccache directory or prebuilt
database clusters. Missing paths are skipped; paths overlapping the lane's own
grants keep the grant. Unsandboxed lanes need no grant.

What holds in each mode:

- **Credentials.** Every lane's environment is rebuilt from the adapter's
  allowlist, and hosted-cloud and API-key variables (`AWS_*`, `NEON_*`,
  `CLOUDFLARE_*`, `*_API_KEY`, `*_ACCESS_TOKEN` and similar; `GH_TOKEN` is
  kept for publishing) are stripped. An unsandboxed lane can read on-disk
  credential stores such as `~/.aws` and `~/.ssh`: route trust replaces the
  file denial.
- **Git.** Writer custody (one registered writer per worktree), the lane's
  private Git directory and host-owned delivery with object and expected-ref
  checks hold in both modes. Only a sandboxed lane is kernel-denied writes to
  the shared Git directory, other worktrees and the primary checkout; an
  unsandboxed writer is trusted not to bypass its Git shim or write there.
- **Host state.** An unsandboxed lane can also write the state directory,
  including `config.toml` (`sandboxed_routes`, `[knobs]`) and run records, so
  every floor above holds only as far as the unsandboxed routes are trusted.
- **Process tree.** Sandboxed lanes are walked by their sandbox mark,
  unsandboxed lanes by ancestry, so heavy-lock accounting covers both.
  Cleanup of escapees that left the process tree relies on the sandbox mark:
  a daemon an unsandboxed lane detaches (for example `setsid`, `pg_ctl` or a
  compiler-cache server) is not killed, and while it holds the lane's
  inherited descriptors the attempt cannot settle. Start shared daemons from
  the host, or close inherited descriptors when a lane starts one.

## Unreadable process census exceptions

`confinement.census_ignore_unreadable` accepts exact signed executables
(`{ path, team_id }`, `team_id = "platform"` for Apple) whose unreadable
process entries land cleanup may ignore. A signature does not prove
non-occupancy, so list only pairs whose risk you accept. Host-only; task
overrides refuse it. See the
[supervisor spec](../specs/supervisor.md#unreadable-processes-need-positive-non-occupancy-evidence).

## OpenCode Go quota calibration

OpenCode exposes no account-wide usage, so ledger usage is a lower bound. Enter
dashboard percentages as floors; quota and pacing use the higher of ledger and
floor until `expires`, which must be the earliest included window end:

```toml
[domains.quota.calibration.opencode-go]
five_hour = 43
seven_day = 17
thirty_day = 59
expires = "2026-11-04T00:00:00Z"
```

Provisional Go pool members stay excluded while their windows are
uncalibrated; accept unknown headroom on this host with
`provenant config set routing.allow_unknown_partial_headroom true`.

## Chair account switching

Opt-in, no daemon. Register each Claude account
(`provenant accounts register LABEL --adapter claude --directory ~/.claude-LABEL`;
the directory must exist, be signed in separately, and `~/.claude` is never a
target), then:

```sh
provenant config set chair.switch_accounts '["work-spare","personal-spare"]'
provenant config set chair.switch_threshold 92
provenant config set chair.auto_switch true
provenant chair switch --dry-run
provenant chair switch --to work-spare --session-id ID
```

An empty list permits no targets. A manual switch bypasses `auto_switch` but
needs an allowed account with headroom and a session ID (`--session-id`, then
`CLAUDE_CODE_SESSION_ID`, then `CLAUDE_SESSION_ID`). The prompt hook reads cached
quota only: stale (over 15 minutes), missing or unknown quota cannot establish
headroom, so refresh with `provenant quota tick --adapter claude`. The hook
saves a private checkpoint, then prints (or in a Herdr pane runs) the launch
command, and the source session stops; the new session injects the hand-off once
within an hour. Read the checkpoint's lane IDs and collect finished results
rather than dispatching them again.
