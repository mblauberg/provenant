## Fixes

- OpenCode lanes no longer end failed with "provider identity was not observed". The answering model is now read from OpenCode's own session record, keyed by the stream session id. A missing or conflicting record still fails the lane.
