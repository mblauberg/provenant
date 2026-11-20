## Fixes

### CLI and install

- `ci failures` reads each job log with `--allow-escape-sequences` and strips terminal control sequences before parsing, so a failed job is listed with its failed step and failing tests instead of being silently dropped; when a log genuinely cannot be read, the gh message is reported as unreadable evidence rather than as zero failures. The aggregate gate also checks every declared matrix member, so a missing member keeps the gate a failure with uncertain cause instead of a cancellation (#3727)
