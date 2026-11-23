## Fixes
### CLI and install

- `provenant output ATTEMPT` on a running attempt now exits 0, carries
  `status: running` in the envelope and labels the page instead of refusing with
  `NONTERMINAL`: `partial` when bytes arrived before settlement, and
  `no result yet` when the result is not sealed. (#3809)
