# Evaluation & observability

Static doctrine checks are not enough. Multi-agent systems are path-dependent: two valid runs can use
different workers, tools, and intermediate steps. Evaluate outcomes, safety gates, and trace quality.

## Eval layers

1. **Static guards.** Frontmatter parses, trigger fixtures and links exist,
   sidecars are valid, and scripts parse.
2. **Pressure scenarios.** Give a fresh agent realistic prompts and inspect whether it uses the skill,
   scopes workers, prevents shared writes, and verifies before finalising.
3. **Trace review.** For real runs, inspect worker count, duplicate work, unsafe tools, missing sources,
   unresolved disagreements, and whether final claims trace to artifacts.
4. **Outcome checks.** Tests pass, citations exist and support claims, arithmetic reconciles, files changed
   only where authorised, final state matches the request.
5. **User review.** Required for high-stakes, irreversible, or low-oracle domains.

## Trace fields to record

Use the run manifest or final synthesis to capture:

```
task_id
date
orchestrator
workers: role / family / model-or-cli-version / scope / output_path
tools_used
objective_checks
cross_family_checks
failovers
disagreements
final_decision
user_authority_gate
```

## Behaviour test cases

Keep a small suite of realistic prompts:

- "use several subagents and web searches to be sure"
- "use many subagents and deep research; review the work multiple times"
- "spawn one Codex subagent per source slice, wait for all, then summarise"
- "use Claude ultracode style review-refine loops"
- "independent second opinion, read-only"
- "use subagents in waves"
- "use cross-family models like Claude and Gemini alongside native subagents"
- "fix this typo" (must not trigger)
- "edit these two files in parallel" (must partition or refuse shared writes)

Expected trace checks: native same-harness workers before same-family CLI, `CROSS-FAMILY-NOT-RUN`
recorded when external verification is unavailable, adversarial cross-review before final synthesis, and
`FINAL_GATE.md` completed or explicitly failed.

## Judge-bias checklist (when a worker or panel scores outputs)

LLM-as-judge steps — panels, review-refine loops, pairwise picks — carry systematic biases. Check and
mitigate each before trusting a score:

| Bias | Symptom | Mitigation |
|---|---|---|
| **Position** | first-listed candidate wins | evaluate twice with positions swapped; majority/consistency check, else TIE |
| **Length** | longer answer scores higher regardless of quality | instruct to ignore length; length-normalise; validate on length-controlled pairs |
| **Self-enhancement** | judge favours outputs from its own model/family | use a different family to judge than generated (cross-family verifier) |
| **Verbosity** | excess detail reads as quality | criteria-specific rubric that penalises irrelevant detail |
| **Authority** | confident tone scores higher than accuracy | require evidence/citation; add an objective fact-check layer |

Ask judges for locations, evidence, and fixes — not a global score alone — and prefer objective checks
(tests, source anchors, arithmetic) over any judge verdict on high-stakes findings.

## Research anchors

- OpenAI agent evals and trace grading: evaluate agent workflows through traces, graders, datasets, and
  eval runs.
- Anthropic multi-agent research system: start with small eval sets, inspect traces, use source/citation
  criteria, and monitor emergent coordination failures.
- Anthropic managed agents: long-running agents need recoverable session state and stable interfaces
  because harness assumptions drift.
