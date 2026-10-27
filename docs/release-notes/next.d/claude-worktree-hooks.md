## Behaviour changes

- `provenant install --hooks on` now also registers Claude Code `WorktreeCreate`
  and `WorktreeRemove` hooks. Worktrees Claude creates for native agents are
  made by `provenant worktree create` on the branch `claude/<name>`, so they are
  registered with an owner from the start, and removal goes through
  `provenant worktree remove`. A name outside letters, digits, `.`, `_` and `-`,
  or a worktree the owner refuses to remove (dirty, live writer, primary or not
  listed for the project), fails the hook and leaves the worktree in place;
  the hooks never force. Without `--hooks on` Claude keeps creating its own
  worktrees (#1845).
