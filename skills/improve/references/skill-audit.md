# Skill audit

Read-only. Report evidence; a fix needs its own grant and goes through
[skill author](skill-author.md).

## Check

Mark missing evidence `N/A`; an unsupported result is never zero.

| Dimension | Evidence |
|---|---|
| Selection | Loaded for the requests it should take, from a routing run or explicit invocation. Loading is not selection. |
| Over- and undertrigger | Requests selected in error, and missed ones. A rate needs a denominator. |
| Conflict | Pairs of skills that compete for one request, contradict on authority or set competing completion gates. |
| Duplication | Rules the always-loaded instructions or another skill already state. Each rule has one home. |
| Steering | Lines a capable agent would get wrong without the skill. Lines it already follows are cost. |
| Environment | Named paths, tools and commands exist in the installed version (check `--help`, do not read alone). |
| Token cost | Always-loaded description and triggered body, against evidenced use. |
| Supply chain and authority | Source, version and licence; scripts, hooks, binaries and network use; state writes. |

Name failures by mechanism: premature completion, duplication, sediment,
sprawl, no-op or negation-only.

## Local histories

Analyse session history only when the request names the roots and the window.
Read it in place, keep excerpts in run-owned scratch, never commit raw
transcripts, and report aggregates or paraphrases. Classify each user
correction as an existing-rule miss, a trigger miss, weak steering, a missing
procedure or a local preference; only the middle three justify skill text.

## Output

A compact table of dimension, finding, evidence and confidence, then P0, P1 and
P2 findings. A suggested description change names the confusion it fixes.
