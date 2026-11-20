## Fixes

### Lanes and settlement

- Import pins under `refs/provenant/import-extra/<attempt>/` no longer outlive their attempt. Settlement now removes them on every terminal outcome (success, failure, cancellation and declared stop), and `provenant gc` removes the pins of settled or missing attempts, so ref listings and `packed-refs` stay small. Thousands of pins are deleted in one batched `update-ref --stdin` transaction rather than one Git process per pin. When cleanup withholds an import pin as recovery evidence (no writer slot or a writer fence with issues), it now keeps the import-extra pins too. `provenant gc --dry-run` reports how many import pins it would remove; only an applied pass deletes them. (#3605)
