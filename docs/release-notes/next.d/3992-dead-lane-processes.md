## Fixes

### Lanes and settlement

- A lane killed before its guardian could publish an exit can leave processes holding the repository's heavy lock. `provenant reconcile <lane>` now ends the provider and descendants the attempt recorded, revalidating each identity before TERM and KILL under writer exclusion; a live guardian or a surviving process still fences settlement, and no exit is invented. Status names surviving PIDs and the lock resources observed at the cleanup deadline, marking a holder found only by its open descriptor as ownership unproven so it is reported but never signalled. (#3992)
