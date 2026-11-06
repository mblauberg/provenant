## Fixes
### Lanes and settlement

- In-lane `run` recognises the parent attempt marker even when other lane markers are absent. Repeating the same child brief creates a separate child rather than replaying the first launch.
- Session hooks collect attempt inventory through `status` and preserve cursor ordering when continuing a partial page.
- Retrying a proven pre-launch refusal preserves pending sibling work. Accepted or uncertain launches retain their custody fence.
- Status retains bounded provider errors, result locations and graph advance failures.
