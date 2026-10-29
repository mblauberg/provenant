# Interview and spike

## Interview

Keep one decision context: constraints, evidence, decided branches, parked owner
calls and the next unresolved branch. For each question give the evidence already
known, two or three options when useful, and a recommended answer with its
trade-off. If the user is unsure why, give the trade-offs in prose first and offer
options only once they can decide. Do not run a second interview on the same
request. The interview is not approval; return the register to the spec gate.

## Spike

Agree four things before building: the one falsifiable question ("can X stream 10k
rows without buffering?", not "explore X"), a timebox (default 30 to 60 minutes of
agent work), what result answers it either way, and a registered scratch worktree
away from production source. Hitting the box with no answer is a finding: split
the question. Cheat freely (hardcode, stub, fake the network) except in the
variable under test. Do not touch credentials, user data or external systems
without separate authority.

Harvest into the spec, then retire the code. A spike never graduates to production;
rebuild the smallest production version through implement.

```markdown
**Spike:** <question> (date, timebox)
**Answer:** yes, no or partial, with numbers where measured
**Evidence:** <command and output>
**Implication:** <what the spec now says>
```

Skip the spike when a search or the docs answer it in ten minutes.
