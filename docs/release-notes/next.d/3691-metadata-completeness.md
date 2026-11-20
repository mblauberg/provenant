## Fixes
### CLI and install
- Every setting a domain accepts now has field metadata and a `config/harness-knobs.json` entry, including the account registration fields (adapter, label, key, directory and their limits, plans and directories) and the land override composition and branch fields. A parity test derives settings from the typed schemas and fails, naming any key metadata or the registry omits. (#3691)
