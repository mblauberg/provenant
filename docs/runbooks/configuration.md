# Harness configuration
Owner: provenant-config
Anchor: config/harness-knobs.json

Read effective configuration with `provenant config show`. The Rust config
owner reads the selected state root's `config.toml` and validates owner sections.
Run `provenant config check` to list every warning with its file and key.
Use `--state-dir /absolute/path` to select an isolated root. Change the
profile or a knob with `provenant config set profile economy` or
`provenant config set lanes.max_concurrency 4`; `config unset KNOB` reverts it.

Profiles are `balanced`, `economy` and `rigorous`. Override a knob for one run
with `provenant run --knob NAME=VALUE` (the terminal path; agents use
`fabric_run` (`op: submit`)'s `knobs`).

## Precedence and stamps

The header requires `schema_version = 1`. `profile` selects `balanced`,
`economy` or `rigorous`, and a missing file selects defaults. Unknown keys
and invalid optional values warn and use defaults; invalid optional rows are
isolated so valid rows still apply. Effective values retain their source in
`config show`. Each warning names the offending key, so an invalid value
inside a table drops only that value and its valid siblings still apply.
Syntax, file size and schema-version errors remain hard failures, as do
invalid known credentials, authority, confinement and Git-boundary settings.
Recovery never widens an enforced restriction, so these also refuse the file:

- knobs marked `strict` in the registry (`lanes.publish`,
  `fabric.accept_handoff`, `chair.auto_switch`, `chair.switch_accounts` and
  `lanes.max_depth`), whose defaults could grant what the user withheld;
- enforced model policy (`lane.read_only`, `lane.max_context`,
  `lane.max_turns`), and a malformed rule that carries one;
- Fabric protocol caps (`limits.*`, which may only be lowered), graph limits
  and budgets, `graph.allowed_read_roots`, `home_hosts` and executable
  overrides;
- quota probe and history ceilings.

Bundled assets remain strict. Knob overrides live in a `[knobs]` table:

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
caps writer lanes at `max_writer_lanes` (default 40) and at
`max_project_writer_lanes` per project (default 30), inside fixed ceilings of
64 active lanes per host and 32 per project. A writer at a cap waits until the
admission deadline and is then refused with a reason that names the cap, its
scope and the key to raise. On macOS it defers starts at critical kernel
memory pressure by default. Set `domains.fabric.lanes.memory_pressure_threshold`
to `warning` to defer at warning and critical, or `never` to disable this gate.
The 5 % memory floor applies on every platform and the disk floor
applies to both classes. `lanes` shows a queued or starting lane's
class and what it is waiting for, such as `read-only startup spacing: 1.2 s
left of 2.0 s`.

Build slots gate builds, not lanes. A writer is admitted on the writer caps
and the memory and disk floors alone, however many writers are live, so one
project's idle writers never block another's. The slots are a build throttle,
not a hard cap and not a security boundary: they share CPU and memory fairly
among our own lanes. `max_concurrent_builds` is the number of cargo builds
that run at once on the host. The default is
`max(1, min(cores / cargo_build_jobs, GiB of RAM / 6))`, so a 10-core 16 GiB
host with the default 5 jobs gets 2. A writer lane gets a `cargo` shim first on
its PATH only when its environment sets `CARGO_TARGET_DIR` and
`~/.cargo/bin/cargo` exists; other lanes run cargo ungated. A cargo command that compiles (`build`,
`check`, `clippy`, `test`, `run`, `bench`, `doc`, `rustc`, `rustdoc`, `fix`,
`install`, `package`, `publish`, `nextest`, `llvm-cov` and `miri`, and their
short aliases) takes one slot before the real cargo starts. The shim process
holds the slot itself, runs cargo as its child and releases the slot when that
child exits, so `cargo run` and `cargo test` hold the slot for the whole of the
program or test run, not only the compile. Other commands, such as `fmt` and
`metadata`, take none. A cargo started inside a gated build skips the gate only
when the `PROVENANT_BUILD_SLOT` marker, `pid:start` (the holder shim's pid and
native process start time), names a live ancestor of the nested shim that began
at that time and is a provenant process. A marker that names another process,
even a live ancestor such as the shell above the shim, a reused pid or a value a
lane invents, is ignored and the command acquires normally. A nested cargo that
a build starts with a scrubbed environment loses the marker, so it queues behind
the slot its own parent holds and waits until it times out; keep
`PROVENANT_BUILD_SLOT` in a build's environment when it runs cargo itself.

A command that finds every slot taken waits, prints
`provenant: waiting for a build slot (n of m in use)` on its own stderr, and
repeats the message about every 60 seconds. While it waits it also writes
`build-slot-wait` in the lane's private temporary directory, and `lanes` and
`status` show the lane as waiting (`build slot: ...`) the way they show an
admission wait. The host trusts only which of two fixed waits the file names
and that it was refreshed in the last 150 seconds. It gives up after
`build_wait_minutes` (default 30) with an error that names the slot count and
suggests `provenant lanes` to find the holders. The same bounded wait and
progress message apply while the command waits for another build's
`.cargo-lock` on its target directory, which it does before it takes a slot so
a slot is never held by a build that Cargo would queue anyway. Both waits share
one deadline: together they take at most `build_wait_minutes`. A shim that
receives SIGTERM, SIGINT or SIGHUP passes the signal to its cargo child, and on
Linux the child is also signalled if the shim is killed outright, so a build
does not keep compiling after the shim and its slot are gone. Provenant repairs
the slot directory when it launches a lane: it restores the 0700 and 0600 modes
and recreates a missing, non-regular or hard-linked slot file. Waiting blocks
that command only, not the lane or its admission. `lanes` does not report
build waits; the message is in the lane's output.

The slots are files `slot-0.lock` to `slot-N-1.lock` under
`<state>/build-slots/`, shared by every project on the same state directory and
outside `fabric/locks`, where a lane creates its landing locks. Provenant
creates them outside the sandbox when it launches a lane and refuses a slot
directory another user owns. A lane has read access to that directory and its
files and no write access, so it cannot create, unlink or replace a slot file;
the shim opens a slot file read-only and takes `flock` on it. The descriptor is close-on-exec, so a
daemon or an orphaned grandchild (sccache, a backgrounded test process) never
keeps a slot after the build exits. The kernel releases the slot when the shim
exits, even after a crash or `kill -9`. The slot directory, count and wait
come from the environment written into the shim script by the host, not from
arguments the lane chooses.

What this does not do, by design: a build that runs `~/.cargo/bin/cargo`,
`rustc` or a non-cargo build tool directly is not gated, and a lane can hold a
slot by locking a slot file on purpose. Gating non-cargo builds is out of
scope. If the slot directory cannot be opened or locked for an unexpected
reason, the build runs without a slot and prints a single warning: fair
sharing fails open. A slot that stays busy for the whole wait is an error, not
an ungated build.

`cargo_build_jobs` is the separate per-build parallelism: Cargo defaults to
half the available cores, at least one. Set host overrides in `config.toml`:

```toml
[domains.fabric.lanes]
max_concurrent_builds = 2
cargo_build_jobs = 5
build_wait_minutes = 30
max_writer_lanes = 40
max_project_writer_lanes = 30
```

Omit a key from `config.toml` to select its default. The host settings are
not workflow knobs managed by `provenant config set`; edit the
`[domains.fabric.lanes]` table directly. The value `-1` also selects automatic
behaviour. `max_concurrent_builds` is 1 to 64, `cargo_build_jobs` 1 to 1024,
`build_wait_minutes` 1 to 1440, `max_writer_lanes` 1 to 64 and
`max_project_writer_lanes` 1 to 32 and, when set, at most `max_writer_lanes`
(left unset it follows the host cap down, as the smaller of 30 and the host
cap). An
out-of-range value is rejected when configuration loads, so it never blocks
lane admission. A lane keeps the slot count and wait it was launched with; a
change applies to lanes launched afterwards.

Higher values can reduce waiting but increase peak memory and CPU demand.
An existing `CARGO_BUILD_JOBS` in the submitting environment takes precedence;
a brief can also set it inside the lane. Memory pressure and the disk floor are
checked before lane admission, not continuously during a build; the memory
floor remains the guard against running out of memory.

### Lane build output

Lane builds are set up to stay small. Every lane gets `CARGO_INCREMENTAL=0`
and `CARGO_PROFILE_DEV_DEBUG=line-tables-only`, so a lane's target holds no
incremental sessions and carries line tables instead of full debug information.
On `provenant-core` this measured about 65 MB against 76 to 85 MB with
incremental compilation on, and the size did not grow on rebuild. The repository's
`Cargo.toml` is unchanged. Override either default in `[domains.fabric.lanes]`:

```toml
[domains.fabric.lanes]
cargo_incremental = true       # CARGO_INCREMENTAL=1
cargo_debug = "full"           # CARGO_PROFILE_DEV_DEBUG
build_dir_max_bytes = 21474836480
build_dir_grace_seconds = 900
build_dir_survey_interval_seconds = 300
```

`cargo_debug` takes any Cargo `debug` level: `none`, `line-directives-only`,
`line-tables-only`, `limited`, `full`, `0`, `1`, `2`, `true` or `false`. A
value already in the lane environment takes precedence over both settings.

Fabric also collects idle build output. A Cargo profile directory in a lane
cache (`target/debug`, `target/<triple>/release`, or the same under
`target-lint/`) is idle when a non-blocking exclusive `flock` succeeds on all of
its `.cargo-lock`, `.cargo-build-lock` and `.cargo-artifact-lock`. Cargo holds
all three while it builds, so a directory with any of them held is never
removed. Idle directories go least recently used first, judged by the newest
modification time below each, until the allocated bytes of all profile
directories, idle and busy alike, are within `build_dir_max_bytes` (20 GiB by
default) and free space on the cache volume is at least `gc.free_floor`. When
busy directories alone exceed the cap, the sweep removes every eligible idle
directory and the total stays over. The lock files are removed last. Space
outside profile directories (`target/doc`, Cargo homes, other lane files) is
not counted or collected here.

Free locks do not prove a directory is unused: test and run binaries keep
executing after Cargo drops its locks, and Cargo can open a lock file just
before it takes the lock. The grace does not cover Cargo opening an existing
lock file without writing anything; the `flock` probe covers that case. A directory whose newest modification is within
`build_dir_grace_seconds` (15 minutes by default) is therefore never removed,
and neither is one written after the sweep measured it. These directories still
count toward the cap and are reported as `recent`. Set the grace to 0 to turn
the protection off. The grace protects only recently written directories: a test or run binary that runs longer than the grace without writing under `target/` is unprotected unless a Cargo lock is still held.

Collection runs on three events:

- when a lane settles;
- when lane admission finds free space below `gc.free_floor`, at most about
  once a minute while the lane waits;
- in `provenant gc`, including the scheduled sweep, `--pressure` and
  `--dry-run`, which previews without taking the lock.

The walk that measures the caches takes no lock, and a sweep that finds the cap
and the floor already met stops there. Only removal holds the host admission
lock, so no lane is admitted mid-removal; it probes each candidate again under
that lock. A settling lane waits at most 5 seconds for it.

Settlement does not walk the caches every time. It walks only when free space is
below `gc.free_floor`, when the last walk is older than
`build_dir_survey_interval_seconds` (5 minutes by default), or when that walk
found the total within 10% of the cap. The last result is kept in the state
root, and a settlement that finds another walk running skips. `provenant gc`
always walks. While free space stays below the floor, each settlement walks,
because reclaiming is the point.

`provenant gc` reports the directories removed or reclaimable, the bytes
reclaimed, how many bytes of lane build output remain and how many directories
were busy or recent. The JSON form has the same fields under `build_dirs`
(`removed`, `bytes`, `cache_bytes`, `busy`, `recent`, `errors`); the summary form has
`build_dirs_removed`, `build_dir_bytes_reclaimed` and `build_dir_bytes`.
A directory that Cargo is building in is skipped and counted as busy; a later
event collects it once Cargo has finished. A collection skipped because another
sweep or an admission holds the host ledger is reported as an alert.

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
The worker starts in the quota event's working directory, which need not be a
Git checkout, and quota events launch it at most once every 15 minutes whatever
its outcome. `<state>/provider-updates/worker.json` holds the last launch time
and, if that run failed, its one cause. Worker failures surface through the
activity note, that record and the update records only; the quota tick does not
report them. Activity records the old and new versions.
Each lane's `availability.json`
also includes its observed version and the latest update result.

## Lane runtime deadline

A lane's wall-clock allowance is its dispatch `--provider-timeout` (3600
seconds, or 10800 for `worktree-write`; at most 14400). Cap it per provider with
`runtime_deadline_seconds`; the shorter of the two applies:

