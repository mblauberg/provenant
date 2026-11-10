## Changes
### Review and repair

- A review panel that cannot meet its provider-family minimum now runs with one warning per short family in the panel record and each lens's advice, including panels re-planned during repair. Set `review.diversity = "strict"` to refuse it as before (#3135).
- `max_repair_cycles` keeps its default of three and no longer has an upper limit. A raised budget runs every round before the principal's design judgement; the default budget still asks for it at the third review (#3135).
- `scripts/check-skill-negatives` reports short near-miss coverage and exits 0; it still fails when it cannot read a fixture (#3135).
