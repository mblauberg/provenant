## Breaking changes
### Lanes and settlement

- A writer whose handoff says `blocked` now settles with the cause `blocked` and its note, and a launch refused before the provider starts records its specific cause and a bounded message; the generic `preparation_refused` cause is removed. (#3167)
