## Fixes
### Routing and preparation

- A model-only pin whose account admission refused now records the real
  exclusion cause (provider, free-tier or mismatch) instead of exhaustion, and
  when the redraw to another account cannot serve the intent it is attempted
  once on the refused account with the recorded warning. Advisory expiries come
  only from a reset in the future, so an expired availability record cannot
  justify the attempt. (#3764)
