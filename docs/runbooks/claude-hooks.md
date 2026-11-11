# Run the Claude Code session and worktree hooks
Owner: provenant
Anchor: [`hooks_session.rs`](../../tools/front-door/tests/hooks_session.rs)

`provenant install --harness claude --hooks on` registers these hooks in the
Claude settings (see [install](install.md)); rerun it after an upgrade to
register new handlers. Every handler exits 0 on its own failure, except the
worktree hooks and the guard's exit-2 denial, and prints only the protocol
response on stdout.

| Event | Handler | What it does |
|---|---|---|
| `PreToolUse` (`Bash`) | `hook claude pre-tool-use` | Enforces the guard, then optionally offers human answers |
| `SessionStart` (`startup`, `resume`, `clear`) | `hook claude session-start` | Reports checkpoint status, then the state digest |
| `SessionStart` (`compact`) | `hook claude compact` | The same report after compaction |
| `PreCompact` | `hook claude pre-compact` | Writes derived state to `autosave.md` and prints the summary instruction below |
| `Stop` | `hook claude stop` | Refreshes `autosave.md` when an existing session is 10 minutes quiet |
| `WorktreeCreate` | `hook claude worktree-create` | Runs `provenant worktree create claude/<name>` |
| `WorktreeRemove` | `hook claude worktree-remove` | Runs `provenant worktree remove <name>` |
| chair prompt, start, stop | `hook claude chair-prompt`, `chair-start`, `chair-stop` | Account switching; see [configuration](configuration.md#chair-account-switching) |

If every Bash call is denied with `guard policy unavailable`, policy root
resolution failed: from a terminal outside Claude, check that `HOME` resolves
and that `PROVENANT_STATE_DIR` or the hook's `--state-dir` names the intended
state root. The [human inbox](human-inbox.md#delivery-service) owns how the hook
offers answers.

## SessionStart output

Read-only, under 8 KiB in total (the checkpoint excerpt shrinks first), with a
4-second budget; a section that could not be read is listed as unavailable,
never empty. In order:

- `## Standing`: the checkpoint's typed `provenant_standing` record (goal,
  grants, open obligations, what waits on the owner). A grant with an
  `owner:<ref>` or `rule:<ref>` source is listed active; without one, or with an
  unreadable expiry, unverified; an expired one is counted only. It points at
  authority and never creates any. After an account switch `chair-start` appends
  the source session's standing section to its handoff.
- `## Intent`: the checkpoint's intent section (3 KiB cap, shared with standing),
  plus the host transcript path for exact words. No intent section gets an
  `Intent:` diagnostic line.
- The digest (3 KiB): lanes in flight, lanes settled since the last chair save
  (24 hours with no checkpoint), and pull requests named in the checkpoint
  (`PR #12`, `pr/12`, `pull/12`; the sixteen most recent), each from
  `provenant pr status N`. Write delivered PRs into the checkpoint that way.
  Lane listings read one page of 100; a trailing `+` marks a partial count and
  the section gives the command for the next page.
- The chair's checkpoint, then `autosave.md`.

Every printed command repeats the hook's non-default `--state-dir` so it reads
the same store; a hint that cannot round-trip is shown as a note.

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
  [session skill](../../skills/session/SKILL.md)). Codex shares only the
  SessionStart rendering and has no `PreCompact` hook.

## Worktree hooks

Claude Code replaces its own `git worktree` handling with these hooks, so each
prints the new worktree's absolute path and nothing else on stdout.

- `worktree-create` accepts a name of 1 to 64 letters, digits, `.`, `_` and `-`
  (starting alphanumeric) and creates branch `claude/<name>` at
  `<repo>/.worktrees/claude-<name>`. A reused name finds its earlier tree only
  when it is clean, has no live writer, is on that branch at the recorded HEAD
  and both creation records agree (one in the Git admin directory, one in
  `<state>/hooks/claude-worktrees/`). The primary checkout, hand-made trees and
  dirty trees are never reused: exit 2 with the reason.
- `worktree-remove` removes only a linked worktree this hook created, through
  `provenant worktree remove <name>` without `--discard`. Dirty trees, live
  writers, the primary checkout and unmerged branches (`not_merged`) refuse with
  exit 2 and the worktree stays; use `provenant worktree remove` yourself once
  the cause is cleared. Declared regenerable output is reclaimed; any other
  ignored file, such as an undeclared `target/`, keeps the tree.
- A failure means no worktree is created: fix the cause or run
  `provenant install --hooks off` to return to Claude's own worktrees.

Check with `provenant hook claude --help`; the fixture tests are
`cargo test -p provenant --test hooks_session` from `tools/front-door`.
