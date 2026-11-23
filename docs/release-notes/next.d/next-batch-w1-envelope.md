## Breaking changes
### CLI and install

- The JSON envelope is version 2 and carries a `warnings` array; warnings no
  longer print to stderr under `--json`. Repeated configuration and root
  warnings show once per change, and the state root's `config-warning-*` and
  `root-warning-*` marker files are removed on upgrade. Read warnings from the envelope's `warnings` array and accept `schema_version` 2. (#3778 #3813 #3812)
