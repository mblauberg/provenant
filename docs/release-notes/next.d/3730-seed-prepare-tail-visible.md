## Fixes

### CLI and install

- `provenant worktree seed-build` now shows the bounded output of a failed `build.prepare` command as an indented cause in human output and in `error.cause` under `--json`; the output tail no longer has to be reproduced by hand to learn why the command failed (#3730)
