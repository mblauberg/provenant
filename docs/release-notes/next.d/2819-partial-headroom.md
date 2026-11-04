## Behaviour changes
### Accounts, quota and routing

- Exclude uncalibrated partial quota headroom by default during selection and admission. Set `routing.allow_unknown_partial_headroom` to accept it; an observed exhausted lower bound still blocks the route (#2819).
- Check requested write and network controls against adapter capabilities before selecting a route and again during launch preparation (#2819).
- Restrict automatic route fallback to confirmed local preparation refusals; launched tasks keep their route, while configured read-only result-quality escalation waits for custody release (#2819).
