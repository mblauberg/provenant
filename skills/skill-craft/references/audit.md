# Audit branch

Read-only assessment of an existing skill. Report evidence; never edit. The
working rules in [../SKILL.md](../SKILL.md) apply. If the request also asks for
a revision, the action-owning skill stays primary and this branch is its
companion.

## Check

Run every dimension. Mark missing evidence `N/A`; an unsupported result is
never zero.

| Dimension | Evidence |
|---|---|
| Trigger selection | Selected over valid opportunities, from a frozen routing eval or explicit invocation. Loading a skill is not selection. |
| Overtrigger | Balanced negatives selected in error. |
| Undertrigger | Balanced positives missed. Production undertrigger needs an opportunity denominator. |
| Cross-skill conflict | Confusion pairs with overlapping triggers, contradictory authority or competing completion gates. |
| Workflow completion | Checkpoint stages reached over required stages. |
| Static quality | Frontmatter holds only `name` and `description`. The first 250 characters carry the trigger and exclusion. Critical rules come early. The body is near 500 words, with depth in references. No dated history in the body. |
| Environment | Referenced paths, tools and CLIs exist, or are capability-gated. |
| Token cost | Always-loaded metadata and triggered body, against evidenced use. |
| Catalogue exposure | Catalogue characters, truncated skills and duplicate triggers. |
| Supply chain and authority | Source, version and licence. Scripts, hooks, binaries and network use. State writes and approval inheritance. |

Report each dimension as a finding, not a weighted score. Overtrigger, conflict
and environment stay qualitative unless a versioned dataset gives a
denominator.

Check static facts with the commands the skill names, not by reading alone.
Confirm that `scripts/check-skill-negatives` passes for the skill.

## Local histories

Analyse session history only when the request names the roots and the time
window. Read it in place, keep excerpts in run-owned scratch, and never commit
raw transcripts. Report aggregates or paraphrases. A persistent shared
artefact, raw excerpts sent to another provider, or a new audience needs a
separate grant.

Classify each user correction as an existing-rule miss (a fixture or check),
a trigger miss, weak steering, a missing procedure or a local preference. Only
trigger misses, weak steering and missing procedures justify skill text.

## Output

Return a compact table of dimension, finding, evidence and confidence. Then list
P0, P1 and P2 findings. Every behavioural rate gives its numerator,
denominator, source, window and limitations. A suggested description change
names the confusion case and a balanced regression fixture.

Do not cross into authoring from here. A fix needs its own grant under the rule
in [../SKILL.md](../SKILL.md).

## Research basis

Dated, most recent first. Weigh the 2026 sources above the older ones.

- [MCP description quality](https://arxiv.org/abs/2602.18914), February 2026:
  standard-compliant tool descriptions were selected 72% of the time in
  competitive settings, against a 20% baseline. Supports front-loaded
  descriptions.
- [Memento-Skills](https://arxiv.org/abs/2603.18743), March 2026: stored skills
  act as memory, and a router selects the relevant one. Supports fixtures that
  test selection, not prose quality.
- Anthropic's agent evaluation guidance (undated in this repository): balance
  positive and negative cases, separate capability and regression suites, and
  measure the model plus harness over repeated trials.
- [Lost in the Middle](https://aclanthology.org/2024.tacl-1.9/), 2024, possibly
  stale: model use of long context varies with position. Supports keeping hard
  rules early.
