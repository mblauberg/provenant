## Behaviour changes
### Accounts, quota and routing

- After a provider capacity error, model selection skips that model for a cooldown set by `domains.routing.capacity_cooldown_seconds` (default 600; `0` disables new cooldowns). Explicit models and resumes stay available, and `quota` and `lanes` show live cooldowns (#2050).
