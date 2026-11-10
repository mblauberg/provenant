## Fixes
### Lanes and settlement

- A graph reads a peer's handoff only from the sealed handoff in the peer's status, not from its answer text. A peer on an earlier release returns no handoff, so upgrade hosts together (#3122).