```toml
[providers.codex]
runtime_deadline_seconds = 7200
```

The deadline runs on the wall clock, so a host that sleeps through it stops the
provider on waking. The attempt settles as `timed_out`, releases its worktree
and lock, and `lanes` shows the deadline of each running lane and a
`provenant resume` hint for a timed-out one.

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

The optional `[gc]` section controls daily collection by the host tick:

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
scheduled collections. The host tick continues to serve graph deadlines.
`gc.idle_days` accepts 1 to 3650 (default 7).
`gc.free_floor` defaults to 10% of the state volume's capacity, with a minimum
of 50 GiB. Set it to override that floor. GC requests additional cache
collection until available disk space reaches the floor, after the usual
orphaned, tombstoned and idle pass. Sizes are whole binary units: `B`, `K`,
`M`, `G`, `T`, or `KiB` through `TiB`; plain numbers mean GiB.
On APFS, local snapshots can keep deletions from increasing available space, so
a floor may remove every eligible cache on each run.
The idle threshold and floor are read from configuration each collection.
Edit these keys in `config.toml`; they are separate from
the harness knob registry.

The macOS host tick runs at load and every 60 seconds, only for installations
in the account's default state root. Collection runs when its persisted daily
deadline is due. Logs are `<state>/tick/stdout.log` and `<state>/tick/stderr.log`.
Collection runs in a detached one-shot worker, with its latest result in
`<state>/tick/gc-result.json`, so a slow collection does not hold up graph wakes.
It discovers registered projects without requiring a checkout as its working
directory. Unobservable projects keep their caches.
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
`--dry-run` to preview all cleanup. The daily tick task applies those removals. Merged worktree cleanup fetches `origin`'s actual default branch;
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
Install retires the old LaunchAgent once; the daily GC tick task remains a backstop.

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

