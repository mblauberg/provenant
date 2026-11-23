## Fixes
### Accounts, quota and routing

- A model-only pin whose account admission refused now records the real
  exclusion cause (provider, free-tier or mismatch) instead of exhaustion, and
  when the redraw to another account cannot serve the intent the pinned route is
  attempted once on its refused account with the recorded warning. That final
  attempt is limited to advisory recorded refusals (exhaustion, pacing or a
  provider/free-tier refusal); a missing login, confinement refusal or another
  non-advisory cause is not retried. Advisory expiries come only from a reset in
  the future, so an expired availability record cannot justify the attempt.
  (#3764)
