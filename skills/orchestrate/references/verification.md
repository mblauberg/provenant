# Cross-family verification

Cross-family review works because different families miss different things, so an outside judge catches
errors a self-check won't. That gain is **decorrelation pressure, not magic** — and it is fragile.
Protect it.

## Why self-verification is weak

A model self-grading shares the latent cause of its own error (hallucinated premise, missed constraint,
"house style"). It also exhibits **self-preference** — it favours outputs that look like its own. A
different family removes that specific bias. (The bias is real but smaller than folklore; treat the
effect as "may reduce", not "removes".)

## The four rules

1. **Different family, always.** Never let the generating model be the final judge of its own output.
2. **Independence is earned, not assumed.** Two things silently re-correlate cross-family errors:
   - **Shared context / scratchpad** — if the verifier reads your reasoning, it inherits your framing.
     Give it the **artifact only**, plus the minimum it needs, not your scratchpad.
   - **Shared prompt template** — identical phrasing pushes different models toward the same answer.
     **Vary the framing** of the verification prompt.
   When it matters, **measure** overlap: run two families on a small set and check how often they agree
   *and are both wrong*. If overlap is high, the second opinion is buying little.
3. **Scout ≠ veto.** Match judge competence to task difficulty. A weaker/cheaper cross-family model may
   *flag* candidates for review, but must **never override** a stronger model's nuanced output — on hard
   items a weak judge throws false rejections of correct work. Escalate real disagreement to a strong
   judge or to ground truth.
4. **Objective checks beat taste.** Prefer locally-checkable verification — does the cited source exist
   and say that? does the test pass? does the number reconcile? does the quote match? — over holistic
   "is this good?". Holistic judging is the most bias-prone and adversary-fragile mode.

## How to run a verification pass

1. Isolate the **artifact** (the claim, draft, diff, or answer) and its **checkable anchors**
   (sources, tests, figures).
2. Pick a verifier family + tier by role (`routing-and-tiers.md`); auth-preflight it.
   If no safe/data-authorised different-family route exists, write
   `CROSS-FAMILY-NOT-RUN: <missing tool | auth/quota | data policy | no read-only mode | user constraint>`
   to the run traces and final gate.
3. Send a **clean, minimal** prompt: the artifact, the specific checks, and a defect taxonomy
   ("look for: fabricated citations, unsupported numbers, missed constraints, logic gaps…").
   Ask for a ranked list of problems with location + fix, and an explicit "what it gets right".
4. For high-stakes work, use **two** different families (a small jury) when cost/data policy allow it.
   Don't simple-majority-vote — weigh by competence and by whether a flag is objectively checkable.
5. **Beware debate convergence:** multi-round debate can settle on a confident *falsehood* if the
   models share a blind spot or the judge rewards confidence. Keep an objective anchor in the loop.
6. Fold corrections; record unresolved disagreements in the scratchpad for a user decision.

Ordinary workspace material inside the chair's approved authority envelope may
use any approved provider-family route. Credentials, secrets, platform rules,
and narrower task or project prohibitions still control. Record the effective
disclosure route; private memory cannot grant or widen it.

A source-read-only reviewer leaves reviewed source and working state unchanged.
A separate bounded capability may authorise only a named, namespaced review
artifact and correlated Fabric request/reply communication. Verify both the
source no-write boundary and the artifact path after the run. A failed, missing,
or null worker result does not prove that no partial write occurred; inspect and
preserve its owned paths before reassignment or cleanup.

