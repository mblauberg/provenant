## Fixes

- Bound object verification and lock-wait retries so a stalled import eventually fails and releases writer custody. Shorten the locked connectivity check and verify large staged files across publication.
