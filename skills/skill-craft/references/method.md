# Skill-audit method

Run every dimension. `N/A` is the correct result when the evidence is absent.

| Dimension | Evidence and interpretation |
|---|---|
| Trigger selection | Selected opportunities / valid opportunities from an authorised routing eval, structured attribution or explicit invocation. Loading is not selection. |
| User correction | User corrections / provenance-valid user episodes from the requested local scope. Separate new scope, preferences, approvals and machine-origin text; do not export raw free text without disclosure authority. |
| Workflow completion | Receipt/checkpoint stages reached / required stages; do not search ordinary prose for step headings. |
| Static quality | Valid frontmatter, clear trigger/exclusion, early critical rules, safe YAML, portable references, compact body and progressive disclosure. |
| Overtrigger | Balanced negative cases selected incorrectly. |
| Undertrigger | Balanced positive cases missed. Production undertrigger is unknown without an opportunity denominator. |
| Cross-skill conflict | Confusion pairs with overlapping triggers, contradictory authority or competing completion gates. |
| Environment | Referenced paths, tools, adapters and platform assumptions verified live. |
| Token economics | Always-loaded metadata and triggered body cost against evidenced use and information gain. |
| Catalogue exposure | Canonical and provider-rendered characters/tokens, omitted or truncated skills, duplicate triggers and sidecar drift. |
| Supply chain and authority | Source/version/licence, scripts/hooks/binaries, MCP/network/data flows, state writes, approval inheritance, update and rollback. |

## Scoring

Use a five-point score only when the evidence supports it. Suggested weights:
selection 25%, corrections 20%, completion 15%, static quality 15%,
undertrigger 15%, token economics 10%. Redistribute missing dimensions and
show which were excluded. Overtrigger, conflict and environment remain
qualitative unless a versioned dataset defines a denominator.

## Static checks

- frontmatter contains only `name` and `description` and is under 1024 chars;
- name is kebab-case and the first 250 description characters carry the main
  trigger and exclusion;
- `SKILL.md` is at or below 500 words under the canonical whole-file counter,
  including YAML frontmatter; the skill-craft audit reference has a separate
  600-word ceiling under the same counter; stable depth belongs in references;
- directives are concrete bright-line rules rather than repeated emphasis;
- positive, negative and boundary fixtures cover adjacent skills;
- paths and CLIs exist or are capability-gated; and
- narrative history and dated change logs are absent from the operational body.
- the complete canonical/provider-rendered catalogue stays within its documented
  budget and proves which skills were exposed; and
- third-party provenance, licence, executable surfaces and data flows are
  inventoried without running untrusted installers or hooks.

## Research basis

- [Memento-Skills](https://arxiv.org/abs/2603.18743): skill retrieval is
  load-bearing for stored procedural knowledge.
- [MCP description quality](https://arxiv.org/abs/2602.18914): descriptions
  materially affect selection.
- [Lost in the Middle](https://aclanthology.org/2024.tacl-1.9/): position
  affects model use of long context.
- [IFEval](https://arxiv.org/abs/2311.07911): multi-constraint instruction
  following needs explicit evaluation.
- [Anthropic agent evals](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents):
  balance positive and negative cases, separate capability and regression
  suites, and measure the model plus harness over repeated trials.

These sources motivate tests; they do not make a description universally
optimal. Prefer local held-out evidence over generic scoring folklore.

## Local and shared evidence

For requested personal/local analysis, inspect source history in place and keep
any excerpts in local run-owned scratch. A direct request is sufficient
authority; do not add a second receipt or redaction ceremony. Historical data
from an older skill contract may inform harness habits but cannot score a new
skill. Mark unsupported attribution and insufficient denominators `N/A`.

A compact aggregate or paraphrased report to the requesting user in the same
authorised session is local delivery, not export. For a persistent
repository/shared artifact, raw cross-provider handoff, new audience or
external destination, obtain separate authority and confirm permitted content
with the user. Minimise authorised exports to aggregates or paraphrases. Never
commit raw provider history, secrets or third-party private content.
