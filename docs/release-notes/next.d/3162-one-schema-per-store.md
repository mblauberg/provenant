## Breaking changes
### CLI and install

- **Every state database opens from one schema at one version.** A database at any other version is refused with `STATE_RESET_REQUIRED` and left unchanged; the message says to move the store aside. 2.0 starts from a fresh state root, so no 1.x store reaches it. Migration chains, pre-migration snapshots and older-shape readers are removed. (#3162)
