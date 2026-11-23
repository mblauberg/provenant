## Changes
### Envelope and warnings

- The JSON envelope is version 2 and carries a `warnings` array; warnings no
  longer print to stderr under `--json`. Repeated configuration and root
  warnings show once per change, and the state root's `config-warning-*` and
  `root-warning-*` marker files are removed on upgrade. (#3778 #3813 #3812)
