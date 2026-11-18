## Enhancements

### Configuration

- Port layers now deep-merge object fields, so a project or host layer that
  sets only `verify.base` keeps the lower layer's other verify fields, and
  `schema_version` is optional in an overlay. `config show` lists the effective
  `verify.base` and `verify.remote` with their layer. An unset `verify.base` is
  inferred from `refs/remotes/<remote>/HEAD`; if nothing resolves, the refusal
  names the key and the files to set with an example. Builtin verify tiers now
  use neutral cargo commands, while this repository keeps its own
  `scripts/verify-nextest` tiers in `config/ports.json`. Malformed port layers
  name their file and key instead of surfacing as a Git configuration error;
  an invalid project layer warns and is skipped so commands that do not need
  verify still run. (#3534)
