## Fixes
### Lanes and settlement

- A writer whose observed provider identity contradicts the requested route now settles `review_required` with its committed head and a warning naming both the requested and observed identities; the work is no longer failed or discarded when publication is unavailable. A writer with no verified new head, and read-only lanes, keep the fatal `identity_mismatch` decision. (#3557)
