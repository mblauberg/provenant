# Harness configuration
Owner: provenant-config
Anchor: [`config/harness-knobs.json`](../../config/harness-knobs.json); [`LaneBuildSettings`](../../tools/front-door/crates/config/src/gc.rs)

Settings live in the selected state root's `config.toml`. The
[configuration keys](../reference/config-keys.md) and the
[advanced reference](../reference/config-keys-all.md) are generated from the
registry and list every key with its table, default, effect and bounds. This
runbook covers what the reference cannot: precedence, safety rules and the
operator decisions behind each setting.

```sh
provenant config schema [--all]        # editable fields, bounds, defaults, source
provenant config show                  # effective values, sources and revision
provenant config check                 # every warning with its file and key
provenant config set lanes.max_concurrency 4
provenant config unset lanes.max_concurrency
provenant config apply --file PATCH.json --expect-revision REV [--dry-run]
provenant config history [--since T] [--limit N]
```

`--state-dir /absolute/path` selects an isolated root. `config set`, `unset`
and `apply` share one lock and one editor that validates the whole result
before replacing the file. `apply` takes a flat JSON object of field to value
(null clears a field) and commits all of it or none; a stale revision is
refused and names the current one. History redacts secret-looking values.
Host tables outside `[knobs]`, such as `[domains.fabric.lanes]` and `[gc]`,
are edited by hand.

## Precedence and stamps

The file needs `schema_version = 1`; a missing file selects defaults. A knob in
`[knobs]` wins over the `profile` preset (`balanced` or `economy`), which wins
over the registry default. There is no project overlay or environment layer.
A dispatch's `--knob NAME=VALUE`, or `knobs: ["NAME=VALUE"]` on `fabric_run`,
wins over configuration. Each sealed task and attempt records the profile,
effective knob values and dispatch overrides, and continuations keep that
stamp.

Unknown knobs are refused. Other unknown keys and invalid optional values warn
and fall back to their defaults; each warning names the key, and valid siblings
in the same table still apply. A model policy rule or pacing override whose
selector or schedule is malformed is dropped whole, because without it the
record would apply more widely. Syntax, file size and schema-version errors are
hard failures. Recovery never widens a restriction, so these also refuse the
file:

- knobs marked `strict` in the registry, whose default could grant what the
  user withheld (`lanes.publish`, `fabric.accept_handoff`, `chair.auto_switch`,
  `chair.switch_accounts`, `lanes.max_depth`,
  `confinement.census_ignore_unreadable`), and a `knobs` or `domains` value
  that is not a table;
- invalid known credentials, authority, confinement and Git-boundary settings;
- enforced model policy (`lane.read_only`, `lane.max_context`,
  `lane.max_turns`), and a malformed rule that carries one;
- Fabric protocol caps (`limits.*`, which may only be lowered), graph limits
  and budgets, `graph.allowed_read_roots`, `home_hosts` and executable
  overrides;
- quota probe and history bounds.

The `review` knobs and the `rigorous` profile are refused; run
`provenant config migrate` to remove them.

For model effort, context and compaction, see [model policy](model-policy.md);
for pools, councils and pacing, see [model routing](model-routing.md).

### Refusal notices

`chair.refusal_notice = "inbox"` (the default) posts a path-free notice to the
bound chair's inbox when a graph submission is refused before admission;
retries reuse the message identity. If posting fails, restore inbox access and
run `provenant reconcile` from the submitting session. It retries every pending
notice and reports any that still need recovery.

## Lane GitHub access and `ps`

Every lane with open network, read-only lanes included, receives the host's
`gh` login and a Git credential helper, so it can read and comment on issues
and a writer can push its own branch. `lanes.publish = false` withholds them;
because the knob is strict, a typo refuses the file rather than silently
re-enabling access. The credential comes from a generated
`publishing.gitconfig` that the lane's `git` wrapper includes through
`GIT_CONFIG_GLOBAL`, not from host Git configuration. Provider credential files
and keychains stay denied.

The host runs `gh auth token` up to three times when it times out or is
signalled. If every attempt fails, the launch prints
`warning: <route>: GitHub login lookup failed (<cause>)` and the lane starts
without `GH_TOKEN`; with a keychain login, `git push` then fails with
`could not read Username`. The warning never contains the token.

`/bin/ps`, `top`, `su` and `sudo` exit with "Operation not permitted" in a
confined macOS lane: a sandboxed process cannot exec a setuid image. Use
`pgrep` or `kill -0 PID`. No setting changes this.

