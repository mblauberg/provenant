## Fixes

- Refuse a stalled object import after three verification retries or 30 object-lock waits, seal refusal evidence and release writer custody. Persist retry counts across restarts and prune them at terminal publication. Release `writer.lock` during object verification and live delivery while retaining the reservation and a separate settlement guard, so cancel and reconcile can progress. Reacquire within five seconds, validate the reservation, and recheck the verified tip and object-file generation before branch publication. Terminal publication receives a fresh database budget.
