## Fixes
### CLI and install
- `provenant wait --pr --until complete` now defaults to a 60-minute budget from `domains.wait.pr_complete_timeout_seconds` (3600 seconds), covering a normal CI run without repeated re-arming. An explicit `--timeout` still wins, other waits keep `domains.wait.timeout_seconds`, and every budget still clamps to `domains.wait.max_timeout_seconds`. The help text and `config/harness-knobs.json` name the new setting. On timeout a complete wait now names the checks still pending in its last-known summary. (#3603)
