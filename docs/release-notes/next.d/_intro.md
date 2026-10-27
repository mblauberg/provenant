This release makes lanes self-managing (epic #1829):
- A writer dispatch creates, owns and prunes its own worktree.
- `lanes --wait` and landing replace chair polling.
- Garbage collection runs on settlement and landing, with a scheduled backstop.
- Closed runs' event logs expire under `retention.logs_days`.

It also:
- records Claude lane token usage, including cache counts;
- serialises concurrent Claude OAuth refreshes;
- links each attempt to its provider transcript;
- reports quota use by account across projects.

## Stability

There is no stability promise. Commands, flags, `--json` output, `config.toml`
keys, the state layout and Fabric MCP tool names and schemas can change in any
release, with no deprecation period or compatibility shims. Each release's notes
list its breaking changes. Upgrade every host together, since peers on different
versions are not supported.
