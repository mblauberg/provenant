# Run native session and worktree hooks
Owner: provenant
Anchor: [`hooks::run`](../../tools/front-door/src/hooks.rs)

`provenant install --harness claude --hooks on` registers these hooks in the
Claude settings (see [install](install.md)). Other harnesses use their native
hook events declared in [`AdapterSpec`](../../tools/front-door/crates/records/src/adapters.rs). Unrecognised or unusable advisory input
abstains with exit 0 and a diagnostic. Worktree operation errors and
[hard boundary](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md) checks can refuse with exit 2. Stdout contains
only the native protocol response.

| Event | Handler | What it does |
|---|---|---|
| `PreToolUse` (`Bash`) | `hook claude pre-tool-use` | Leaves tool policy to confinement, then optionally offers human answers |
| `SessionStart` (`startup`, `resume`, `clear`) | `hook claude session-start` | Reports checkpoint status, then the state digest |
| `SessionStart` (`compact`) | `hook claude compact` | The same report after compaction |
| `PreCompact` | `hook claude pre-compact` | Writes derived state to `autosave.md` and prints the summary instruction below |
| `Stop` | `hook claude stop` | Refreshes `autosave.md` when an existing session is 10 minutes quiet |
| `WorktreeCreate` | `hook claude worktree-create` | Runs `provenant worktree create claude/<name>` |
| `WorktreeRemove` | `hook claude worktree-remove` | Runs `provenant worktree remove <name>` |
| chair prompt, start, stop | `hook claude chair-prompt`, `chair-start`, `chair-stop` | Account switching; see [configuration](configuration.md#chair-account-switching) |

The guard leaves shell and tool permissions to the lane's confinement policy.
Unavailable hook state is diagnostic, and does not deny the tool. The
[human inbox](human-inbox.md#delivery-service) owns how the hook offers answers.

## SessionStart output

The hook reports checkpoint and lane state. An unavailable section is not an
empty one; inspect the source before acting. The checkpoint points to authority
but does not create it. Record delivered PRs in the checkpoint so the digest can
find them.

## Autosave and compaction

- A hook never writes the chair's `checkpoint.md`; "settled since the last chair
  save" is measured from its modification time. `pre-compact` and `stop` write
  only `autosave.md`, derived from the run store, `worktree list` and `pr status`
  under the checkpoint lock. It grants no authority. `pre-compact` creates it
  when no checkpoint exists, and the next SessionStart says chair notes still
  need saving; `stop` never creates either file. Hooks inside a lane report but
  never write.
- `pre-compact` prints one instruction that Claude Code takes as guidance for
  the compaction summary: write an Intent section with each live ruling's wanted
  outcome, reason, rejected alternatives, open disagreement and uncertain
  readings, and quote only authority grants or corrections. The hook only asks;
  without a compliant summary, intent survives only if the chair saved it (see the
  [session skill](../../skills/session/SKILL.md)). Each harness's native
  compaction events and unavailable mappings are recorded in `AdapterSpec`.

## Claude worktree hooks

Claude Code replaces its own `git worktree` handling with these hooks, so each
prints the new worktree's absolute path and nothing else on stdout.

- `worktree-create` accepts a name of 1 to 64 letters, digits, `.`, `_` and `-`
  (starting alphanumeric) and creates branch `claude/<name>` at
  `<repo>/.worktrees/claude-<name>`. The worktree already holding that branch is
  reused when it is clean and idle. A clean, idle tree is adopted with a warning
  even when it is off the hook's usual path or carries no ownership record from
  `<state>/hooks/claude-worktrees/` or the Git admin directory: those are
  preferences, not boundaries. The primary checkout, a live or held writer
  (`live`/`busy`), uncommitted or ignored files and an unknown layout stay
  refused with exit 2 and the reason. Invalid event input abstains without
  creating or removing anything.
- `worktree-remove` removes a clean, idle linked worktree the repository lists,
  through `provenant worktree remove <name>` without `--discard`. A tree without
  this hook's ownership record is adopted with a warning only once the removal
  is confirmed; the warning never claims an adoption that was refused. Dirty
  trees, live writers, the primary checkout and unmerged branches (`not_merged`)
  refuse with exit 2 and the worktree stays; use `provenant worktree remove`
  yourself once the cause is cleared. Declared regenerable output is reclaimed;
  any other ignored file, such as an undeclared `target/`, keeps the tree.
- If a hook fails, inspect and resolve the reported cause.

Check with `provenant hook claude --help`; the fixture tests are
`cargo test -p provenant --test integration hooks_session::` from `tools/front-door`.
