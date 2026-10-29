## Behaviour changes
### Accounts, quota and routing

- After a provider capacity error, model selection skips that model for `domains.routing.capacity_cooldown_seconds` (default 600; `0` disables). Explicit models and resumes stay available, and `quota` and `lanes` show live cooldowns (#2050).
