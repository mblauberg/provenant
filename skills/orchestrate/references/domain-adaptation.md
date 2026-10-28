# Cross-domain adaptation

The orchestration loop is domain-agnostic, but **what counts as verification changes** when you leave
code.

## Code: you have an oracle

Tests, compilers, type-checkers, linters, and runtime behaviour are ground truth. Use them as the final
judge. Cross-family review is a *supplement* that catches design/logic issues tests miss, not a
replacement for running the tests.

## Non-code (research, admin, legal, analysis): no oracle

With no test suite, **wrong output is invisible**: it's plausible, fluent, and throws no error.
Verification must be **built into generation**, not bolted on after. Substitute these for the missing
compiler:

- **Source-anchoring.** Every non-trivial claim traces to a dateable, retrievable primary source
  (document + locator). Verify the source **exists and actually says it**: retrieval-grounding reduces
  but does **not** eliminate fabrication, and a cited source is sometimes chosen *after* the answer
  (post-hoc rationalisation). Check the citation, not just the vibe.
- **Separate the generator from the checker.** Have a different agent/family attack the draft (e.g.
  skeptic → researcher → judge), so one agent's bias doesn't certify itself.
- **Rubric/checklist judging** is useful but **fragile under adversarial or contested inputs**: never
  the sole layer; pair it with objective source checks and cross-family review.
- **Quantify uncertainty.** Prefer "CANNOT-VERIFY" over an overstated confirmation when a source
  couldn't be fetched.

## Action-authority gates (high-stakes / irreversible)

Some actions can't be undone: filing, serving, sending, publishing, signing, disclosing, committing,
deploying. The orchestrator and its agents **prepare and verify**; a user authorises the irreversible
step. Respect the **host project's** authority rules; do not perform a gated action without the exact
authority that project grants for that artifact. Encode any "must NOT do" as a `prohibited-action` flag
in the handoff record (`layering-and-context.md`).

## Data-sensitivity (external provider routes disclose content)

Sending content to an external-family CLI discloses it to that provider. **Follow the host
project's / operator's data policy** for what may leave the machine, and decide this *before*
dispatch, not after. Absent an explicit policy, treat confidential or sensitive content as
**do-not-send** until the operator authorises it (per artifact or category). If the operator has set
a no-gating policy, respect it. This is a per-project choice; never bake one project's policy in as a
universal default.

## Tuning the loop by domain

- **High-stakes, low-oracle (legal, medical, finance):** more cross-family verification, strict
  source-anchoring, user action-gates, conservative confidence.
- **Exploratory research:** breadth-first parallel scouts → synthesis → one verification pass.
- **Code:** tests as oracle first; agents for breadth and design critique.
