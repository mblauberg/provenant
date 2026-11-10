## Changes
### Review and repair

- A review panel that cannot meet its provider-family minimum now runs with a warning in the panel record and each lens's advice. Set `review.diversity = "strict"` to refuse it as before (#3135).
- `max_repair_cycles` keeps its default of three and no longer has an upper limit (#3135).
- `scripts/check-skill-negatives` reports short near-miss coverage and exits 0; it still fails when it cannot read a fixture (#3135).
