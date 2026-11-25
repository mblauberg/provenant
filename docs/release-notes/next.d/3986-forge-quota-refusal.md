## Fixes

### Runs and review

- `provenant run review --pr N` no longer reports an exhausted GitHub REST core quota as `INVALID_INPUT` naming an input field the caller never supplied. When the batched GraphQL read has valid base and head subject fields plus a usable PR URL, it serves the review even if the check rollup is truncated; otherwise the refusal is typed `FORGE_QUOTA`, carries the reset and says `retry after <reset>`. A peer that refuses this way is retried rather than settled as a permanent verdict. (#3986)
