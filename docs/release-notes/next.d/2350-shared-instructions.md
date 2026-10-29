## Fixes
### CLI and install

- `provenant install` and `upgrade` now manage `~/.agents/AGENTS.md` as a link to
  the installed release's shared instructions, so OpenCode and Agy agents that
  read it always follow the current release instead of a stale copy. A plain
  user file blocks the install until it is adopted (`install --adopt-existing`)
  or moved aside, never overwritten silently, and `check install` reports a
  stale or unmanaged shared instructions file with the exact repair command (#2350).
