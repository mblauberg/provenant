## Fixes
### Lanes and settlement

- `lanes --wait` no longer fails with a run-store path error when a running lane replaces its process checkpoint during the read (#2057).
- A refused run-store path error names the path, relative to the state root, and the failed check (#2057).
