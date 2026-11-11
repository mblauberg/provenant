# Claims, numbers and condensing

## Claim classes

Classify each claim before editing, then pick a verb that fits its class and
hold it there: `suggests` never becomes `proves`, a pilot never becomes final
evidence, a specified extension never becomes a completed result.

| Class | Means | Wording |
| --- | --- | --- |
| observed | measured, run or reproduced | `measured`, `recorded`, `reproduced` |
| inferred | supported, not measured | `suggests`, `likely`, `is consistent with` |
| designed | what the code or protocol requires | `requires`, `rejects`, `caps` |
| limitation | not measured, tested or included | `was not measured`, `remains untested` |
| planned | specified, not built or run | `is left for future work` |
| background | prior work | cite it |
| pending | placeholder, result macro, unverified artefact | `is pending`, `[FLAG: verify]` |

Observed and inferred stay visibly apart: `CPU sat at 100% for 4 minutes` is
observed; `the regex likely backtracks` is inferred and says so.

**Tense trap.** Plain present tense silently asserts unbuilt work is finished
(`The service handles failover`; `The method is evaluated on the held-out
set`). Where work is unbuilt, untested or unverified, scope the claim to the
design or state its status. Keep pending and pilot evidence visibly pending.

**Comparatives carry their number.** Never a bare `faster`, `better`,
`improved` or `more scalable`: `cold start dropped from 3.1 s to 0.9 s (M2,
cache warm)`. Name the interval or test behind `significant`, and don't use it
as a pass/fail verdict.

**Contribution and limitation claims** name their evidence and bound. Weak:
`The study has some limitations.` Better: `The evaluation does not establish
field performance because all outcomes were measured in a controlled setting.`
Future work is concrete, not `explore more advanced approaches`.

## Mechanised number audit

In a number-heavy document a style pass drifts a figure or drops a qualifier
and a reread misses it. Keep a script that holds each number-bearing sentence
as a literal, recomputes its values from the source data and fails when either
changes. Rerun after every pass. A failure on reworded text is the audit
working; update the literal only after confirming the number and claim
survived.

## Condense pass

Shortening, not improving. Agree any target; don't force a cut on tight text.

1. Count prose words (not macros, labels, code or frontmatter) as the baseline.
2. Lock invariants: facts, numbers, units, identifiers, commands, flags,
   dates, citations, defined terms, obligations.
3. Write one phrase per paragraph. A paragraph that resists a one-line
   summary, or repeats another, is the cut or merge target.
4. State each fact, caveat, definition and number once, in its home section;
   cross-reference the rest. Moving detail to an appendix is not losing it.
5. Cut throat-clearing, recap closers and re-explanations. Narrow a sweeping
   claim to what the evidence supports; never soften an honest negative.
6. Replace padding phrases directly: `In order to X` becomes `To X`, `The
   fact that X` becomes `X`, `is able to` becomes `can`.

**Stop rule.** Stop when a cut would remove a fact, number, unit, caveat,
scope condition, honest negative or defined term, break a cross-reference or
force awkward phrasing. When unsure, keep it, and compress rather than delete
wherever a fact is involved. Already tight: say so.

**Verify.** A drafting model's own claim that nothing was dropped is not
evidence. Set-diff the numbers, identifiers, commands, dates and defined terms
of before and after: zero unexplained loss. Then have an independent read ask
whether any condition, caveat or obligation is now weaker, unowned or
unfindable. After moving a constraint, re-read what depended on it. Restore
any gap from the before-text, never from memory. Report words before and
after and that the locked list survived.
