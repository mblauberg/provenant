## Breaking changes
### Chair, inbox and console

- `provenant chair viewer` and its seen, snooze, dismiss and summary-baseline records are removed; nothing read them, and the human inbox and `provenant-console` are unchanged. Opening a Fabric store upgrades it and drops any stored viewer rows, after which older releases refuse the store as newer (#3082).
