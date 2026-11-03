## Fixes
### CLI and install

- The brief read-root scan skips `/`, `~` and ancestors of effective read roots, and `[domains.run] brief_read_root_warnings = false` turns the warning off (#2737).
