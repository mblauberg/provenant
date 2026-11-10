## Behaviour changes
### Commands

- `pr status`, `pr watch`, `land` waiting, the landing gate and post-merge checks read CI through one observer per repository. Concurrent waiters on a head share one GitHub read per poll, an unchanged answer is revalidated with a conditional request, and a rate limit is reported once with its reset time while every waiter backs off until then. `pr watch` now polls the pinned head itself instead of running `gh pr checks --watch`. (#3137)
