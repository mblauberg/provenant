## Fixes

### Lanes and settlement

- A lane killed before its guardian could publish an exit can leave processes holding the repository's heavy lock. `provenant reconcile <lane>` now ends the provider and descendants the attempt recorded, revalidating each identity before TERM and KILL under writer exclusion; a live guardian or a surviving process still fences settlement, and no exit is invented. When a guardian publishes its cleanup observation, status names observed PIDs and the custody writer-lock path; an open descriptor alone does not prove ownership. Unrecorded detached processes and arbitrary repository locks may remain unreported, including when the guardian dies before publishing an exit. (#3992)

- Concurrent project binding creation and checkout-hint updates now share one publication lock, and GC uses the same contention-safe read as other binding readers. (#3992)
