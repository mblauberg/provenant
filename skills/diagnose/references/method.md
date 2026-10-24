# Diagnostic method

## Feedback-loop menu

Prefer the smallest deterministic seam that reaches the failure:

1. failing test;
2. curl/HTTP script;
3. CLI invocation against a fixture;
4. headless browser assertion over DOM, console or network;
5. captured request, payload or trace replay;
6. throwaway one-call harness;
7. property/fuzz loop;
8. `git bisect run` between known states;
9. differential old-versus-new execution; or
10. a bounded user-feedback loop.

Sharpen the signal before interpreting it: cache setup, narrow scope, assert
the exact symptom, seed randomness, pin time, isolate filesystem/network and
repeat enough times to estimate a flake rate.

## Hypothesis table

For each candidate record: rank, proposed cause, predicted observation,
single-variable probe, result and status. Reject or update it from evidence;
do not silently replace it with a new story.

## Repair gate

The regression seam must exercise the real bug pattern at its call site. Watch
it fail before the repair and pass after it. Also rerun the original scenario
and relevant broader suite. If the seam cannot be built, report the coupling
that prevents it.

## Evidence before claims

- A failed or stochastic search proves nothing about impossibility. State
  "none exists" or "best achievable" only from an exact formulation or an
  exhaustive check; otherwise say "best found within this sampled envelope".
  Recheck that a threshold borrowed from someone else's numbers survives a
  change of metric.
- Search for a name you do not confidently recognise, or one from a
  fast-moving area, before answering about it. Familiarity makes a stale
  answer sound authoritative.
- Before asserting absence, identity or drift, run the exact check that would
  prove it.

## Closure checks

- original reproduction no longer fails;
- regression passes or the missing seam is documented;
- unique debug prefixes are absent;
- run-owned throwaway artifacts are retired under their manifest/authority;
- the root cause and prevention opportunity are recorded.
