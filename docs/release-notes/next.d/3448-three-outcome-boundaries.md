## Behaviour changes

### Git and safety

- The hard boundaries are now three outcomes, defined once in
  [ADR 0035](../../adr/0035-three-hard-boundaries-and-flexible-everything-else.md):
  credentials and secrets reach only what was granted them, no lane changes
  another lane's work, and user gates, merge, release and ruleset changes need
  authority. Everything else warns and proceeds with its source shown. Default
  review panels now include the flexibility lens, and the security lens runs
  only for credential, secret or publication paths or on request. (#3448)
