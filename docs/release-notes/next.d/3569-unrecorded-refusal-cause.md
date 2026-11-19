## Fixes
### Lanes and settlement
- A refused attempt whose record names no cause now reads `unrecorded` in `result` and `status`, instead of being reported as an access-policy refusal the host never observed. This covers refusals withdrawn before launch, queued proposals the host refuses, and graph attempts that were never launched. (#3569)
