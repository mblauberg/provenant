## Changes
### Runs

- Every lane limit (token budget, context handoff threshold, deadline) is one record with a value, a source (`default`, `host`, `project`, `template` or `launch`) and a strength (`default`, `ceiling` or `advisory`). The lane preamble states the limits the host will enforce, from the same record the host enforces, and `status --json` reports each lane's limits with their sources. No default changed. (#3169)
