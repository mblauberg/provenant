## Behaviour changes

### CLI and install

- Port tables now declare `schema_version: 3`. A repository `.provenant/ports.json` that still declares 2 is read with the current schema after a `CONFIG_MALFORMED` warning: set `"schema_version": 3` in that file, or rerun init to regenerate it. The install migration updates state copies only, not repository files. (#3849)
