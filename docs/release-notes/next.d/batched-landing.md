## Behaviour changes
### Git and safety

- `provenant land` gates ordered batches, including an unflagged single PR by default, on an integration candidate and records outcomes and recovery evidence. Confirmed landings run the existing safe local cleanup, and recovery retries incomplete follow-up. Recovery rechecks PR heads, approvals and bases before retrying an uncertain publication, and uses a configurable settle interval when it cannot safely retry. Use `--shadow` to compare its decisions before switching from another lander (#2856).
