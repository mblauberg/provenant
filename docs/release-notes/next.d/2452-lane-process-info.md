## Fixes
### Lanes and settlement

- macOS lanes can query their own processes' resident memory through libproc, while outside-host queries are denied. Stock `ps` still fails during privileged execution, so memory guards that invoke it remain blocked (#2452).
