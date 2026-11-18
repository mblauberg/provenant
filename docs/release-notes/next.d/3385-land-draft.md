## Fixes

### Git and safety

- `provenant land` checks pull request draft status early to refuse draft pull requests before candidate gating, and preserves GitHub's response message on HTTP 405 merge refusals. (#3385)
