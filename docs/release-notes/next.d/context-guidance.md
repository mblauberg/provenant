## Fixes
### Lanes and settlement

- Every lane is told to locate before reading: search with line numbers, then read a bounded range, because output stays in the lane's context and is paid for again on each later turn. `orchestrate` and `session` direct chairs to send broad code reading to read-only lanes.
