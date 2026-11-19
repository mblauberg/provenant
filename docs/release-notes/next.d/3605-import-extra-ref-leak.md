## Fixes

### Lanes and settlement

- Import pins under `refs/provenant/import-extra/<attempt>/` no longer outlive their attempt. Settlement now removes them on every terminal outcome (success, failure, cancellation and declared stop), and `provenant gc` removes the pins of settled or missing attempts, so ref listings and `packed-refs` stay small. Thousands of pins are deleted in one batched `update-ref --stdin` transaction rather than one Git process per pin. A refused import still keeps its pins as recovery evidence. (#3605)
