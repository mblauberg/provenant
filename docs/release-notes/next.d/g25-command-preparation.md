## Fixes
### Lanes and settlement

- Repeated read-only command and model-provider preparation uses fresh private Git scratch space, avoiding a file-exists refusal and keeping snapshots from different checkouts separate.
