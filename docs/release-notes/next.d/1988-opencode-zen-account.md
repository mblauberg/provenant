## Fixes
### Accounts, quota and routing

- OpenCode Console free models now count against a separate `zen` account in lanes, telemetry, quota and pacing, so Go pacing no longer applies to them (#1988).
- Unregistered local OpenCode logins now identify paid lanes as `go` and Console lanes as `zen`, so continuing an older `local` run needs a new run (#1988).
