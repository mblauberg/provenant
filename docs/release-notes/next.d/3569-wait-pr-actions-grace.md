## Fixes
### Chair, inbox and console
- `wait --pr` no longer settles on the first pass of a fresh head before its Actions workflows exist. A reading that would settle stays pending until an Actions run is reported for the head or `domains.wait.actions_grace_seconds` (default 60; 0 disables) passes. Unavailable Actions runs warn once per wait, a timed-out `--until complete` wait keeps its last known line, failure lines bound and sanitise check names and links, and the spec says that runs stuck in `waiting` hold a complete wait until its timeout. (#3569)
