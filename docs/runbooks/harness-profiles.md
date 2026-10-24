# Harness configuration

Read effective configuration with `provenant config show`. The Rust config
owner reads the selected state root's `config.toml` and validates owner sections.
Use `--state-dir /absolute/path` to select an isolated root.

The Python profile presets and profile mutation wrapper are retired. Set
route, effort, access and timeout explicitly on dispatch; consult
`provenant help dispatch` for the supported controls. Install registrations
with `provenant install --dry-run` before applying an authorised install.
