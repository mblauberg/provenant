## Fixes

### Lanes and settlement

- Malformed handoffs now retain valid fields and report the failing field and decode error in status and output, instead of appearing missing. String concerns, scalar remaining/decision/verification strings and status casing decode to their canonical forms. (#3078)
