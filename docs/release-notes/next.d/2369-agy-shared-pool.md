## Fixes
### Accounts, quota and routing

- `provenant quota` names an exhausted agy pool on the account line, for example `claude pool exhausted (shared)`, and the summary reports a partly spent agy account instead of leaving the shared pool invisible. Routing keeps refusing only the spent pool's models: an agy gemini lane stays available while the shared Claude pool is out (#2369).
