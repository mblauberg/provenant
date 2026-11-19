## Fixes

### CLI and install

- `scripts/collate-release-notes --check` now fails, naming file and line, on a fragment with a second section heading or content before its first heading, which collation used to drop or misfile; `scripts/prepare-release` runs the check before it changes anything. (#3574)
