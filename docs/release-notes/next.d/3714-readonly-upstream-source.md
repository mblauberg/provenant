## Fixes

### Lanes and settlement

- A read-only task launched from a checkout behind its fetched upstream now reviews that upstream head by default instead of the stale local tree. The launch names the source ref and commit it used and why, and keeps the behind warning; `--source local` still reads the local checkout on purpose. (#3714)
