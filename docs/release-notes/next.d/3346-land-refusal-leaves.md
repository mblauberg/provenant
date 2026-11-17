## Fixes

### Lanes and settlement

- `provenant land --json` refusals from GitHub's merge endpoint keep their typed leaf cause in `error.cause` (`HEAD_MOVED` for a 409, `MERGE_NOT_ALLOWED` for a 405) with a non-empty `error.repair`, under the unified `LAND_REFUSED` code, #3346.
