## Fixes

- Refuse a stalled object import after three verification retries or 30 lock waits and release writer custody. Seal refusal evidence in the attempt directory and prune terminal retry counters. The locked publication and connectivity budget is 60 seconds plus 10 seconds per started GiB, capped at 20 minutes; staged-file hashing timeouts also release custody. Verification checks staged and reachable objects without hashing unrelated shared-store objects, and terminal publication gets a fresh database budget afterwards.