## Unreadable process census exceptions

Land cleanup refuses an incomplete census for possible writers. Stable foreign
system credentials now scope out unrelated system-process gaps without an
executable listing; positive occupancy still keeps the worktree. The
[census policy](../specs/supervisor.md#unreadable-processes-need-positive-non-occupancy-evidence)
owns that trust assumption and the cases that remain closed.

The host-only signed-image listing remains a fallback when direct credential
evidence is unavailable. A refusal that reaches this image policy reports its
pid, path, uids, code-signing flags and team identifier. The host owner may
accept specific signed executables through that fallback:

```toml
[knobs]
"confinement.census_ignore_unreadable" = [
  { path = "/Library/Example/daemon", team_id = "TEAM123456" },
  { path = "/usr/bin/login", team_id = "platform" },
]
```

`config set` and `config apply` accept the same list as JSON. Entries are exact
absolute paths, not prefixes or globs; `platform` selects Apple platform
signing instead of a team string. Any malformed entry refuses the whole file.
The knob is host-only: task overrides refuse it, even an empty list. It grants
no signalling or removal authority, and destructive operations still take fresh
evidence under writer custody.

Each match also requires live signing checks, an exact kernel executable path,
and system-account uids that differ from every candidate root's owner; a
`platform` entry additionally needs a root-owned, non-writable path to `/`.
The [supervisor spec](../specs/supervisor.md) owns those conditions. The
accepted residual risk is that a listed executable may be sitting in a
worktree: a signature does not prove non-occupancy. List only executable and
signer pairs whose occupancy risk you accept.

`confinement.census_exclude_ancestors` (default `true`) excludes the invoking
shell's ancestor chain from run admission's live-writer detection. Set it to
`false` to treat an ancestor shell inside the worktree as a live writer. It
does not change land policy.

To assert a clean census in the native acceptance test, list this host's
unreadable daemons, then set `PROVENANT_CENSUS_HOST_LIST_COMPLETE=1`. Without it
the test reports that the clean-Free assertion was skipped.

## Run host policy

`[domains.run.host_policy]` adds optional read, write and network restrictions
to the mandatory lane boundaries:

```toml
[domains.run.host_policy]
deny_read = ["/absolute/private-directory"]
deny_write = ["/absolute/archive"]
network = "local" # open, local or provider
```

With no table, nothing is denied and the network is `open`. The table has no
project, lane or child override; `config show` names the file and section it
came from. Paths are absolute normalised subtrees; the filesystem root is
omitted with a warning. Invalid entries warn individually, and a malformed
network value falls back to `open`. These tolerant rules apply only to this
optional policy, never to the mandatory credential, state, Git and source
boundaries.

The effective network is the narrower of this ceiling and the lane's request,
and adapter offline requirements still deny all network. macOS enforces the
policy with Seatbelt; Linux has no enforcing backend and refuses to launch
rather than run unconfined. See
[lane policy and credential exceptions](providers.md#lane-filesystem-and-network-policy).

`[domains.run]` also holds the capacity limits for sandbox profiles, grants and
continuation copies. A refusal names the limiting setting, its value and its
source; raise it and retry. These limits never relax path, credential or Git
checks.

## Writer extra write paths

Writer lanes can write shared run state outside the worktree once the host
owner lists the directories:

```toml
[domains.run]
writer_writes = ["/Users/me/proj/.agent-run/locks"]
```

Each entry is an absolute directory, granted read, write and exec as a subtree
to worktree-write lanes only. The file is refused if an entry is too broad or
names a credential directory such as `~/.ssh`; a launch is refused if it
overlaps the state directory's protected parts or Git metadata, or is or
encloses another checkout. Other checkouts of the project stay read-only to a
writer, except an entry strictly inside one, such as the lock directory above.
Graph write roots get no such exemption; see
[writer lanes](providers.md#writer-lanes).

`review_exempt` lists repository-relative paths that do not count toward an
`uncommitted` outcome: a trailing slash names a root directory, a bare filename
matches at any depth, and other paths match exactly. A project list replaces
the host list, and an empty list disables exemptions; `provenant status` shows
the effective list.

`push_conflict_markers = false` turns off the pre-push hook's advisory check
for added `<<<<<<<` and `>>>>>>>` lines, for fixtures that need them. Pushes
that delete, rewrite or push from a linked worktree to the default branch are
refused either way, and an unreadable config keeps the check on.

## Lane admission

Batch concurrency comes from `lanes.max_concurrency` when any lane in the batch
writes a worktree, and from `lanes.max_concurrency_read_only` otherwise. A
`lanes.max_concurrency` you set yourself in `[knobs]` also caps the read-only
default unless you set both; a profile value does not. Document concurrency
overrides either.

Host admission is separate. `[domains.fabric.lanes]` caps live lanes with
`max_lanes`, `max_project_lanes`, `max_writer_lanes` and
`max_project_writer_lanes`; every cap is off unless set. A lane at a cap waits
until its admission deadline and is then refused with the cap, setting and
source. `start_spacing_ms` spaces starts within each class, so a read-only lane
never waits behind a writer; spacing is enforced at admission and resets on
reboot. On macOS, starts defer at critical kernel memory pressure
(`memory_pressure_threshold`). The `gc.free_floor` disk floor applies to both
classes, and below it each running lane's supervisor pauses that lane's own
builds until space returns; the [disk spec](../specs/disk.md) owns that rule
and [GC](gc.md) owns the collection that runs first. `status` shows what a queued lane waits for, such as
`read-only startup spacing: 1.2 s left of 2.0 s`.

Under `[domains.lanes.projects.<project>]`, `shared_paths` shares an existing
host test-lock directory with writers, `exec_roots` grants read and execute on
checkout-relative toolchain directories, and `capabilities = ["postgres"]`
grants database IPC to writer lanes by default. A graph node's `capabilities`
(an empty array removes the default) and the `--capability` and
`--no-capability` flags override it. See
[project paths](providers.md#project-paths-and-host-checkout-identity).

`lanes.launch_retry_limit`, `lanes.launch_retry_first_seconds` and
`lanes.launch_retry_max_seconds` set retries after a transient launch failure;
delays double up to the cap, and the admission deadline can end the wait
sooner.

## Build slots

Build slots throttle Cargo builds so lanes share CPU and memory fairly. They
are not a lane cap and not a security boundary: a writer is admitted on the
writer caps, however many builds are running.

A writer lane gets a `cargo` shim first on its `PATH` when its environment sets
`CARGO_TARGET_DIR` and `~/.cargo/bin/cargo` exists. A cargo command that
compiles (`build`, `check`, `clippy`, `test`, `run`, `nextest` and the rest of
that set) takes one slot of `max_concurrent_builds` and holds it until cargo
exits, so `cargo run` and `cargo test` hold it for the whole program or test
run. `fmt`, `metadata` and `tree` take none, and nor does a `cargo nextest run`
with both `--cargo-metadata` and `--binaries-metadata`, which reuses an
existing build.

A command that finds every slot taken prints
`provenant: waiting for a build slot (n of m in use)` about every minute and
waits up to `build_wait_minutes` in total, including any wait for another
build's `.cargo-lock`; then it fails and suggests `provenant status` to find the
holders. While it waits, `status` shows
`build slot: waiting N of M (longest first); H of S slots held`. The rank is an
estimate: slots go to whichever waiter polls first. Writer preambles tell lanes
the wait is normal and bounded.

A nested cargo skips the gate only when `PROVENANT_BUILD_SLOT` names a live
provenant ancestor that holds a slot. A build that scrubs its environment loses
the marker and queues behind its own parent's slot until it times out, so keep
`PROVENANT_BUILD_SLOT` in the environment of any build that runs cargo itself.

The slots are `flock`ed files under `<state>/build-slots/`, shared by every
project on the state directory. Lanes can read but not replace them, and the
kernel releases a slot when its shim exits, even after `kill -9`. A slot count
and wait are fixed when a lane launches. By design, a lane that runs
`~/.cargo/bin/cargo`, `rustc` or a non-cargo build directly is not gated, and a
slot directory that cannot be locked lets the build run with one warning.

`cargo_build_jobs` sets each build's `CARGO_BUILD_JOBS`; an existing value in
the submitting environment or the lane wins. `lane_port_base`,
`lane_port_block` and `lane_port_blocks` allocate a port block to each writer
(see [providers](providers.md#writer-lanes)). Exhausting the default pool warns
and continues; an explicitly configured pool refuses when full.

### Lane build output

Every lane gets `CARGO_INCREMENTAL=0` and
`CARGO_PROFILE_DEV_DEBUG=line-tables-only`, so its target holds no incremental
sessions and carries line tables instead of full debug information. Override
them with `cargo_incremental` and `cargo_debug`; a value already in the lane
environment wins. Collection of idle lane build output, bounded by
`build_dir_max_bytes`, is described in [GC](gc.md). Lane Cargo paths and the
host Cargo seed are described in [worktrees](../worktrees.md).

#### Which cargo commands a lane may run

`lanes.local_cargo` limits what the lane's `cargo` shim runs, for every
provider:

| Value | Runs | Refuses |
|---|---|---|
| `all` (default) | every cargo command | nothing |
| `check` | `cargo check` and subcommands that do not compile | every other compiling command |
| `none` | subcommands that do not compile | every compiling command |

A refused command exits 1 with one line naming the knob and saying CI runs it.
Set it under `[knobs]` for the host, with `--knob lanes.local_cargo=check` for
a run, or with `"attempt": {"local_cargo": "check"}` for one graph node, which
wins over the run. A cargo nested inside an allowed command is not checked
again, and a lane that calls `~/.cargo/bin/cargo` or `rustc` directly is not
stopped. No project file sets lane settings, so set `check` for this repository
under `[knobs]` on each host that builds it.

## Lane cache collection

Retention and collection settings live in `[retention]` and `[gc]`:

```toml
[retention]
provider_state_days = 7

[gc]
schedule = true
lane_cargo_cache_max = "32G"
# free_floor = "100G"

# Use the project ID from `provenant fabric identity`.
[gc.projects."PROJECT_ID"]
regenerable = ["node_modules", "target"]
keep = ["web/target"]
foreign_worktrees = "keep" # opt in with "merged-clean"
```

The [advanced reference](../reference/config-keys-all.md#host-gc-settings)
lists every key with its default and bounds. [GC](gc.md) owns what each
collection removes, keeps and reports; [disk inspection](disk.md) covers
measuring what is using space. Completion cleanup is independent of the daily
schedule: settlement publishes durable intent without waiting for collection or
dispatch locks, and the host tick dispatches the worker. Unlanded refs remain
regardless of age; there is no `cleanup.unlanded_days` setting.

## Provider updates

Provider updates run by default, at most once a day per provider, outside
lanes and only while the provider is idle. Lanes keep self-update disabled.

```toml
[providers.codex]
update = { command = ["brew", "upgrade", "codex"] }

[providers.opencode]
update = false
```

`update = true` selects the detected native updater (`claude update`,
`opencode upgrade`, or Homebrew or npm for Codex); a provider without a known
method needs a `command` argv, executed without a shell. A quota tick schedules
due updates in a detached worker, at most once every 15 minutes; an unknown
installation produces one activity note until it is resolved.

An update holds an exclusive lease on its provider, and preparation of a lane
holds a shared one. A busy provider drops out of a lane's candidates; when every
candidate is busy, preparation waits up to `--wait` (at most 30 seconds) and
then refuses with `provider_updating`. A timeout kills the update command's
process group, so a custom command must not daemonise: a descendant that leaves
the group with `setsid` keeps the lease until it exits. An interrupted update
is recorded as failed and retried after the interval with the same updater.
`<state>/provider-updates/worker.json` holds the last worker launch and its
failure, if any; activity records old and new versions.

## Codex cloud transport

```toml
[providers.codex.cloud]
transport = "thread"
command = "codex-cloud-task"
```

`official` uses `codex cloud exec`, `status` and `list`. Select `thread` when
the workspace's environments use the cloud thread API, which official
`codex cloud exec` may report as `no cloud environments are available for this
workspace`, and install a compatible client separately. The client must accept
`submit --env NAME_OR_ID [--read-only]` with the prompt on stdin, and
`status THREAD_ID` returning thread and turn JSON. Keep the same transport and
command while observing saved jobs after a restart. See
[cloud placement](providers.md#cloud-placement).

## Lane runtime deadline

A lane may run for its task's provider timeout; `runtime_deadline_seconds` under
`[providers.<id>]` caps it, and the shorter applies. The deadline is wall-clock,
so a host that sleeps through it stops the provider on waking. The attempt
settles as `timed_out`, releases its worktree and lock, and `status` shows a
`provenant retry` hint.

## Store busy timeout

`[domains.fabric.limits] store_busy_timeout_ms` bounds how long a command waits
for a locked local store before it reports contention (exit 3). Zero fails fast.
It is read once when a command starts; a hook with a shorter deadline waits no
longer than that.

## OpenCode Go

`[domains.routing.opencode_go] min_monthly_allowance_usd` refuses Go models,
including explicitly selected and uncatalogued IDs, whose documented monthly
allowance is below the floor or unknown. Set it to zero to disable the policy.
It does not apply to OpenCode Zen.

## Quota probe batching

`[domains.quota]` `max_parallel` and `wall_budget_seconds` bound a quota tick.
Each probe also has a 55-second claim window and a deadline one second before
that claim expires.

## OpenCode Go quota calibration

Go usage from this host's ledger is a partial lower bound: OpenCode exposes no
account-wide usage across devices. Enter dashboard percentages as floors:

```toml
[domains.quota.calibration.opencode-go]
five_hour = 43
seven_day = 17
thirty_day = 59
expires = "2026-11-04T00:00:00Z"
```

The table key is the account key, usually `opencode-go`. Percentages are
integers from 0 to 100, and `expires` is a required timestamp with a UTC
offset. Quota and pacing use the higher of the ledger and each floor until
expiry, even across a window reset. The table has one expiry, so set it to the
earliest included window end; a five-hour floor entered at 09:00 for a window
ending at 13:00 expires at 13:00. Recheck the dashboard and refresh the floors
after a reset or heavy use on another device. Edits apply on the next read.

Provisional Go pool members are excluded while their partial windows are
uncalibrated. To accept unknown headroom on this host, run
`provenant config set routing.allow_unknown_partial_headroom true`; a dispatch
cannot override it. An observed lower bound at the limit remains exhausted.

## Quota polling intervals

```toml
[domains.quota.polling_intervals.codex]
timer_seconds = 600
panel_seconds = 120
```

Set timer and panel intervals per registered adapter, in whole seconds;
omitted values keep the provider default. Battery doubles each interval and
low power quadruples it. Manual ticks keep their 30-second interval. Quota
ticks, `quota show` staleness and routing admission all use these values.

## Routing pools

Routing changes are stored per state root and apply on the next route command:
`provenant route set`, `route pacing`, `route remove POOL MODEL` and
`route reset POOL`. Inspect weights with `provenant route list --pool NAME`.

## Quota menu display

Per account, run `provenant accounts set <account> [--display-name NAME]
[--hidden true|false] [--order N] [--headline auto|5h|weekly|default]`, where
`<account>` is an entry ID or a label unique across adapters. An empty name,
`--order 0` and `--headline default` clear the field. Globally,
`provenant quota display [--headline auto|5h|weekly] [--icon
most_constrained|<account>]` writes `[domains.quota.display]`. The menu bar
applies hidden accounts and order; the CLI does not. An icon naming an
unregistered account falls back to `most_constrained` with a
`display_icon_warning`.

## Chair account switching

Claude chair switching is opt-in and has no daemon. Register each account with
`provenant accounts register <label> --adapter claude --directory <dir>`. The
directory must be exactly `~/.claude-<label>`, already exist and be signed in
separately. The current `CLAUDE_CONFIG_DIR` must match a registration;
`~/.claude` is never a switch target. Credentials and identity are never copied
between directories. [Account sync](install.md#account-sync) keeps managed
content and shared settings identical across registered directories.

```sh
provenant config set chair.switch_accounts '["work-spare","personal-spare"]'
provenant config set chair.switch_threshold 92
provenant config set chair.auto_switch true
provenant chair switch --dry-run
provenant chair switch --to work-spare --session-id <host-session-id>
```

An empty `chair.switch_accounts` permits no targets. A manual switch bypasses
`chair.auto_switch` but still needs an allowed account with headroom, and a
session ID from `--session-id`, `CLAUDE_CODE_SESSION_ID` or `CLAUDE_SESSION_ID`,
in that order, so a retry cannot launch a second successor. A dry run reports
the target, checkpoint path and launch command without writing anything.

The managed `UserPromptSubmit` hook reads cached quota only. When either the
five-hour or weekly window reaches the threshold, the first allowed account
with both windows below it wins. Quota that is missing, expired, future-dated
or more than 15 minutes old cannot establish headroom; refresh it with
`provenant quota tick --adapter claude`. Unknown current quota leaves the
source running. When no target qualifies, the hook records one refusal per
source session and lets the turn continue.

Before launch, the hook saves a private checkpoint under
`<state>/chair-switch/<cwd-hash>/<source-session-id>.json` with the cwd,
transcript path, session checkpoint path and open lane IDs, never prompts. In a
Herdr pane it splits the pane and submits the launch command; otherwise it
prints the command. The source session then stops. The launch passes the resume
hooks with `claude --settings`. The new session's `SessionStart` injects the
handoff once, within an hour, for that cwd and account. Read the checkpoint and
its lane IDs before continuing, and collect completed results rather than
dispatching them again.
