## Breaking changes
### CLI and install

- **Every state database opens from one schema at one version.** A database at any other version is refused with `STATE_RESET_REQUIRED` and left unchanged; the message names `install --reset-state`. Migration chains, pre-migration snapshots and older-shape readers are removed. (#3162)
