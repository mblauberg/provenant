## Fixes

### CLI and install

- `provenant worktree seed-build` now shows the bounded output of a failed `build.prepare` command as an indented cause in human output and in `error.cause` under `--json`; the output tail no longer has to be reproduced by hand to learn why the command failed. The cause follows the message and precedes `next:`, is masked under `redaction=full`, and is summarised to one row in compact output (#3730)
- Refusals now render the diagnostic detail as an indented human cause (and as `error.cause` in JSON), between the message and `next:`; previously the detail reached only structured fields (#3730)
