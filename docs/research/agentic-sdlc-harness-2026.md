# Agentic delivery harness: 2026 research synthesis

Acceptance policy note (21 October 2026): historical human-acceptance wording
below records the research baseline. Current delivery acceptance is met by
objective checks and tier-required review; explicit user acceptance applies
only to `terminal` work or approved scope. A project may name its landing gate
as acceptance. External-action gates remain.

Status: Research baseline
Evidence cut-off: 10 July 2026
Review cadence: quarterly, and whenever a primary model, provider interface or
material security standard changes

Implementation status (11 July 2026): this document is the pre-implementation
baseline. Its 30-skill inventory and ten material gaps are historical, not the
live backlog. The current 32-skill catalogue is generated in the
[skill catalogue](../../skills/); closed gaps, dated
evaluation evidence and later consolidations are explained in
[Agent skill portfolio: 2026 research and refactor](skill-portfolio-practices-2026.md).

## Executive finding

The harness already implements the strongest common pattern across current
guidance: humans set intent and consequential gates; agents execute inside an
authority envelope; deterministic checks and independent review precede human
acceptance; production evidence returns to the next cycle. Its software path is
materially stronger than its paths for research, documents and other
evidence-bearing work.

The next design should not add another monolithic workflow. It should introduce
one domain-neutral delivery kernel with profiles, safeguards, typed evidence
and machine-checkable receipts. Software remains one profile. Research,
analysis, document and agent-product profiles substitute their own artifacts,
graders and release semantics; high-stakes safeguards compose across them
without weakening shared authority, review and improvement contracts.

The other major gap is proof that the harness itself improves. `retrospect`
describes the right flywheel, but comparable baselines, recurrence and promoted
changes are not yet machine-checkable. Skill telemetry also needs an explicit
privacy boundary: private transcripts must not become the default analytics
database for a public, cross-project harness.

## Research boundary and method

This report combines:

- a live structural audit of the repository's 30 global skills, runtime
  constitution, routing, run receipts, installers and tests;
- the supplied Google/Kaggle May 2026 whitepaper, checked from the local PDF;
- current primary publications from OpenAI, Anthropic, Google, AWS, Microsoft,
  GitHub, NIST and OWASP; and
- synthesis limited to practices supported by more than one source or by a
  clear local failure mode.

The sources address two related systems. Coding-harness guidance concerns an
agent doing project work. Agent-lifecycle guidance concerns an AI agent being
built and operated as a product. This repository must support both without
confusing their gates: a delivery receipt is not a production-agent trace, and
a coding review is not an agent-behaviour evaluation.

## Source matrix

