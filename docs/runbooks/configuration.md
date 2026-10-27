# Harness configuration
Owner: provenant-config
Anchor: config/harness-knobs.json

Read effective configuration with `provenant config show`. The Rust config
owner reads the selected state root's `config.toml` and validates owner sections.
Use `--state-dir /absolute/path` to select an isolated root. Change the
profile or a knob with `provenant config set profile economy` or
`provenant config set lanes.max_concurrency 4`; `config unset KNOB` reverts it.

Profiles are `balanced`, `economy` and `rigorous`. Override a knob for one run
with `provenant dispatch --knob NAME=VALUE` (the terminal path; agents use
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
allows 64 active lanes and 32 per project, and the memory (5 %) and disk
(500 MiB) floors apply to both classes. `lanes` shows a queued or starting lane's
class and what it is waiting for, such as `read-only startup spacing: 1.2 s
left of 2.0 s`.

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
text output lists each path and byte count. `--dry-run` previews these removals.
`gc.schedule = false` disables scheduled retention sweeps too;
settlement cache pruning still runs.

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
Rerun `install` after changing the idle threshold or floor to update the
agent's arguments. Edit these keys in `config.toml`; they are separate from
the harness knob registry.

The schedule runs at load and hourly, only for installations in the account's
default state root. Logs are `<state>/gc/stdout.log` and
`<state>/gc/stderr.log`. It discovers registered projects without requiring a
checkout as its working directory. Unobservable projects keep their caches.
Manual `provenant gc --dry-run --free-floor 20G` previews the selected
project's cleanup, including oldest-first pressure removals. Live writers and
reservations remain protected.

Writer dispatch and resume refuse to launch when the minimum available space
across the worktree, state and lane-cache volumes is below the floor. The error
names the limiting path, available bytes and the floor and directs you to
`provenant gc`. Read-only dispatch remains available. This is a point-in-time
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
parent traversal are refused. Unknown or non-terminal lanes, dirty worktrees,
recent writes within 24 hours and symlinks prevent pruning. Failures skip
individual entries. Lane caches still use their existing GC path.

After collection, GC checks available space on the state volume against the
floor. `gc.alert_below` can raise the alert threshold and accepts the same
size syntax as `free_floor`. Applied collections emit a Fabric note with
entity `gc.low_disk` and a gc log message once per observed dip below the
threshold. A reading at or above it rearms the alert. The latch persists
under `<state>/gc/`; a preview reports low space without changing it. An alert
failure is logged without blocking collection. This setting is read each run
and does not require reinstalling the agent.

## Disk guard

On macOS, the per-user disk guard checks available space on the state volume
at login and every two minutes. It is enabled by default for installations
in the account's default state root. Configure it in `config.toml`:

```toml
[disk_guard]
enabled = true
warn = "60G"
pause = "25G"
resume = "50G"
processes = [
  "cargo", "rustc", "ld", "ld64", "clang", "clang++", "cc1", "cc1plus",
  "sccache", "swift-frontend", "swift-build", "xcodebuild", "turbo", "esbuild"
]
```

Sizes use the same binary units as `gc.free_floor`; plain numbers mean GiB.
Thresholds must satisfy `0 < pause < resume <= warn`. `processes` replaces the
default list with exact executable names, without paths or whitespace. An
empty list disables pausing while retaining warnings and GC.

Below `warn`, the guard runs `gc --scheduled --apply` using GC's configured
free floor at most once every 30 minutes. It warns at most once every 30 minutes while
space remains at or above `pause`. Below `pause`, it sends `SIGSTOP` to
matching processes owned by the current user and sends a critical notification
at most once every five minutes. Paused process identities are saved under
`<state>/disk-guard/`. They receive `SIGCONT` only when space reaches `resume`;
STOP checks the PID, owner, executable name and UTC start time. CONT checks
the PID, owner and executable name, tolerating changes to saved start-time text.
Records from before the current boot are discarded, and identity mismatches
are logged. The guard excludes itself and its descendants; `provenant` is
rejected as a configured process name.
While its processes remain paused, the checker stays running and checks free
space every two minutes. launchd does not start another instance while it is
running. On recovery, it checks only the recorded PIDs before resuming them.

Rerun `install` after changing any disk guard setting. The installer puts
the settings in the LaunchAgent's arguments, so routine healthy checks need
no config reads. The healthy path performs one `statfs` call on the
state volume and one metadata check for `<state>/disk-guard/paused.json`, then
exits before normal CLI initialisation, without process
enumeration, GC, Fabric, network requests or log writes. State is written only
when it changes.
The paused-state file also acts as a recovery marker. If the checker crashes,
the next scheduled check reads it even when space has recovered to `warn` or
above, and resumes the recorded builds once space reaches `resume`.
Reinstalling restarts the checker and resumes saved processes even when the
settings are unchanged, including records saved before recovery markers existed.
Setting `enabled = false` and reinstalling stops the owned agent and resumes
its saved paused processes. See [installation](install.md#install) for plist
ownership and uninstall behaviour.

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

A shared Cargo target across worktrees is unsupported because it can run another
checkout's stale test binaries; see [writer lanes](providers.md#writer-lanes).

## Routing pools

Routing changes are stored per state root and apply on the next route command.
To change routing, run `provenant route set` or `provenant route pacing`.
Inspect configured weights with `provenant route list --pool NAME`; pacing and
readiness apply at dispatch. Remove a member with
`provenant route remove POOL MODEL`; restore the bundled pool with
`provenant route reset POOL`.

## Quota menu display

The menu bar reads display choices from `quota show --json`. Per account, set
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
If `display.icon` names an unregistered account, `quota show` reports a
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
registered or used as a switch target. Credentials and settings are never copied
between directories.

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
