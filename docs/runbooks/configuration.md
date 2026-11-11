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

- `fabric.mcp_server` is off by default: set it, then `provenant upgrade` (or
  `install`) to write the entry. An edited `fabric` entry blocks the install
  with a conflict; restore or remove it.
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

## Run host policy

`[domains.run.host_policy]` (`deny_read`, `deny_write`, `network =
"open|local|provider"`) adds optional restrictions on top of the mandatory
boundaries; absolute paths only, no per-project override. The effective
network is the narrower of the ceiling and the lane's request.

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
