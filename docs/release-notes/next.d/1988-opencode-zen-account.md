## Fixes

- OpenCode Console free models now count against a separate `zen` account in lanes, telemetry, quota and pacing, so Go account pacing no longer applies to them (#1988).
- Quota reports show Zen usage separately, based on observed free-limit refusals. The daily reset is unknown unless the provider supplies one (#1988).
- Unregistered local OpenCode logins now identify paid lanes as `go` and Console lanes as `zen`. Existing account labels and refusals are not rewritten, so continuing an older `local` run needs a new run under the new identity (#1988).
