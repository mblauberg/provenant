## Fixes

- OpenCode lanes no longer fail with "provider identity was not observed". The answering model is read from OpenCode's own session record, and a missing or conflicting record still fails the lane (#1936).
