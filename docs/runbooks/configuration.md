# Harness configuration
Owner: provenant-config
Anchor: config/harness-knobs.json

Read effective configuration with `provenant config show`. The Rust config
owner reads the selected state root's `config.toml` and validates owner sections.
Use `--state-dir /absolute/path` to select an isolated root. Change the
profile or a knob with `provenant config set profile economy` or
`provenant config set lanes.max_concurrency 4`; `config unset KNOB` reverts it.

Profiles are `balanced`, `economy` and `rigorous`. Override a knob for one run
with `provenant run --knob NAME=VALUE` (the terminal path; agents use
`fabric_run` (`op: submit`)'s `knobs`).

## Precedence and stamps

The header requires `schema_version = 1`. `profile` selects `balanced`,
`economy` or `rigorous`, and a missing file selects defaults; a malformed or
unsupported file fails. Knob overrides live in a `[knobs]` table:

```toml
schema_version = 1
profile = "economy"
```

A knob in `[knobs]` wins over the profile preset, which wins over the registry
default. There is no project JSON overlay or `PROVENANT_KNOB_*` environment
layer. `config set|unset|show` read and write this file through one editor
that validates the whole result before replacing it, and `config show` reports
each value and its source. Dispatch accepts repeated `--knob NAME=VALUE`, and
`fabric_run` (`op: submit`) accepts `knobs: ["NAME=VALUE"]`; these validated
overrides win over configuration. Each sealed task and attempt records the
profile name, effective knob values and exact dispatch overrides, and
continuations keep that stamp.

Batch concurrency resolves from the profile by batch class. A batch with any
`worktree-write` lane uses `lanes.max_concurrency` (registry default 8); a
batch with none uses `lanes.max_concurrency_read_only` (registry default 16).
A `lanes.max_concurrency` you set yourself (in `[knobs]` or with `--knob`) also
caps the read-only default unless you set `lanes.max_concurrency_read_only`
too; a preset does not. An explicit `--concurrency N` (1 to 16) or Fabric
`concurrency` wins for either. The `economy` preset sets 4 and 8 respectively.

Batch concurrency is separate from host admission. Each lane has a class, and
spacing is measured only against the last start of the same class: writers
wait 20 s after the last writer start, read-only lanes 2 s after the last
read-only start, so a read-only lane never waits behind a writer. Within one
boot no two lanes of a class start less than one spacing apart, however the
wall clock changes; a reboot resets spacing, except that the one-time upgrade
of a version 1 ledger that recorded a start holds each class for one spacing.
Spacing is enforced at admission, not at provider spawn, so a guardian
suspended between the two can spawn alongside a later lane. The host
allows 64 active lanes and 32 per project. On macOS it defers starts at warning
or critical kernel memory pressure; on other platforms the 5 % memory floor
applies. The disk floor applies to both classes. `lanes` shows a queued or starting lane's
class and what it is waiting for, such as `read-only startup spacing: 1.2 s
left of 2.0 s`.

Every worktree-write lane occupies a host build slot until it settles, since
build activity cannot be identified cheaply. The default is one slot on hosts
with 16 GiB or less, and two on larger hosts. Cargo defaults to half the available
cores per lane, at least one. Set host overrides in `config.toml`:

```toml
[domains.fabric.lanes]
max_concurrent_builds = 2
cargo_build_jobs = 5
```

Omit either build setting from `config.toml` to select its host-dependent default.
The host settings are not workflow knobs managed by `provenant config set`;
edit the `[domains.fabric.lanes]` table directly. The value `-1` also selects
automatic behaviour.


Higher values can reduce waiting but increase peak memory and CPU demand.
An existing `CARGO_BUILD_JOBS` in the submitting environment takes precedence;
a brief can also set it inside the lane. Pressure and slots are checked before
lane admission, not continuously during a build.

## Knob registry

`config/harness-knobs.json` owns the workflow knob registry and presets.
Fabric's similarly named TOML assets supply only Fabric domain defaults; they
are not a second source for the workflow knob values. The binary embeds the
registry at build time and `config set` validates against it. The review
knobs take effect when a skill reads `provenant config show`.

The registry holds only knobs that code or a skill reads: `chair.auto_switch`,
`chair.switch_threshold`, `chair.switch_accounts`, `lanes.silence_seconds`,
`lanes.max_concurrency`, `lanes.max_concurrency_read_only`, `lanes.max_depth`,
`review.extra_reviewers`,
`review.cross_family`, `review.rereview_scope`, `review.polish`,
`fabric.accept_handoff` and `usage.project_attribution`. Effort, council size,
repair cycles, summary size, pool spreading, prose polish, context ceilings and
quota pace are chair judgement, described in the
[orchestrate skill](../../skills/orchestrate/SKILL.md). Use `config show` to
see each knob's value and source. Preview install registrations with
`provenant install --source /absolute/path/to/release --harness claude --dry-run`
before applying an authorised install.

## Provider updates

Provider updates are enabled by default and checked on existing housekeeping
events, with at least 24 hours between attempts. Updates run outside lanes
when the provider is idle; lanes keep self-update disabled. Configure each
provider in the same `config.toml`:

```toml
[providers.claude]
update_interval_seconds = 86400
update_timeout_seconds = 300

[providers.codex]
update = { command = ["brew", "upgrade", "codex"] }

[providers.opencode]
update = false
```

`update = false` opts out, `true` selects the detected native updater, and
`update = { command = ["program", "argument"] }` overrides it with an argv
array executed directly without a shell. The array must contain a non-empty
program name. The interval and timeout are positive numbers of seconds.
Supported provider IDs are `claude`, `codex`, `cursor`, `agy` and `opencode`;
providers without a known native update method need a command override.

Quota ticks are the housekeeping events. A tick schedules due work in a
detached, one-shot process and returns without waiting for an updater. The
worker takes exclusive leases without waiting and stops after one completed
native update attempt. An error checking one provider is recorded without
preventing checks of the remaining providers. Updates default to a 300-second
timeout, configurable per provider with `update_timeout_seconds`.
Claude uses `claude update`, OpenCode uses `opencode upgrade`, and Codex uses
Homebrew or npm when its resolved executable identifies that installation.
An unknown installation produces one activity note until its method becomes
known or a command override is configured.

Lanes hold shared leases from provider preparation through completion; updates
need an exclusive lease. Busy providers are excluded from a lane's candidates
without delaying other providers. Only when every candidate is busy does
preparation wait, for up to 30 seconds total, before asking for a retry.
Cancellation interrupts this wait. Nested launches retry missing executable
discovery under the lease using the host's sealed search directories.
Provider children inherit their shared lease: a long-lived background process
keeps it and defers updates until it exits.

The updater inherits the exclusive lease, keeping it held if the worker is
killed. On timeout the worker kills the updater's whole process group. The
lease remains held until the last inheriting process exits. A descendant that
leaves the group with `setsid` or daemonises also keeps the lease until it exits;
the group timeout cannot terminate it. Kernel lock release covers crashes;
stable `.lock` files remain in
`<state>/provider-updates/` to avoid inode replacement races. An interrupted
update is recorded as failed and retried after the configured interval, as are
other failed attempts. Interruption can leave a partially replaced installation;
the next attempt uses the same native updater to repair it. Update failures
are recorded once per interval, with updater output discarded. An independent
attempt record preserves backoff if publishing the result record fails.
Worker failures surface through the activity note and update records only;
the quota tick does not report them. Activity records the old and new versions.
Each lane's `availability.json`
also includes its observed version and the latest update result.

## OpenCode Go monthly allowance floor

OpenCode Go models must have a documented monthly allowance at or above the
configured floor. The default is $60 on the $10 plan; set the floor to zero to
disable this policy:

```toml
[domains.routing.opencode_go]
min_monthly_allowance_usd = 60
```

The floor applies to pool members and explicitly selected Go models, including
uncatalogued exact IDs. A model with an unknown allowance is refused while the
floor is enabled. Choose a model with a documented allowance at or above the
floor, or lower the floor to admit it. The separate OpenCode Zen account is not subject to this
floor. This policy is independent of the quota calibration below.

## OpenCode Go quota calibration

Go usage from this host's ledger is a partial lower bound. Enter dashboard
percentages under the quota domain in the selected state root's `config.toml`:

```toml
[domains.quota.calibration.go]
five_hour = 43
seven_day = 17
thirty_day = 59
expires = "2026-11-04T00:00:00Z"
```

The table key is the OpenCode account key, usually `go`. Each percentage is an
integer from 0 to 100, and expiry is a required timestamp with a UTC offset.
Quota and pacing use the maximum of the raw ledger and each floor until the
expiry instant, even if a ledger window resets sooner. Set the expiry no later
than the end of each included window. The table has one shared expiry, so use
the earliest window end and refresh the floors afterwards. For example, a
five-hour floor entered at 09:00 with a window ending at 13:00 should expire at
13:00, within five hours. Editing or removing the table takes effect on the next read;
floors are never written over the raw ledger cache. Zen does not use Go floors.

DeepSeek V4.1 Flash (junior weight 2, senior 1) and GLM-5.3 Flash (junior 1,
senior 1) are provisional Go pool members. They remain eligible without
calibration and use the local ledger for pacing. Route diagnostics identify the
partial coverage and any uncalibrated windows; adding floors can tighten pacing.
MiMo V2.6 Flash is catalogued for exact selection only. Paid OpenCode pacing
remains `pace`.

Without an observable reset, a calibrated window uses a full remaining window
for conservative pacing. Floors stay binding until expiry even if a recorded
reset passes. Recheck the dashboard and update the floors after a reset or
substantial usage on another device. Peer ledgers are not aggregated.

OpenCode 2.2.0's help, `stats --help`, `api --help` and an isolated
`stats --standalone --json` were checked on 28 October 2026 without reading
credentials. Stats contains session counts, tokens and costs, with no account
allowance percentages. The [CLI docs](https://opencode.ai/docs/cli/#stats)
describe session statistics, and the [server API docs](https://opencode.ai/docs/server/)
describe the local server interface. No documented authoritative account-wide
quota source was found. The [Go docs](https://opencode.ai/docs/go/)
describe the allowance windows and per-response consumption; those do not
establish total usage across devices.

## Lane cache collection

GC also applies [terminal provider-state retention](gc.md). The separate
`[retention] provider_state_days = 7` key accepts 1 to 3650 and is reread on
each sweep without reinstalling. `[retention] session_revisions = 8` (1 to 64)
sets how many [checkpoint revisions](session-checkpoints.md#earlier-saves) a
session keeps and is reread on each save. Known credential entries expire after
24 hours. GC also reclaims `tmp/` directories from indexed terminal attempts
after 24 hours, across the state run store and its child stores. Nonterminal
attempts, writer locks and retained reservations prevent deletion. GC JSON
reports the paths and observed allocated bytes in `provider_state.attempt_tmp`;
default output shows aggregate counts and exceptions; `--json` retains each path and byte count. `--dry-run` previews these removals.
`retention.logs_days = 30` (range 1 to 3650) expires stdout and stderr
captures larger than 64 KiB, and intermediate event log rows, from runs whose
attempts are all terminal.

Checkpoint saves warn above 2048 bytes. Override the soft budget for a project
with `[retention.projects."PROJECT_ID"] checkpoint_soft_bytes = 4096` in the
state config; the range is 1 to 262144 bytes. This is advisory and does not
change the hard checkpoint size limit. A save also checks up to the first eight
distinct issue and PR references for closed state; unavailable GitHub lookups
are ignored.

Chair log retention is per session: `[retention] chair_log_days = 30` (1 to
3650), `chair_log_entries = 10000` (1 to 100000), and `chair_log_bytes =
16777216` (65536 to 1073741824). Appends apply all three limits; listing
shows the retained view, and full `gc` removes excess payloads. The optional
`chair_log_replay_days` (7 to 3650) controls receipt retention and defaults to
the larger of `chair_log_days` and 7 days. See the
[chair log](session-checkpoints.md#chair-log) for replay and pruning behaviour.
The last 64 KiB survives as `stdout.log.tail` or `stderr.log.tail`;
results, exit evidence, recorded digests and small captures survive. The same
rule applies inside child stores, whose databases and retained evidence stay.
GC reports `provider_state.attempt_logs`; disk reports measure `attempt_logs`.
`gc.schedule = false` disables scheduled retention sweeps and admission
collection; settlement pressure collection and landing cleanup still run.

The optional `[gc]` section controls the macOS per-user schedule:

```toml
[gc]
schedule = true
idle_days = 7
# free_floor = "100G"
# alert_below = "150G"

# Use the project ID from `provenant fabric identity`.
[gc.projects."PROJECT_ID"]
regenerable = ["node_modules", "target"]
keep = ["web/target"]
foreign_worktrees = "keep" # opt in with "merged-clean"
```

`gc.schedule` defaults to true. Setting it to false prevents subsequent
scheduled collections; rerun `install` to boot out the agent and remove its
unchanged receipt-owned plist. `gc.idle_days` accepts 1 to 3650 (default 7).
`gc.free_floor` defaults to 10% of the state volume's capacity, with a minimum
of 50 GiB. Set it to override that floor. GC requests additional cache
collection until available disk space reaches the floor, after the usual
orphaned, tombstoned and idle pass. Sizes are whole binary units: `B`, `K`,
`M`, `G`, `T`, or `KiB` through `TiB`; plain numbers mean GiB.
On APFS, local snapshots can keep deletions from increasing available space, so
a floor may remove every eligible cache on each run.
Rerun `install` after changing the idle threshold to update the
agent's arguments. The floor is read from configuration each collection. Edit these keys in `config.toml`; they are separate from
the harness knob registry.

The schedule runs at load and daily, only for installations in the account's
default state root. Logs are `<state>/gc/stdout.log` and
`<state>/gc/stderr.log`. It discovers registered projects without requiring a
checkout as its working directory. Unobservable projects keep their caches.
Manual `provenant gc --dry-run --free-floor 20G` previews the selected
project's cleanup, including oldest-first pressure removals. Live writers and
reservations remain protected.

Writer dispatch and resume check available space across the worktree, state
and lane-cache volumes. Below the floor, admission makes one collection
attempt, capped at five seconds, and samples all three volumes again. The
pressure collection skips worktree deletion and network fetches and shares
GC's collection lock. If space remains low, the refusal names the limiting
path, available bytes, floor and bytes reported reclaimed. Interrupted
collection can reclaim space without returning a byte report, so zero reported
bytes does not prove no removal occurred. Run `provenant gc --apply` for a full sweep. Read-only dispatch has the same
admission floor. This is a point-in-time
check with no space reservation: concurrent writers can each pass and then
consume the same free space.

Bare `provenant gc` deletes eligible caches and previews merged worktree and
declared output cleanup. Pass `--apply` to delete those entries too, or
`--dry-run` to preview all cleanup. The installed scheduled agent passes
`--apply`. Merged worktree cleanup fetches `origin`'s actual default branch;
it does not need a fresh local `main`. Failed fetches keep worktrees and
report the failure. Worktrees need known-terminal lanes and merged commits
of their own: tips equal to upstream or on its first-parent history stay.

`gc.projects` is keyed by the registered Fabric project ID. Each project's
`regenerable` list contains directory names, matched at any depth in its
registered linked worktrees. The default is an empty list, which prunes
nothing. `keep` contains optional paths relative to the worktree; those paths,
their descendants and any matching directory containing them are preserved.
Tracked paths are also preserved. `.git`, `.worktrees`, absolute names and
parent traversal are refused. Declared output must be Git-ignored. An idle
registered linked worktree can qualify without a terminal lane when no lane
or writer lease is live. The minimum idle age is `--idle-days`, or one day
below the free-space floor. The primary checkout and unknown writers remain
protected. JSON reports this class separately as `worktree_cleanup[].idle_output`.
Failures skip individual entries. Lane caches still use their existing GC path.

`foreign_worktrees = "merged-clean"` opts this project into removing linked
worktrees that otherwise report `unknown_writer`. GC requires fetched-default
ancestry, a clean tree after only declared ignored output is removed, no live
lanes or reservations, a free existing writer lock where present, a complete
free-process census and the full `--idle-days` age, including below the floor. Missing process observations
refuse removal. It verifies Git registration and filesystem identity again
under repository custody. This policy also covers Git-registered worktrees
outside `.worktrees/`; it preserves their local branch refs. The default
`"keep"` retains the existing terminal-lane requirement.

Scheduled GC excludes missing Git roots, non-Git workspaces and known staging,
probe, run and scratch roots before collecting project IDs for disk alerts.
Old identity records stay available for indexed run evidence and keep their
existing IDs. New transient
and non-Git invocation roots receive a deterministic in-memory identity
without a persistent registration. `unobserved_projects` in GC JSON is an
array of paths whose project custody could not be observed. The separate
`skipped_transient` array lists excluded transient common directories.

After collection, GC checks available space on the state volume against the
floor. `gc.alert_below` can raise the alert threshold and accepts the same
size syntax as `free_floor`. Applied collections emit a Fabric note with
entity `gc.low_disk` and a gc log message once per observed dip below the
threshold. A reading at or above it rearms the alert. The latch persists
under `<state>/gc/`; a preview reports low space without changing it. An alert
failure is logged without blocking collection. This setting is read each run
and does not require reinstalling the agent.

## Disk pressure

`gc.free_floor` is the floor for dispatch admission, running build pauses and
settlement pressure collection. Its default is max(10% of the state volume,
50 GiB). Below it, each active lane's supervisor pauses build processes proven
to belong to that lane. It resumes them at the same floor, and before lane
cleanup. The provider itself, other users' builds and nested lane owners are
excluded. Each nested lane supervises its own builds.

Checks run once per second only while the provider runs, using the supervisor's
existing process census. No disk checker runs while the host has no lanes.
A failed observation preserves the last pause decision. The running lane's
status names a disk pause and the command `provenant gc --apply`.

The `[disk_guard]` table and its separate warning, pause, resume and process
settings have been removed. Remove that table from existing configuration.
Install retires the old LaunchAgent once; the daily GC agent remains a backstop.

## Writer extra write paths

Some projects keep shared run state outside the worktree. Writer lanes can
write it once the host owner lists the directories in the state `config.toml`:

```toml
[domains.run]
writer_writes = ["/Users/me/proj/.agent-run/locks", "/Users/me/proj/.worktrees/feature/target"]
```

Each entry is an absolute directory (letters, digits and `/._-` only, no `..`,
at most 16), granted read, write and exec as a subtree to worktree-write
lanes only, so build scripts under it run. This is the grant for Fabric,
manifest, nested and `--host` dispatch, which cannot pass `--write-root`; see
[providers](providers.md#writer-lanes).
Read-only lanes are unchanged. The config is refused if an entry is too broad
or names a credential directory such as `~/.ssh`; a launch is refused if an
entry overlaps the state directory's protected parts or Git metadata.
`/usr/bin` is already executable in lanes, so `/usr/bin/lockf` works.

Fabric supplies private Cargo paths to each lane; see
[writer lanes](providers.md#writer-lanes). Native unconfined agents may share a
per-host target outside worktrees, taking care to run the intended checkout's
test binaries. The optional host Cargo registry seed is configured by hand in
`config.toml`, not through `config set`; see [worktrees](../worktrees.md).

## Routing pools

Routing changes are stored per state root and apply on the next route command.
To change routing, run `provenant route set` or `provenant route pacing`.
Inspect configured weights with `provenant route list --pool NAME`; pacing and
readiness apply at dispatch. Remove a member with
`provenant route remove POOL MODEL`; restore the bundled pool with
`provenant route reset POOL`.

## Quota menu display

The menu bar reads display choices from `quota --json`. Per account, set
them with `provenant accounts set <account> [--display-name NAME] [--hidden
true|false] [--order N] [--headline auto|5h|weekly|default]`. `<account>` is an
entry id such as `claude-work` or a label unique across adapters. An empty
`--display-name`, `--order 0` and `--headline default` clear the field. Stored
as `display_name`, `hidden`, `order` (1 to 999) and `headline` on
`[domains.accounts.entries.<id>]`.

Globally, `provenant quota display [--headline auto|5h|weekly] [--icon
most_constrained|<account>]` writes `[domains.quota.display]` and returns the
quota report. Reports carry a top-level `display` block and a per-row `display`
block (`id`, `name`, `hidden`, `order`, `headline`). Hidden accounts and order
are not applied by the CLI; the app does that.
If `display.icon` names an unregistered account, `quota` reports a
`display_icon_warning` field in JSON and one warning line in human output.
The report uses `most_constrained` for the icon without changing config.

## Chair account switching

Claude chair switching is opt-in and has no daemon. Register each account with
`provenant accounts register <label> --adapter claude --directory <dir>`.
Registrations live in the selected state directory's `config.toml`, alongside
optional quota metadata such as `--plan`, `--priority` and `--billing-anchor`.
Each Claude account directory must be exactly `~/.claude-<label>`, already exist
and be signed in separately. Labels and directories must be distinct, with no
nested account directories. The current `CLAUDE_CONFIG_DIR` must match a
registration. When unset, `~/.claude` is the sign-in source only; it cannot be
registered or used as a switch target. Credentials and identity are never
copied between directories. Provenant's own hooks, skills and MCP entry, and
the allow-listed shared settings below, are kept identical in every registered
directory by `install`, `accounts register` and `accounts sync` (see the
[install runbook](install.md#account-sync)).

Shared Claude settings are configured under `[domains.accounts]`:

```toml
[domains.accounts.shared_settings]
source = "/Users/me/.claude/settings.json"   # default: ~/.claude/settings.json when it exists
keys = ["permissions", "env", "statusLine", "enabledPlugins",
        "outputStyle", "includeCoAuthoredBy", "cleanupPeriodDays"]   # the default allow-list

[domains.accounts.entries.claude-work.settings]   # wins over the source for this account
outputStyle = "verbose"
```

`source` is a JSON file; with no source file and no per-account `settings`,
nothing is synced. Only keys in `keys` are synced, and a per-account `settings`
key must be in `keys`. Identity and credential keys (`apiKeyHelper`,
`awsAuthRefresh`, `awsCredentialExport`, `forceLoginMethod`,
`forceLoginOrgUUID`, `otelHeadersHelper`) and `hooks` are refused even when
listed. So is any key, or variable name inside `env`, that contains an `auth`,
`credential`, `token`, `oauth`, `secret` or `password` word, an `api key`,
`access key` or `private key` pair, or, after lower-casing and dropping
separators and digits, any of `token`, `secret`, `password`, `passwd`,
`credential`, `oauth`, `apikey`, `authtoken`, `privatekey`, `session`,
`cookie` or `bearer`. The substring rule also catches glued names such as
`ANTHROPICAUTHTOKEN`, `token2` and `oauth2`, and accepts some false positives;
the one exemption is the default key `includeCoAuthoredBy`. The message names
the key and the rule that matched. A refused configuration stops install,
upgrade and sync, never uninstall.

```sh
provenant config set chair.switch_accounts '["work-spare","personal-spare"]'
provenant config set chair.switch_threshold 92
provenant config set chair.auto_switch true
provenant chair switch --dry-run
provenant chair switch --to work-spare --session-id <host-session-id>
```

`chair.auto_switch` defaults to `false`; `chair.switch_accounts` defaults to an
empty list, which permits no targets. A manual switch bypasses the automatic
trigger and enable flag, but still requires an allowed account with headroom.
A real manual handoff requires `--session-id <host-session-id>` or an environment
session ID: `CLAUDE_CODE_SESSION_ID` first, then `CLAUDE_SESSION_ID`. The flag
takes precedence. `workflow session handoff` uses the same order. This keeps
retries from launching another successor for the same source.
A dry run reports the target, planned checkpoint path and exact launch command
without writing a checkpoint or opening a pane.

The managed `UserPromptSubmit` hook checks cached quota only, never refreshes it.
Either the five-hour or weekly window at or above the threshold triggers a
handoff. The first allowed registered account with both windows below the
threshold wins. Missing, expired, future-dated or more than 15-minute-old quota
cannot establish target headroom. Unknown or stale current quota leaves the
source running without a checkpoint or hook warning; diagnostics go to stderr. A failed refresh also invalidates retained
headroom until a successful refresh. Refresh quota separately with
`provenant quota tick --adapter claude` when needed. Install the managed hooks
through the normal authorised install workflow.

Before launch, the hook saves a private checkpoint under
`<state>/chair-switch/<cwd-hash>/<source-session-id>.json`. It records the cwd,
transcript path, the saved workflow session checkpoint path when present,
open run/task/attempt IDs and creation time. Raw submitted prompts are never
persisted. The run store remains authoritative for lane state. When the current
account is over threshold and no target qualifies, one refusal per source session
is recorded in the checkpoint log and hook output without stopping the turn.

Inside a Herdr-managed pane, switching splits the current pane and submits the
launch command to the returned new pane ID. Otherwise, or on a Herdr failure,
it prints the exact command for the user. Pane admission is not proof that the
new session is ready. The old session stops after the checkpoint is prepared;
a repeated prompt is advisory and cannot launch the same source handoff again.
Disabling `chair.auto_switch` always passes prompts through. The advisory retains
an exact recovery command for use after checking that no successor is running.
Hooks check their deadline between steps and skip snapshot or launch with less
than three seconds remaining, leaving the source running.
Incomplete checkpoint publication can be retried. The launch
command supplies the resume hooks with `claude --settings`, so target accounts
need no copied settings. If target settings already install these hooks, they
may run twice; the prepared and consumed markers make repeats idempotent.
SessionStart injects an unconsumed handoff within one hour for this cwd
only in the selected account's new session and marks it consumed. Another source
cannot replace an unconsumed handoff within that hour. Any new session in the
selected account and cwd can consume it, so inspect the successor before starting
another session there.
Read the checkpoint and inspect its lane IDs before continuing; collect
completed results rather than dispatching them again.
