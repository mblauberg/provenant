# Routing & tiers

> `config/model-routing.json` is the dated machine catalogue; `scripts/model-route`
> is the policy resolver. This file owns human-readable family/role and
> degradation policy. `HARNESS.md` keeps only the invariant core.

The resolver's default `--adapter-gate fabric` fails closed when the selected
fabric adapter is disabled or has unresolved compatibility pins. A direct CLI
executor that owns its own safety and activation gates must opt in explicitly
with `--adapter-gate direct-cli`; this never bypasses family or model-pattern
constraints.

Route every dispatch by **task class, role, evidence surface, safety requirement,
and capability tier**. Never route by a memorised model name. Discover current
model IDs and effort modes at runtime (`cli-headless.md`) and retain the route
receipt.

| Task class | Bound role | Default tier | Default effort | Typical work |
|---|---|---|---|---|
| `mechanical` | worker | scout | low | search, extraction, formatting, deterministic checks |
| `legwork` | worker | workhorse | medium | ordinary implementation, analysis, drafting, source mapping |
| `critical-review` | critical-review | flagship | high | hard review, adversarial verification, design judgement |
| `orchestration` | orchestrator | flagship | high | decomposition, adjudication, synthesis |

`scripts/model-route resolve --task-class ...` is authoritative for these
defaults. An explicit role override may raise effort; an unavailable effort may
substitute only when the receipt records requested and effective values. Alias
routing remains a compatibility surface. Chair inheritance is exceptional: it
must be explicit and recorded, never inferred from an omitted binding.
Task-class dispatch rejects mismatched roles and requires a fresh, adapter-bound
runtime snapshot. Codex snapshots verify model availability and supported effort.
Account-default transport omits the literal model, so receipts retain policy
identity. Claude's no-tools, no-session subscription canary verifies the effective
model but records requested effort as unverified, so task-class dispatch fails
closed. It rejects caller-authored source labels without scrubbed provenance.
Canaries cost a little; reuse them only within the router's five-minute freshness
window.

## Tiers (relative, family-agnostic)

| Tier | Use for | Reasoning effort |
|---|---|---|
| **scout** | bounded, objective work: extraction, classification, formatting, schema/grep checks, first-pass scouting | low |
| **workhorse** | research legwork, drafting, ordinary review, diff analysis, source mapping | medium |
| **flagship** | sparingly: decomposition, final synthesis, resolving disagreements, hard/high-stakes calls | high |

Current durable aliases (verify against runtime before execution):

| Family | flagship | workhorse | scout |
|---|---|---|---|
| Claude | Opus | Sonnet | Haiku |
| OpenAI GPT-5.6 | Sol | Terra | Luna |

Opus is Claude's default flagship and high-effort critical reviewer. Sonnet is
the low-to-high workhorse. Each catalogue-configured risk tier has one bounded
override occupant. Validation prevents it from being an alias or alias
candidate. Callers must select the override explicitly and stay within that
tier's configured roles, alias and effort ceiling. Retargeting a tier removes
that tier's special treatment from its former occupant; a model no configured
tier names is no longer override-only. A malformed override block fails the
whole family closed — every route on that family is rejected with
`risk_tier_config_invalid` rather than quietly leaving its occupant
dispatchable. Fable currently occupies both configured tiers. Sol leads for
Codex. Eligible Sol lead/orchestrator routes may use Ultra; runtime model
capabilities decide the effective effort and every fallback is recorded. Claude
and Codex are equal primary families.

