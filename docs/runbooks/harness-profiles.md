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
when unset, `~/.claude` must itself be explicitly registered. Credentials and
settings are never copied between directories.

```sh
provenant config set chair.switch_accounts '["work-spare","personal-spare"]'
provenant config set chair.switch_threshold 92
provenant config set chair.auto_switch true
provenant chair switch --dry-run
provenant chair switch --to work-spare
```

`chair.auto_switch` defaults to `false`; `chair.switch_accounts` defaults to an
empty list, which permits no targets. A manual switch bypasses the automatic
trigger and enable flag, but still requires an allowed account with headroom.
Use `--session-id <host-session-id>` for a manual handoff of a saved session;
otherwise the command uses `CLAUDE_SESSION_ID` if set, or creates a manual key.
A dry run reports the target, planned checkpoint path and exact launch command
without writing a checkpoint or opening a pane.

The managed `UserPromptSubmit` hook checks cached quota only, never refreshes it.
Either the five-hour or weekly window at or above the threshold triggers a
handoff. The first allowed registered account with both windows below the
threshold wins. Missing, expired, future-dated or more than 15-minute-old quota
cannot establish target headroom. Refresh quota separately with
`provenant quota tick --adapter claude` when needed. Install the managed hooks
through the normal authorised install workflow.

Before launch, the hook saves a private checkpoint under
`<state>/chair-switch/<cwd-hash>/<source-session-id>.json`. It records the cwd,
transcript path, the saved workflow session checkpoint path when present,
open run/task/attempt IDs and the submitted prompt as the next step when
available. The run store remains authoritative for lane state. Refusals are
recorded in the same checkpoint log and hook output without stopping the turn.

Inside a Herdr-managed pane, switching splits the current pane and submits the
launch command to the returned new pane ID. Otherwise, or on a Herdr failure,
it prints the exact command for the user. Pane admission is not proof that the
new session is ready. The old session stops after the checkpoint is prepared;
a repeated prompt cannot launch the same source handoff again. The launch
command supplies the resume hooks with `claude --settings`, so target accounts
need no copied settings. SessionStart injects the newest unconsumed handoff for
this cwd only in the selected account's new session and marks it consumed.
Read the checkpoint and inspect its lane IDs before continuing; collect
completed results rather than dispatching them again.
