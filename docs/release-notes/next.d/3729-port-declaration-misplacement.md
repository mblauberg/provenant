## Fixes

### CLI and install

- `provenant config check` now warns when a port declaration sits in a file the port reader never reads, naming the file and dotted key: a `build.prepare` table in `.provenant/config.toml`, or a host-project `ports.json` filed under the 32-digit project id instead of the full 64-digit project key. `config ports --json` reports the effective `build.prepare` value with its own leaf source, so a misplaced declaration can be diagnosed before a seed build fails with E0583; warnings never refuse a command. (#3729)