| Source | Date | Strongest applicable guidance | Harness consequence |
|---|---:|---|---|
| [Google/Kaggle, *The New SDLC With Vibe Coding*](https://www.kaggle.com/whitepaper-the-new-SDLC-with-vibe-coding) | May 2026 | Move from ad-hoc prompting to agentic engineering; treat context, tools, hooks, sandboxes, subagents and observability as the harness; use a benchmark → diagnose → optimise → regress → monitor flywheel. | Keep a human specification/review boundary, make the flywheel measurable, and version the research assumptions because the paper expects rapid change. |
| [OpenAI, *Harness engineering*](https://openai.com/index/harness-engineering/) | 11 Feb 2026 | Repository knowledge is the system of record; keep `AGENTS.md` short; use progressive disclosure, structural tests, observability and recurring garbage collection. | Retain the small bootstrap, move depth to skills/references/scripts, test architectural rules and make context hygiene a first-class maintenance gate. |
| [OpenAI, *Symphony*](https://openai.com/index/open-source-codex-orchestration-symphony/) | 27 Apr 2026 | Durable orchestration rests on an agent-friendly repository, automated guardrails and explicit work state rather than chat alone. | Keep paired communication as transport while receipts and project artifacts remain authoritative. |
| [Anthropic, *Effective harnesses for long-running agents*](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents) | 26 Nov 2025 | Give fresh contexts a clear initializer, durable progress state, incremental work and end-to-end checks. | Preserve session/handoff/work-map layering and require resumable checkpoints for genuinely long runs. |
| [Anthropic, *Demystifying evals for AI agents*](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents) | 9 Jan 2026 | Evaluate model plus harness; grade outcomes and trajectories; use repeated trials, balanced cases, mixed graders, separate capability/regression suites and production monitoring. | Add profile-specific outcome graders, trajectory assertions, repetitions for stochastic cases and held-out trigger/behaviour suites. |
| [Google Research, *Towards a science of scaling agent systems*](https://research.google/blog/towards-a-science-of-scaling-agent-systems-when-and-why-agent-systems-work/) | 28 Jan 2026 | Multi-agent systems help parallelisable tasks and can harm sequential work; architecture should follow task structure. | Orchestration remains proportional, with one writer and no automatic council for tightly coupled work. |
| [Google Cloud, *A developer's guide to production-ready AI agents*](https://cloud.google.com/blog/products/ai-machine-learning/a-devs-guide-to-production-ready-ai-agents) | 25 Feb 2026 | Production agents require adapted testing, context/memory, secure tools, staged deployment and persistent observability. | Add an agent-product profile rather than embedding production-agent assumptions in every delivery. |
| [AWS, *Evolving software delivery for agentic AI*](https://docs.aws.amazon.com/prescriptive-guidance/latest/strategy-operationalizing-agentic-ai/software-delivery.html) | Current 2026 guidance | Define zones of intent: outcome, rationale, constraints and trust boundaries; treat architecture as scaffolding, testing as behavioural evaluation and operation as continuous observation. | Make authority and success measures generic, and distinguish deterministic artifact checks from behavioural evals. |
| [AWS, *AgentCore Evaluations*](https://aws.amazon.com/blogs/machine-learning/build-reliable-ai-agents-with-amazon-bedrock-agentcore-evaluations/) | 31 Mar 2026 | Repeated runs are necessary; assess session, trace and tool levels; use code, ground-truth and model graders; connect offline and online evaluation. | Receipts need evaluator lineage, sampling/repetition and comparable pre/post evidence, not a single unqualified pass. |
| [AWS, *Architecting for agentic AI development*](https://aws.amazon.com/blogs/architecture/architecting-for-agentic-ai-development-on-aws/) | 26 Mar 2026 | Fast autonomous iteration needs loosely coupled architecture, rapid feedback and agent-legible codebases. | Continue structural checks and small interfaces; reject context-heavy prose as a substitute for executable feedback. |
| [Microsoft, ASSERT and Agent Control Specification](https://devblogs.microsoft.com/foundry/build-2026-open-trust-stack-ai-agents/) | 2 Jun 2026 | Translate policy into evaluation and controls at input, model, state, tool and output checkpoints; feed sampled production traces back into datasets. | Add typed checkpoints, policy/eval evidence and a controlled observation-to-regression path. |
| [Microsoft, agent governance maturity model](https://learn.microsoft.com/en-us/agents/adoption-maturity-model/maturity-model-security-governance) | May 2026 | Mature governance requires inventory, ownership, risk-tiered controls, monitoring and lifecycle management rather than isolated prompt rules. | Record owner, risk, authority and retirement/review state for harness components and deployed agents. |
| [IBM, *Agent lifecycle management*](https://www.ibm.com/think/topics/agent-lifecycle-management) | 23 Jun 2026 | Manage prompts, models, tools, memory, data, permissions, evals, incidents and decommissioning as one versioned system. | Profiles for agent products must cover provision, observe, incident response and retirement, not stop at release. |
| [GitHub, security validation for third-party coding agents](https://github.blog/changelog/2026-06-09-security-validation-for-third-party-coding-agents/) | 9 Jun 2026 | Agent-written code should receive CodeQL, dependency-advisory and secret-scanning checks regardless of provider. | Security evidence becomes a deterministic, risk-proportional gate rather than only a reviewer lens. |
| [OWASP Top 10 for Agentic Applications 2026](https://genai.owasp.org/resource/owasp-top-10-for-agentic-applications-for-2026/) | 9 Dec 2025 | Cover goal hijack, tool misuse, identity/privilege abuse, supply chain, code execution, memory poisoning, inter-agent communication, cascading failure and human-trust risks. | Threat modelling and validation must cover the agent's authority, memory and coordination plane as well as generated code. |
| [NIST SSDF 1.2 initial public draft](https://csrc.nist.gov/pubs/sp/800/218/r1/ipd) | 17 Dec 2025 | Integrate secure-development practices throughout the lifecycle and address root causes so vulnerabilities do not recur. | Retain supply-chain and vulnerability evidence through design, execution, review, release and retrospect. Treat the draft status explicitly. |

## Convergent principles

### 1. Intent is an executable boundary

Modern guidance does not remove specifications. It changes them from detailed
implementation recipes into an outcome, rationale, constraints, risks,
acceptance measures and authority envelope. The harness should fail closed when
a consequential design has no artifact, approver or evidence digest. Status
text alone is not approval evidence.

### 2. Outcome and trajectory are separate

A correct final artifact can be produced through an unsafe or unauthorised
trajectory. Conversely, an acceptable trajectory can fail the requested
outcome. Every profile therefore needs both:

- outcome evidence: tests, source coverage, render checks, rubric scores,
  signed decisions or observed state; and
- trajectory evidence: authority, tool/model lineage, disclosure, reviewer
  independence, degradations and human gates.

### 3. Deterministic checks precede judgement

Static checks are cheaper, reproducible and easier to audit than a model
review. Model or human judgement belongs where meaning, taste or uncertain
evidence genuinely requires it. Reviewer councils should use distinct lenses
and independent first passes, then evidence-based adjudication—not voting.

### 4. Parallelism follows decomposability

The harness is right to use native subagents and the other primary family, but
the number of agents is not a quality metric. Parallel workers need independent
questions, non-overlapping writes and compressed artifact returns. Sequential
reasoning stays with one owner. Gemini, xAI and other families are useful
advisory dissenters; provider failure cannot block the primary-family gate.

### 5. Context is curated infrastructure

Durable project knowledge belongs in concise project artifacts, not provider
transcripts or private memory. Logs are evidence stores with retention rules,
not documentation. The harness should measure stale state, duplicate truth,
oversized entrypoints and orphaned scratch data, but delete only artifacts with
clear ownership and expiry.

### 6. Improvement needs controlled evidence

The flywheel is incomplete until it records a baseline, a comparable run, a
root-cause cluster, an authorised change, a regression gate and recurrence in a
later cycle. Automatic self-modification would combine diagnosis, authority and
verification in one actor. The safer design is proposal-first: humans approve
material harness changes; deterministic and held-out evals test them.

## Current-state assessment

### Strong coverage

- concise bootstrap and deeper runtime constitution;
- equal Claude/Codex primaries with one accountable chair;
- risk-proportional native and cross-family review;
- non-blocking advisory families;
- model policy separated from adapter execution, with actual lineage receipts;
- software implementation, review, release and session hygiene;
- durable work maps, resumable autonomous labs and explicit human gates; and
- public-safety and harness contract tests.

### Material gaps

The numbered list below records the 10 July baseline. Do not re-open an item
without checking the live contracts and the implementation-status companion.

1. `implement` and its run validator assume Git revisions and source paths, so
   the supposed general lifecycle has no neutral execution contract.
2. A design gate can be marked approved without binding an artifact, digest,
   decision owner, approver or required risk analysis.
3. `retrospect` has no comparable-cycle receipt or validator.
4. `skill-audit` instructs agents to scan all provider transcripts and quote
   user messages without an explicit scope, redaction or disclosure gate.
5. Observation is mostly an immediate release check, not a defined evidence
   window with thresholds, incidents and retirement criteria.
6. Security is a review lens more often than deterministic SAST, dependency,
   secret, IaC and agentic-threat evidence.
7. Existing installations are skipped rather than reconciled through a
   versioned ownership manifest.
8. Instruction precedence is not stated identically across all entrypoints.
9. Trigger fixtures cover only a minority of skills and are not a held-out,
   repeated routing evaluation.
10. Several skills exceed the intended compact body and should progressively
    disclose reference material.

## Adopt, adapt, reject

Adopt typed intent, mixed graders, outcome/trajectory evidence, persistent
receipts, deterministic security validation, production-to-eval feedback and
periodic garbage collection.

Adapt enterprise fleet concepts to a personal global harness: use lightweight
local manifests and receipts, not a mandatory cloud control plane. Apply
repeated trials only to stochastic behaviour; deterministic scripts need one
reproducible execution. Make privacy-preserving aggregates the default and raw
trace access an explicit exception.

Reject autonomous harness mutation, majority-vote review, always-on
multi-agent fan-out, vendor-locked lifecycle semantics, raw transcript memory,
unbounded logs and claims of universal best practice without a dated evidence
baseline.

## Research maintenance contract

Each recommendation in the implementation specification names this evidence
cut-off. A quarterly `retrospect` pass should check source availability,
superseding editions, changed provider capabilities and local counter-evidence.
A source change opens a proposal; it does not silently rewrite policy. Draft
standards such as NIST SSDF 1.2 must be labelled and rechecked on finalisation.
Model names and runtime flags remain capability-discovered data, not research
claims.

## Complete primary-source register

The matrix above highlights the sources that most directly change this design.
The full research pass also used the following primary sources. Dates are the
publication or current-document dates visible when checked on 10 July 2026.

### Google

- Addy Osmani, Shubham Saboo and Sokratis Kartakis,
  [*The New SDLC With Vibe Coding*](https://www.kaggle.com/whitepaper-the-new-SDLC-with-vibe-coding),
  May 2026.
- Kimberly Milam, Antonio Gulli and Anant Nawalgaria,
  [*Context Engineering: Sessions & Memory*](https://www.kaggle.com/whitepaper-context-engineering-sessions-and-memory),
  updated May 2026.
- Lee Boonstra,
  [*Spec-Driven Production Grade Development in the Age of Vibe Coding*](https://www.kaggle.com/whitepaper-spec-driven-production-grade-development-in-the-age-of-vibe-coding),
  May 2026.
- Yubin Kim and Xin Liu,
  [*Towards a science of scaling agent systems*](https://research.google/blog/towards-a-science-of-scaling-agent-systems-when-and-why-agent-systems-work/),
  28 January 2026.
- Kanchana Patlolla and Anant Nawalgaria,
  [*A developer's guide to production-ready AI agents*](https://cloud.google.com/blog/products/ai-machine-learning/a-devs-guide-to-production-ready-ai-agents),
  25 February 2026.

### OpenAI

- Ryan Lopopolo,
  [*Harness engineering: leveraging Codex in an agent-first world*](https://openai.com/index/harness-engineering/),
  11 February 2026.
- Alex Kotliarskyi, Victor Zhu and Zach Brock,
  [*An open-source spec for Codex orchestration: Symphony*](https://openai.com/index/open-source-codex-orchestration-symphony/),
  27 April 2026.
- Aravind Srinivasan, Samay Shamdasani, Arthur Fernandes Araujo and John de
  Wasseige,
  [*Building self-improving tax agents with Codex*](https://openai.com/index/building-self-improving-tax-agents-with-codex/),
  27 May 2026.

### Anthropic

- [*How we built our multi-agent research system*](https://www.anthropic.com/engineering/multi-agent-research-system),
  13 June 2025.
- [*Effective context engineering for AI agents*](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents),
  29 September 2025.
- [*Beyond permission prompts: making Claude Code more secure and autonomous*](https://www.anthropic.com/engineering/claude-code-sandboxing),
  20 October 2025.
- [*Effective harnesses for long-running agents*](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents),
  26 November 2025.
- [*Demystifying evals for AI agents*](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents),
  9 January 2026.
- [*Building a C compiler with a team of parallel Claudes*](https://www.anthropic.com/engineering/building-c-compiler),
  5 February 2026.
- [*Quantifying infrastructure noise in agentic coding evals*](https://www.anthropic.com/engineering/infrastructure-noise),
  5 February 2026.
- [*Harness design for long-running application development*](https://www.anthropic.com/engineering/harness-design-long-running-apps),
  24 March 2026.
- [*How we built Claude Code auto mode*](https://www.anthropic.com/engineering/claude-code-auto-mode),
  25 March 2026.
- [*How we contain Claude across products*](https://www.anthropic.com/engineering/how-we-contain-claude),
  25 May 2026.
- [*Equipping agents for the real world with Agent Skills*](https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills),
  16 October 2025.

### AWS

- Aaron Sempf, Brad Ryan, Bhargs Srivathsan and Akhil Bhaskar,
  [*Operationalizing agentic AI on AWS*](https://docs.aws.amazon.com/prescriptive-guidance/latest/strategy-operationalizing-agentic-ai/introduction.html),
  August 2025.
- [*Evolving software delivery for agentic AI*](https://docs.aws.amazon.com/prescriptive-guidance/latest/strategy-operationalizing-agentic-ai/software-delivery.html),
  August 2025 guide.
- [*Focus area 5: Manage the lifecycle*](https://docs.aws.amazon.com/prescriptive-guidance/latest/strategy-operationalizing-agentic-ai/focus-areas-lifecycle.html),
  August 2025 guide.
- Alan Oberto Jimenez,
  [*Architecting for agentic AI development on AWS*](https://aws.amazon.com/blogs/architecture/architecting-for-agentic-ai-development-on-aws/),
  26 March 2026.
- Akarsha Sehwag and others,
  [*Build reliable AI agents with Amazon Bedrock AgentCore Evaluations*](https://aws.amazon.com/blogs/machine-learning/build-reliable-ai-agents-with-amazon-bedrock-agentcore-evaluations/),
  31 March 2026.

### Microsoft and GitHub

- Microsoft,
  [*Observability in generative AI*](https://learn.microsoft.com/en-us/azure/foundry/concepts/observability),
  updated 3 April 2026.
- Microsoft,
  [*Monitor agents with the Agent Monitoring Dashboard*](https://learn.microsoft.com/en-us/azure/foundry/observability/how-to/how-to-monitor-agents-dashboard),
  updated 10 April 2026.
- Microsoft,
  [*Governance and lifecycle actions for agents*](https://learn.microsoft.com/en-us/microsoft-365/admin/manage/agent-actions),
  updated 30 April 2026.
- Sarah Bird,
  [*Build agents you can trust across any framework with open evals and a control standard*](https://devblogs.microsoft.com/foundry/build-2026-open-trust-stack-ai-agents/),
  2 June 2026.
- Sebastian Kohlmeier,
  [*From observability to ROI for AI agents on any framework*](https://devblogs.microsoft.com/foundry/build-2026-from-observability-to-roi-for-ai-agents-on-any-framework/),
  3 June 2026.
- Microsoft,
  [*Agent governance security maturity model*](https://learn.microsoft.com/en-us/agents/adoption-maturity-model/maturity-model-security-governance),
  updated May 2026.
- GitHub,
  [*Security validation for third-party coding agents*](https://github.blog/changelog/2026-06-09-security-validation-for-third-party-coding-agents/),
  9 June 2026.

### IBM

- IBM,
  [*What is agent lifecycle management?*](https://www.ibm.com/think/topics/agent-lifecycle-management),
  23 June 2026.
- IBM,
  [*Agentic AI governance—Playbook*](https://www.ibm.com/think/insights/agentic-ai-governance-playbook),
  current as checked 10 July 2026.
- IBM,
  [*Agentic AI governance, evaluation and lifecycle*](https://www.ibm.com/new/announcements/agentic-ai-governance-evaluation-and-lifecycle),
  current as checked 10 July 2026.

### NIST and OWASP

- NIST CAISI,
  [*Strengthening AI Agent Hijacking Evaluations*](https://www.nist.gov/news-events/news/2025/01/technical-blog-strengthening-ai-agent-hijacking-evaluations),
  17 January 2025; updated 19 December 2025.
- NIST,
  [*AI Agent Standards Initiative*](https://www.nist.gov/artificial-intelligence/ai-agent-standards-initiative),
  launched 17 February 2026.
- NIST CAISI,
  [*Challenges to the Monitoring of Deployed AI Systems*](https://www.nist.gov/news-events/news/2026/03/new-report-challenges-monitoring-deployed-ai-systems),
  9 March 2026.
- NIST,
  [*Summary Analysis of Responses Regarding Security Considerations for AI Agents*](https://www.nist.gov/publications/summary-analysis-responses-request-information-regarding-security-considerations-ai),
  18 May 2026.
- NIST,
  [*Secure Software Development Framework 1.2 initial public draft*](https://csrc.nist.gov/pubs/sp/800/218/r1/ipd),
  17 December 2025.
- OWASP GenAI Security Project,
  [*Agentic AI – Threats and Mitigations*](https://genai.owasp.org/resource/agentic-ai-threats-and-mitigations/),
  17 February 2025.
- OWASP GenAI Security Project,
  [*Multi-Agentic System Threat Modeling Guide v1.0*](https://genai.owasp.org/resource/multi-agentic-system-threat-modeling-guide-v1-0/),
  23 April 2025.
- OWASP GenAI Security Project,
  [*Top 10 for Agentic Applications 2026*](https://genai.owasp.org/resource/owasp-top-10-for-agentic-applications-for-2026/),
  9 December 2025.

### Additional delivery-system evidence

- DORA,
  [*State of AI-assisted Software Development 2025*](https://dora.dev/dora-report-2025/),
  2025. This is used as evidence that AI amplifies the surrounding delivery
  system and moves effort toward verification; it is not an agent-lifecycle
  standard.
