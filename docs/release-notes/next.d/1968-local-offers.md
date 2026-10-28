## Additions
- Add a read-only, deadline-aware local answer reader over exact session discovery and an indexed pending outbox. Explicit checkpoint saves publish an advisory workspace lookup.
- Offer queued human answers once, then re-offer to that exact Claude session until acknowledged, at Bash tool calls. Recording stays within the existing guard deadline and an 8 KiB final JSON limit. Denials retain priority and failures diagnose on stderr.
