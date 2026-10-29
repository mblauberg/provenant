## Behaviour changes

- After a provider capacity error, model selection backs off that model for a host cooldown set by `domains.routing.capacity_cooldown_seconds` (default 600; `0` disables new cooldowns). Explicit models and resumes stay available, and `quota` and `lanes` show live cooldowns (#2050).
