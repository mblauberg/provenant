## Fixes
### CLI and install
- `provenant output <target>` without `--full` keeps both ends of a result: a short opening and a tail large enough for the final message, its `VERDICT:` line and the whole sealed handoff block. The omitted middle is named, so drivers parsing bounded output no longer miss verdicts. [#3990](https://github.com/mblauberg/provenant/issues/3990)
