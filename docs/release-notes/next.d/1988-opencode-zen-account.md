## Fixes

- Attribute OpenCode Console free models to the separate `zen` account in lanes, telemetry, quota and pacing. Go account pacing no longer applies to free models.
- Show Zen quota separately using observed free-limit refusals, with an unknown daily reset unless the provider supplies one.
- Unregistered local OpenCode logins now identify paid lanes as `go` and Console lanes as `zen`. Historical account labels and refusals are not rewritten; older `local` continuations need a new run under the new identity.