For debate, panel, and sparse-communication patterns, use
[Debate, panels, and cross-family judgement](#debate-panels-and-cross-family-judgement)
below.

## Anti-patterns

- Same model (or same family) verifying itself.
- Pasting your full chain-of-thought to the "independent" verifier.
- Letting a cheap scout veto the flagship.
- Treating a single cross-family thumbs-up as proof on a high-stakes, non-objective claim.
- Majority-voting hallucinations as equal to reasoning.

## Debate, panels, and cross-family judgement

Use panels to create independent defect pressure. Do not treat panel agreement as ground truth.

### Patterns

| Pattern | Use for | Risk |
|---|---|---|
| Independent reviewers | broad defect discovery, source audits, security review | duplicate work without scoped briefs |
| Small jury | high-stakes judgement where no single checker is enough | false confidence from correlated errors |
| Adversarial red-team | finding weaknesses, attack paths, overclaims | over-criticising correct work |
| Debate | clarifying disagreements with limited turns | convergence on confident falsehoods |
| Sparse topology | many reviewers where communication cost matters | missed synthesis if no strong judge resolves |

### Rules

- Give reviewers the artifact and check rubric, not your scratchpad.
- Vary prompt framing across families to reduce correlation.
- Ask for locations, evidence, and fixes, not global scores alone.
- Let weak/cheap models scout; do not let them veto stronger grounded work.
- Escalate real disagreement to objective checks, a stronger judge, or the user.
- Prefer sparse, bounded communication over open-ended debate loops.
- Independent agreement is not verification. For load-bearing rule, statute or spec text, N
  agents agreeing from secondary reads never substitutes for one read of the primary text, and
  limb, division and jurisdiction splits within one instrument are a recurring trap. Apply a
  repo-wide correction only after the primary-text check.

### Vote handling

Use voting only for low-stakes or objective candidate filtering. For high-stakes findings, require at
least one of:

- direct source support;
- reproducible command/test;
- exact quote/location;
- arithmetic/schema reconciliation;
- user decision.

### Evidence council

Karpathy's LLM Council describes three stages: independent first opinions,
anonymised peer ranking, then a chairman synthesis. Retain the independence,
identity masking and chair; replace global ranking with claim-level evidence
adjudication. This is an independent workflow design; no LLM Council code is
incorporated.

1. **Blind first pass.** Each reviewer receives the same source packet plus a
   distinct lens. It cannot see other answers. Store each output separately.
2. **Anonymised cross-examination.** Strip provider/author identity, randomise
   order, and give reviewers compact claim packets. They mark each claim
   `supported`, `contradicted`, or `needs-evidence`, with a falsification check.
   They do not score prose quality or vote on truth.
3. **Chair reduction.** The accountable chair deduplicates findings, checks
   anchors, runs objective falsifiers, preserves material dissent and emits one
   evidence map. The chair may reject the entire council result.

Do not let a reviewer judge its own authored surface. Avoid showing all raw
responses to every model when claim packets suffice; this reduces anchoring and
context cost. Use a fresh-context reducer for crucial decisions. A council adds pressure, not authority:
deterministic gates and user one-way-door decisions still win.

### Panel presets and receipts

The council above is one shape of a general mechanism. A panel is a chair-run
reduction over review records the chair already dispatched — it spawns nothing
of its own. `skills/_shared/review_panel.py` is its machine-readable owner.

| preset | distribution | reduction | minimum completed |
|---|---|---|---|
| `council` | `shared` — every member answers the same question | `agreements-conflicts` | 3 |
| `breadth` | `split` — members take disjoint scopes | `union` | 1 |

Resolve the named preset, then override any of the four axes — membership,
distribution, reduction, degradation. There is no other precedence, and no
automatic model or provider routing.

Record each member as an ordinary review with `adapter_gate` set to `fabric` or
`direct-cli`, put those review ids in `spec.membership`, and fill
`result.output` through the declared reducer. `council` keeps `agreements`,
`conflicts` and `minority_views` separate: do not score, weight, average or
vote-count them, per the vote-handling rule above. `breadth` records a
deduplicated `findings` union.

A member whose status is `failed`, `unavailable` or `omitted` does not count
towards the minimum. Falling below it records an explicit shortfall rather than
failing — a panel is advisory and never blocks.

### Research anchors

- LLM-as-judge and self-preference literature: same-model or same-family judges can favour outputs that
  look like their own.
- Multi-agent debate studies: debate and voting can improve some tasks, but communication topology and
  judge quality matter; majority voting can amplify shared hallucinations.
- Google Research debate-topology work: sparse communication can improve debate efficiency and reduce
  unnecessary message flooding.
- Andrej Karpathy, `karpathy/llm-council`: independent opinions, anonymised
  peer review/ranking and a chairman synthesis. The harness deliberately
  replaces ranking with evidence adjudication.
