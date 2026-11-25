## Fixes

### CLI and install

- `provenant upgrade --verify --host <peer>` now exercises the remote write node even when the launch supplies no `--branch`: the bundled smoke graph provisions a scratch branch from a sourced default (overridable with `--branch B --base REF` or `--set write_attempt`), and the peer creates the worktree in its own clone. A bundled smoke launch with no `diff_base` uses the checkout's first parent as its review base, or skips review with a reason if the parent is unavailable. The read lane sees the staged fixture at the path its brief names. (#4004)
