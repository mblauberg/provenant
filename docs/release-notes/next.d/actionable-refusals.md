## Behaviour changes
### CLI and install

- CLI refusals now always carry a concrete next step in `error.repair`: the exact command or flag to run, or the condition to wait for. The renderer no longer substitutes a circular `provenant help <command>` when a repair is absent, and a new `domains.wait.preflight_busy_timeout_ms` (default 2000) lets a wait tolerate a momentarily busy store during preflight while the final snapshot stays nonblocking.
