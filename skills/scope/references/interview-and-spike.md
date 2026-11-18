# Interview and spike

## Interview

Keep one decision context: constraints, evidence, decided branches, parked owner
calls and the next unresolved branch. For each question give the evidence already
known, two or three options when useful, and a recommended answer with its
trade-off. If the user is unsure why, give the trade-offs in prose first and offer
options only once they can decide. The interview ends in the spec digest.

## Spike

Set out before building: the falsifiable question ("can X stream 10k
rows without buffering?", not "explore X"), a timebox (default 30 to 60 minutes of
agent work), and what result answers it either way. Isolate throwaway code from
production source when appropriate. Hitting the box with no answer is a finding: split
the question. Cheat freely (hardcode, stub, fake the network) except in the
variable under test. Do not touch credentials, user data or external systems
without separate authority.

Harvest the evidence into the spec. Assess spike code against production requirements before reuse.

```markdown
**Spike:** <question> (date, timebox)
**Answer:** yes, no or partial, with numbers where measured
**Evidence:** <command and output>
**Implication:** <what the spec now says>
```

Skip the spike when a search or the docs answer it in ten minutes.
