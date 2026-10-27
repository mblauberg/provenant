## Fixes

- `cancel` and `reconcile` now settle a running writer attempt whose `reservation.json` has gone, once no guardian holds its writer lock and no exit is sealed. The attempt ends as lost with the cause instead of staying `running` after its process has died (#1928).
