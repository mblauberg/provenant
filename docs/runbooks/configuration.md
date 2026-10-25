# Harness configuration

Read effective configuration with `provenant config show`. The Rust config
owner reads the selected state root's `config.toml` and validates owner sections.
Use `--state-dir /absolute/path` to select an isolated root. Change the
profile or a knob with `provenant config set profile economy` or
`provenant config set lanes.max_concurrency 4`; `config unset KNOB` reverts it.

Profiles are `balanced`, `economy` and `rigorous`; task-class presets include
`mechanical` and `bulk`. Inspect effective values with
`provenant config show --task-class mechanical` and override a wired knob for
one run with `provenant dispatch --knob NAME=VALUE`.

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
layer. `config set|unset|show` read and write this file, and `config show
[--task-class NAME]` reports each value and its source. Dispatch accepts
repeated `--knob NAME=VALUE`, and `fabric_submit` accepts `knobs:
["NAME=VALUE"]`; these validated overrides win over configuration and
task-class presets. Each sealed task and attempt records the profile name,
task class, effective knob values and exact dispatch overrides, and
continuations keep that stamp.

Batch concurrency resolves `lanes.max_concurrency` from the base profile, so
task-class values do not apply. The registry default is 8, and an explicit
`--concurrency N` or Fabric `concurrency` wins.

## Knob registry

`config/harness-knobs.json` owns the workflow knob registry and presets.
Fabric's similarly named TOML assets supply only Fabric domain defaults; they
are not a second source for the workflow knob values. The binary embeds the
registry at build time and `config set` validates against it. Skill-read knobs
(review, repair, council, writing and summary policy) take effect when the
skill reads `provenant config show`.

The registry marks these knobs `wired`: `chair.auto_switch`,
`chair.switch_threshold`, `chair.switch_accounts`, `lanes.silence_seconds`,
`lanes.max_concurrency`, `lanes.max_depth`, `review.extra_reviewers`,
`review.cross_family`, `review.rereview_scope` and `review.polish`. The other
eight are advisory and do not alter Rust runtime behaviour. Use `config show`
to see each knob's status and source. Install registrations with
`provenant install --dry-run`
before applying an authorised install.

## Writer extra write paths

Some projects keep shared run state outside the worktree. Writer lanes can
write it once the host owner lists the directories in the state `config.toml`:

```toml
[domains.run]
writer_writes = ["/Users/me/proj/.agent-run/locks", "/Users/me/proj/.build/cargo-practice-wave"]
```

Each entry is an absolute directory (letters, digits and `/._-` only, no `..`,
at most 16), granted read-write as a subtree to worktree-write lanes only.
Read-only lanes are unchanged. The config is refused if an entry is too broad
or names a credential directory such as `~/.ssh`; a launch is refused if an
entry overlaps the state directory's protected parts or Git metadata.
`/usr/bin` is already executable in lanes, so `/usr/bin/lockf` works.

## Routing pools

Routing changes are stored per state root and apply on the next route command.
To change routing, run `provenant route set` or `provenant route pacing`.
Inspect effective weights, pacing factors and readiness with
`provenant route list --pool NAME`. Remove a member with
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
Each directory must already exist and be signed in separately. Labels and
directories must be distinct, with no nested account directories. The current
`CLAUDE_CONFIG_DIR` must match a registration;
when unset, `~/.claude` may be registered as the source only. Targets must use
fixed identities such as `~/.claude-<label>`; the rotating `~/.claude` directory
is refused as a target. Credentials and settings are never copied between directories.

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
A real manual handoff requires `--session-id <host-session-id>` or
`CLAUDE_SESSION_ID`, so retries cannot launch another successor for the same source.
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
