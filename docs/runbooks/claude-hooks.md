# Run the Claude Code session and worktree hooks
Owner: provenant
Anchor: tools/front-door/tests/hooks_session.rs

`provenant install --harness claude --hooks on` registers the hooks below in
the Claude settings (see [install](install.md)). Rerun it after an upgrade to
register new handlers. Every handler exits 0 on its own failure except the
worktree hooks, prints only the protocol response on stdout and sends
diagnostics to stderr.

| Event | Handler | What it does |
|---|---|---|
| `SessionStart` (`startup`, `resume`, `clear`) | `hook claude session-start` | Reports the checkpoint status, then the state digest |
| `SessionStart` (`compact`) | `hook claude compact` | The same report after compaction |
| `PreCompact` | `hook claude pre-compact` | Saves the current state into the checkpoint |
| `Stop` | `hook claude stop` | Refreshes an existing checkpoint's state if it is 10 minutes quiet |
| `WorktreeCreate` | `hook claude worktree-create` | Runs `provenant worktree create claude/<name>` |
| `WorktreeRemove` | `hook claude worktree-remove` | Runs `provenant worktree remove <name>` |

## What the SessionStart digest shows

The digest is read-only and at most 3 KiB; the whole hook output stays under
8 KiB, and the checkpoint excerpt shrinks first.

- **Lanes in flight:** this project's non-terminal lanes from
  `provenant lanes`, with ID, state and route, and the worktree and delivery
  branch when the listing reports them.
- **Lanes settled since the last chair save:** terminal lanes (including
  `input_required`) that finished after the chair last wrote the checkpoint.
  With no checkpoint, the last 24 hours.
- **Pull requests named in the checkpoint:** the digest takes PR numbers from
  the chair's own text (`PR #12`, `pr/12`, `pull/12`, `pull request 12`; at
  most the eight most recent) and runs `provenant pr status N` for each. Write
  the PRs you delivered into the checkpoint that way. When `pr status` is
  absent, slow or failing, the line says so and gives the command to run.

Each lane listing reads one page of 100. When more exist, the count shows a
`+` (for example `Lanes settled since the last chair save (3+)`) and the
section ends with `this count is partial; run <command>`, where the command
lists the next page and repeats the hook's `--state-dir` (made absolute and
single-quoted) when it used a non-default one, so it reads the same store. If
that path is not valid UTF-8 or holds control characters, no runnable command
is shown, only a note to page with the state directory the hook used. The
command is also shown only if it survives rendering byte for byte (at most 512
bytes, no `<`, `>` or backticks); otherwise a plain note names the cursor. A
partial count is never shown as complete.

A section the hook could not read is listed as unavailable, never as empty.
The hook budget is 4 seconds in total, with the lane, PR and worktree reads
running in parallel; a read that overruns leaves its section unavailable.

## What the autosave keeps

`pre-compact` and `stop` save the derived state to `autosave.md` in the
session's directory, beside `checkpoint.md`:

```text
## Provenant session state
Auto-saved: 2026-10-27T03:00:00Z
...
```

- The chair's checkpoint is never written by a hook. Its bytes, trailing
  newlines and modification time stay as the chair left them, a copy of the
  saved state in the chair's notes is just notes, and "settled since the last
  chair save" is measured from the checkpoint's own modification time.
- `session-start` and `compact` show the status diagnostic, then the freshly
  derived digest, then the chair's checkpoint, then `autosave.md`, under the
  8 KiB cap (the checkpoint text shrinks first).
- A `workflow session save` replaces only `checkpoint.md`; the next autosave
  rewrites only `autosave.md`. A checkpoint from an earlier release that holds
  an in-body state block keeps it as ordinary chair text; the new file starts
  alongside it.
- The saved state is derived from the run store, `worktree list` and `pr status`. It
  grants no authority, and the chair's notes stay the record of intent.
- The write takes the checkpoint lock, so it never interleaves with a chair save.
- `pre-compact` creates `autosave.md` when no checkpoint exists. The next SessionStart
  then reports that it holds only automatically saved state and that chair
  notes still need saving. `stop` never creates either.
- Hooks inside a lane (a confined Claude lane) report but never write.

## Worktree hooks

Claude Code replaces its own `git worktree` handling with these hooks, so they
must print the new worktree's absolute path and nothing else on stdout.

- `worktree-create` takes the suggested name, accepts only 1 to 64 letters,
  digits, `.`, `_` and `-` starting with a letter or digit, and creates the
  branch `claude/<name>` from the session's current head. The path is
  `<repo>/.worktrees/claude-<name>`, owned and listed by
  `provenant worktree list`. A name Claude reuses finds its earlier worktree
  only when that is a linked worktree at the exact path above, on
  `claude/<name>`, clean, with no live writer, and still owned by the creation
  that made it. Each creation writes a record (canonical path, branch, the HEAD
  it was made at and a random token) to the worktree's Git admin directory
  (`provenant-claude-hook`) and to Provenant's state
  (`<state>/hooks/claude-worktrees/`). Reuse needs both copies present and equal,
  and path, branch and HEAD to match the tree now, so a tree removed and
  recreated by hand, repurposed, or moved on by a commit is not adopted.
  `worktree-remove` deletes the state record with the tree. The record guards
  against adopting a tree the hook did not make; a same-user process that
  forges both copies is outside that threat model. The
  primary checkout, a tree made by hand or by another tool, a tree elsewhere
  and a dirty tree are never reused: the hook exits 2 and names the reason.
- `worktree-remove` removes only a linked worktree that `provenant worktree
  list` shows for the project and that this hook created (both ownership
  records present and equal; a commit since creation does not matter). Any
  other tree exits 2 with "it was not created by this hook". It removes through `provenant worktree remove <name>`
  without `--discard`. The owner refuses dirty trees, live writers and the
  primary checkout, keeps an unmerged branch, and reports `not_merged`.
  A refusal exits 2 with the cause on stderr and the worktree stays.
  The hook never forces removal; use `provenant worktree remove` yourself
  once the cause is cleared.
- Ignored build output (for example `target/`) counts as dirty for removal
  today, so Claude's cleanup of a tree that was built in exits 2 with
  `worktree-remove: refused: ... dirty` and leaves the tree. The hook does not
  change that policy; clearing ignored output on removal is owned by #1816.
  Until it lands, delete the ignored output and run `provenant worktree remove
  <name>` yourself.
- The CLI inherits the hook's environment and `--state-dir`. Failures mean the
  worktree is not created; fix the cause or run `provenant install --hooks off`
  to return to Claude's own worktrees.

## Check

```sh
provenant hook claude --help
provenant lanes --json
provenant worktree list --json
```

Run the fixture-backed tests with
`cargo test -p provenant --test hooks_session` from `tools/front-door`.
