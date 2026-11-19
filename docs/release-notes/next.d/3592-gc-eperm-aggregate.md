## Fixes

### CLI and install

- `provenant gc` no longer floods its output with one EPERM census warning per unreadable process. A census now emits a single aggregated warning with a count and what it means: custody is unknown for those processes, so they are not treated as free. Repeated censuses on the same host do not repeat the warning, and `provenant gc --full` still lists each affected PID. (#3592)
