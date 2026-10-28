## Additions
- Add a read-only, deadline-aware local answer reader over exact session discovery and an indexed pending outbox. Explicit checkpoint saves publish an advisory workspace lookup.
- Offer queued human answers once per Claude session at Bash tool calls, recording offers within the existing guard deadline and an 8 KiB final JSON limit. Denials retain priority, failures diagnose on stderr, and acknowledgements remain explicit.
