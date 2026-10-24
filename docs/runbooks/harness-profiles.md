# Harness configuration

Read effective configuration with `provenant config show`. The Rust config
owner reads the selected state root's `config.toml` and validates owner sections.
Use `--state-dir /absolute/path` to select an isolated root. Change the
profile or a knob with `provenant config set profile economy` or
`provenant config set lanes.max_concurrency 4`; `config unset KNOB` reverts it.

The Python profile presets and profile mutation wrapper are retired. Set
route, effort, access and timeout explicitly on dispatch; consult
`provenant help dispatch` for the supported controls. Install registrations
with `provenant install --dry-run` before applying an authorised install.

## Chair account switching

Claude chair switching is opt-in and has no daemon. Register each account with
an explicit `identity_dir` in `accounts.claude` in
`$AGENT_FABRIC_INSTANCE_ROOT/config/telemetry.json` (default
`~/.agents/config/telemetry.json`). Each directory must already exist and be
signed in separately. Labels and directories must be distinct, with no nested
account directories. The current `CLAUDE_CONFIG_DIR` must match a registration;
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
