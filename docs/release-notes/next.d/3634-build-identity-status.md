## Additions

### CLI and install

- `provenant status --json` attempt rows now include `provenant_version` and `release_source_digest` read from the attempt's stored trace, so an operator can tell which build ran a lane without reading SQLite. Both keys are omitted for older attempts that recorded no such observation. (#3634)
