## Fixes
### Lanes and settlement

- Repeated command preparation uses fresh private Git scratch space, avoiding a file-exists refusal and keeping snapshots from different checkouts separate.
