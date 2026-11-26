## Fixes

### Adapters

- The native updater again takes its default command from the adapter's declared
  `identity.update` after the grouped-`AdapterSpec` refactor. An explicit
  `providers.<id>.update` override still wins, and adapters that declare no
  updater are left alone. (#4034)

### Configuration reference

- `providers.<id>.version_probe` and
  `providers.<id>.version_probe_timeout_seconds` now have generator contracts,
  so the compact configuration reference documents their defaults and effects
  instead of refusing to generate. (#4034)

### Routing

- An exact pin whose adapter declares no effort control again records a sourced
  `effort_warning` naming the requested selector, declaration source and reason
  while dropping the control; pools still prefer a fallback. (#4034)
