## Behaviour changes

- Dispatch now probes accounts only for adapters that the route, its pool or an
  explicit fallback can use. A broken login for an adapter that a dispatch
  cannot reach no longer shows a skip line or runs that provider's commands
  (#1571).
