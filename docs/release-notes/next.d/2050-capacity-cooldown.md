## Behaviour changes

- Pace model draws after a provider capacity error with a host cooldown configured by `domains.routing.capacity_cooldown_seconds` (default 600 seconds; zero disables new cooldowns). Explicit models and resumes remain available, and quota and lanes show live cooldowns.
