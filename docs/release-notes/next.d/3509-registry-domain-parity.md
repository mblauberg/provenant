## Fixes
### CLI and install
- `config/harness-knobs.json` now declares every setting published by a domain's field metadata, including all `domains.wait` settings and `domains.fabric.steer.fallback`. A parity test fails and names any domain field the registry lacks, so new settings cannot ship undocumented. (#3509)