## Writer review-exempt paths

A writer whose commits change a protected path ends `review_required` until
the chair accepts it. Paths every implementation legitimately changes should
not cost that stop, so the host lists them:

```toml
[domains.run]
review_exempt = ["docs/release-notes/next.d/", "docs/specs/", "Cargo.lock", ".tracker-input"]

[domains.run.projects."PROJECT_ID"]
review_exempt = ["docs/release-notes/next.d/", "CHANGELOG.md"]
```

The values above are the defaults (the first list). An entry is a
repository-relative path with no wildcards: `dir/` names a directory at the
repository root, a bare name such as `Cargo.lock` matches a file of that name
at any depth, and anything else is one exact path. Names compare without ASCII
case. A project's own list replaces the host-wide one for that project, and
`review_exempt = []` turns the exemption off. At most 64 entries of 256 bytes.

A listed path never makes a writer `review_required`: a committed change to it
is not protected evidence, and an unmerged or renamed entry that names only
listed paths does not force review. A listed path left uncommitted does not
make a lane that committed nothing end `uncommitted`. Git metadata and
attributes, hook directories, provider control roots (`.claude`, `.codex`,
`.mcp.json` and the rest) and CI workflows under `.github/` can never be
listed, whatever the config says. Instruction documents (`AGENTS.md`,
`CLAUDE.md`, `GEMINI.md`, `HARNESS.md`) and the `.agents/` tree can be listed
and are not on by default. A writer reads the list when it launches and again
when it settles. If `config.toml` cannot be read at settlement, nothing is
exempt.

`provenant status` ends with the list that applies to the current project, and
`status --json` carries it as `review_exempt`.

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
