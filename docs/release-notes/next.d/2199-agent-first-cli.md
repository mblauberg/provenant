## Breaking changes

- Human command results now default to a compact summary of at most 2 KiB with a next command. Use global `--full` for complete human detail or `--json` for the machine envelope. Explicit help and protocol transports retain their complete output.
- `output` defaults to a settlement and result summary. `status --full` includes full lane records and older lanes.
