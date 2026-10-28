## Behaviour changes

- `pr status` reports an empty check result as `checks=unavailable` with null JSON counts, instead of zero checks, so a missing check run is never read as passing.
