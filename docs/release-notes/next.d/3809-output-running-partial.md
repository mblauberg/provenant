## Fixes
### CLI and install

- `provenant output ATTEMPT` on a running attempt now prints the partial result
  under a `running` header and exits 0. The envelope carries `status: running`
  and says the result is partial; a live attempt with no sealed result yet reads
  as an empty, incomplete page instead of a `NONTERMINAL` refusal. (#3809)
