## Fixes

### CLI and install

- Advice now names live commands. A graph whose worktree is missing tells the
  operator to run `provenant stop NAME` instead of the removed `cancel`, and an
  invariant test checks the commands named in user-facing Rust strings against
  the live command tree. (#3529)
