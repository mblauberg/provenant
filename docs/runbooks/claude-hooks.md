# Run native session and worktree hooks
Owner: provenant
Anchor: [`hooks::run`](../../tools/front-door/src/hooks.rs)

`provenant install --harness claude --hooks on` registers these hooks in the
Claude settings (see [install](install.md)). Other harnesses use the native
hook events declared in [`AdapterSpec`](../../tools/front-door/crates/records/src/adapters.rs),
which also records each harness's compaction events and unavailable mappings.
Handler names are the constants in
[`hooks.rs`](../../tools/front-door/crates/cli/src/hooks.rs).

Unrecognised or unusable advisory input abstains with exit 0 and a diagnostic.
Worktree operation errors and
[hard boundary](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md)
checks can refuse with exit 2. Stdout holds only the native protocol response.

| Event | Handler | What it does |
|---|---|---|
| `PostToolUse` | `hook claude post-tool-use` | After the tool has run, optionally offers human answers through `additionalContext` |
| `SessionStart` (`startup`, `resume`, `clear`) | `hook claude session-start` | Reports checkpoint status, then the state digest |
| `SessionStart` (`compact`) | `hook claude compact` | The same report after compaction |
| `PreCompact` | `hook claude pre-compact` | Writes derived state to `autosave.md` and prints the summary instruction below |
| `Stop` | `hook claude stop` | Refreshes `autosave.md` when an existing session has been quiet for 10 minutes |
| `WorktreeCreate` | `hook claude worktree-create` | Runs `provenant worktree create claude/<name>` |
| `WorktreeRemove` | `hook claude worktree-remove` | Runs `provenant worktree remove <name>` |
| chair prompt, start, stop | `hook claude chair-prompt`, `chair-start`, `chair-stop` | Account switching; see [configuration](configuration.md#chair-account-switching) |

Unavailable hook state is a diagnostic and never denies the tool. Answer
delivery deliberately runs on `PostToolUse`: Claude Code treats a failed
`PreToolUse` hook as a refusal of the tool, while a `PostToolUse` failure cannot
undo a call that already ran. `UserPromptSubmit` would block the prompt for the
same reason. Install rewrites the hooks it manages, so an upgrade removes the
old `PreToolUse` entry and registers the `PostToolUse` replacement under the
ownership the receipt already records, with or without `--hooks on`; foreign
hooks on either event are left alone. Hook events
and per-harness decisions live in [`AdapterSpec`](../../tools/front-door/crates/records/src/adapters.rs):
Claude uses `PostToolUse`, Codex keeps its declared `PreToolUse` mapping because
no non-blocking alternative is verified for it, and Cursor, Antigravity and
OpenCode declare their pre-tool answer response unsupported. A hook that fails
inside Provenant still exits 0, but records the cause and elapsed duration under
`<state>/hooks/diagnostics/failures.jsonl`.

The [human inbox](human-inbox.md#delivery-service) owns how the hook offers answers.

## SessionStart output

The hook reports checkpoint and lane state. An unavailable section is not an
empty one: inspect the source before acting. The checkpoint points to authority
but does not create it. Record delivered PRs in the checkpoint so the digest can
find them.

## Autosave and compaction

- A hook never writes the chair's `checkpoint.md`; "settled since the last chair
  save" is measured from its modification time. `pre-compact` and `stop` write
  only `autosave.md`, derived from the run store, `worktree list` and `pr status`
  under the checkpoint lock, and it grants no authority. `pre-compact` creates it
  when no checkpoint exists, and the next SessionStart says chair notes still
  need saving; `stop` never creates either file. Hooks inside a lane report but
  never write.
- `pre-compact` prints one instruction that Claude Code takes as guidance for
  the compaction summary: write an Intent section with each live ruling's wanted
  outcome, reason, rejected alternatives, open disagreement and uncertain
  readings, and quote only authority grants or corrections. The hook only asks;
  without a compliant summary, intent survives only if the chair saved it (see the
  [session skill](../../skills/session/SKILL.md)).

## Claude worktree hooks

Claude Code replaces its own `git worktree` handling with these hooks, so each
prints the new worktree's absolute path and nothing else on stdout.

- `worktree-create` accepts a name of 1 to 64 letters, digits, `.`, `_` and `-`
  (starting alphanumeric) and creates branch `claude/<name>` at
  `<repo>/.worktrees/claude-<name>`. The worktree already holding that branch is
  reused when it is clean and idle. Ignored (declared build) output does not
  block reuse; tracked or untracked changes and a live or held writer still do.
  A clean, idle tree is adopted with a warning even when it is off the hook's
  usual path or has no ownership record under
  `<state>/hooks/claude-worktrees/` or the Git admin directory: those are
  preferences, not boundaries. The primary checkout, a live or held writer
  (`live`/`busy`), uncommitted or untracked changes and an unknown layout stay
  refused with exit 2 and the reason. Invalid event input abstains.
- `worktree-remove` removes a clean, idle linked worktree the repository lists,
  through `provenant worktree remove <name>` without `--discard`. A tree without
  this hook's ownership record is adopted with a warning only once the removal
  is confirmed. Dirty trees, live writers, the primary checkout and unmerged
  branches (`not_merged`) refuse with exit 2 and the worktree stays; run
  `provenant worktree remove` yourself once the cause is cleared. Declared
  regenerable output is reclaimed; any other ignored file, such as an undeclared
  `target/`, keeps the tree.

The fixture tests run with `cargo test -p provenant --test integration hooks_session::`
from `tools/front-door`.
