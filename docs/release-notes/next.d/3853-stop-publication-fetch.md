## Fixes
### Lanes and settlement

- Concurrent `run implement` launches no longer lose stop publication when an unrelated remote tracking ref moves: delivery and stop publication now share one coalesced, locked refresh, so a racing ref transaction is tolerated instead of dropping the binding. (#3853)
