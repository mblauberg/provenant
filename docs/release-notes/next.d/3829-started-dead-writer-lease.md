## Fixes

### Lanes and settlement

- `reconcile` and `release attempt:ID` can reclaim an unreadable dead attempt's writer lease on the same boot when supervisor exit evidence proves cleanup completed; an exit still settling keeps the fence. (#3829)
