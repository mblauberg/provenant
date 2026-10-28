## Fixes

- Refuse a stalled object import after three verification retries or 30 lock waits and release writer custody. The locked publication and connectivity budget is 60 seconds plus 10 seconds per started GiB, capped at 20 minutes; staged files are checked across publication.
