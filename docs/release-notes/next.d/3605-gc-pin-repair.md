## Fixes

### Lanes and settlement

- `provenant gc` no longer skips a whole project when its run store is unreadable. The import-pin backstop now warns on stderr and keeps the pins it cannot prove settled, so settled-lane collection and the path-based sweep still run. The counting test for batched `update-ref` no longer assumes the subcommand is the first Git argument. (#3605)