Effort rule: **medium by default**; **high for verification, adversarial, and high-stakes** calls
(that's where subtle errors hide); reserve the very highest effort for isolated single-shot calls —
it can be slow and has been observed to hang inside agent loops, so don't run it in a tight loop.

Cost is not just tokens: tools meter differently (tokens vs credits vs monthly caps), and the highest
tiers cost far more per call. A "small, objective" task only stays cheap if the schema is strict and
the output is validated — a loose schema lets a cheap model invent fields, which costs more in rework.

**Effort-controllable flagship can substitute for a mid model** when the CLI exposes effort control and
the cost/latency is acceptable. Treat this as a dated local heuristic, not doctrine. Smoke-test important
routes against the task before relying on them.

## Roles → where each tends to fit (dated heuristic — re-check, don't worship)

These are tendencies, not laws; confirm with a quick task-local smoke test before routing something
important.

- **Orchestrate / synthesise / decide** → flagship, in your own session.
- **Research legwork, drafting, review** → mid, in parallel, write-to-file.
- **Adversarial red-team** → a strong *different* family. Alignment-tuned models tend to be weaker at
  *playing* an attacker; a strong code/agent family with an explicit defect taxonomy tends to be a
  sharper critic. Give it the artifact + a checklist of failure types.
- **Long-context audit** → a long-context family (e.g. Gemini through Agent Fabric), then bring the distilled
  result back to a flagship for the decision.
- **Cheap bulk / scouting** → cheapest diverse family (kiro's open models), objective fields only.

## Default stack & fallback chains

Default once this skill triggers: **native same-harness workers + at least one different-family
verifier/adversary** when a safe, data-authorised route exists. For high-stakes or low-oracle work, use
two different-family passes where practical. Prefer the safest adapter that can inspect the needed
artifact. If no safe external adapter is available under the host data policy, use objective local
checks and record `CROSS-FAMILY-NOT-RUN` instead of pretending it happened.

In dynamic workflows, bind every stage from its task class. Route bulk
scan/extract stages to scout and reserve flagship for synthesis/adjudication
(`dynamic-workflows.md`).

## Adaptive review topology

Review pressure follows the current `HARNESS.md` ladder. Substantial work uses
multiple targeted lenses and one strong other-primary review. Targeted lenses
may use smaller models; their briefs must differ by failure surface. Crucial work
uses a distinct family when available. Terminal work adds stronger targeted and
adversarial pressure, and any skipped distinct-family leg records its reason.

The chair schedules these legs under a per-run configurable concurrency ceiling
and may sequence them around deterministic checks. It need not wait for a
particular model. A missing distinct-family leg requires an omitted-leg reason;
the other-primary leg remains required at substantial and above. Overlap creates
defect pressure, not votes; objective checks and source evidence remain
authoritative.

Express chains by **role → tier/family**, resolving names at runtime:

```
verify      → safest different-family read-only adapter → objective checks → user if needed
adversary   → strong different-family critic → source/test-backed fix list
long-ctx    → long-context scout → file-backed synthesis → flagship decision
bulk/scout  → cheap diverse scout → strict schema → sampled verification
```

On an auth/quota/limit/safety error from a tool: log it to the run scratchpad and advance to the next
entry. Never silently skip the verification step.

Cursor, Copilot, Kiro, Agy and Pi are adapters, not model families. Record the
actual provider/model lineage. Gemini, xAI and other distinct families are
flexible advisory workers/reviewers: useful for blind spots, never load-bearing
when quota/API output is absent. Pi stays dormant until a pinned distinct
open-model route, current Herdr integration and smoke evaluation exist; it may
not broker Claude/OpenAI and claim distinct-family certification.

Provider-subscription preferences are deliberately small and explicit. Agy is
Gemini-first and optional/advisory until its Fabric route has repeatable
subscription-backed evidence. Cursor prefers xAI/Grok first, then Composer.
If neither is available, route an explicit recorded fallback through the
family's native Claude, Codex or Agy adapter; do not rebroadcast that family
through Cursor unless its current compatibility contract admits it. The
ordered machine policy is in `config/model-routing.json`; it never authorises
automatic retries or silent substitution.

## Diversity caveat

Frontier families increasingly make *correlated* errors, so "ask another model" is weakening on its
own. Lean on **objective/locally-checkable verification** and (in non-code domains) source-anchoring +
action-authority gates, with cross-family review as pressure on top — not as the sole safety net.
