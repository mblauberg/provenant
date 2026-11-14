## Fixes

### CLI and install

- Commands read their declared configuration domains, so unrelated invalid settings warn without blocking the command; help lists those domains and preserves flag requirements and child commands. (#3078)
