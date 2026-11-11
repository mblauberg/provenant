## Fixes
### Telemetry and providers

- OpenCode lanes no longer settle `identity_unobserved` after the model answered. OpenCode 2.x records a session only in `session_v2`, but settlement and resume looked for it only in the older `session` table; either table now names the session, and the answering provider and model are still read from it, so a Go-model lane must still show the Go provider. (#3152)
