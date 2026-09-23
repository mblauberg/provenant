# Document Patterns

Use this reference for codebase-facing and short-form prose shapes: docs, READMEs, PRs, commits, errors, UI text, comments, reports, and reviews. For heavyweight project-inception deliverables (requirements/SRS, user stories, scoping, stakeholder analysis, business case, development plan, roadmap, estimates) see `requirements-and-planning.md`; for architecture descriptions, full ADRs, design documents, presentations, and meeting briefs see `architecture-and-presentations.md`.

Choose the document shape before rewriting. Structure should match the reader's job, not the writer's source material.

## Contents

- [How-To Guide](#how-to-guide)
- [Tutorial](#tutorial)
- [Reference](#reference)
- [Explanation or Design Note](#explanation-or-design-note)
- [README](#readme)
- [ADR](#adr)
- [Implementation Plan](#implementation-plan)
- [Pull Request](#pull-request)
- [Commit Message](#commit-message)
- [Changelog and Release Notes](#changelog-and-release-notes)
- [Error Message](#error-message)
- [UI Text](#ui-text)
- [Code Comments and Docstrings](#code-comments-and-docstrings)
- [Bug Report](#bug-report)
- [Incident Report / Postmortem](#incident-report--postmortem)
- [Runbook](#runbook)
- [Migration Guide and Deprecation Notice](#migration-guide-and-deprecation-notice)
- [Contributing and Onboarding Docs](#contributing-and-onboarding-docs)
- [Reports and Summaries](#reports-and-summaries)
- [Review Findings](#review-findings)

## How-To Guide

Use when the reader wants to complete a task.

Structure:

1. Goal
2. Prerequisites
3. Steps
4. Expected result
5. Troubleshooting

Keep theory short. Put commands before explanation when the user is blocked.

## Tutorial

Use when the reader is learning.

Structure:

1. Learning goal
2. Small runnable path
3. Explanation after each meaningful action
4. Expected output
5. Next step

Avoid optional branches that increase cognitive load.

## Reference

Use when the reader needs exact facts.

Include syntax, parameters, return values, defaults, limits, errors, examples, and version notes.
Minimise prose.

## Explanation or Design Note

Use when the reader needs context or trade-offs.

Structure:

1. Problem or context
2. Constraints
3. Options considered
4. Decision or current behaviour
5. Trade-offs and risks

Separate observations from interpretations.

## README

Use when the reader needs orientation. A repository README is a landing page, so
write for a capable reader who does not yet know the project.

Answer these first, in the reader's order (GitHub's README guidance):

1. What is this? One plain sentence, no positioning against inferior tools.
2. Why would I use it? A few concrete outcome bullets, not adjectives.
3. What do I need, and how do I start? Prerequisites *before* the first command.
4. What are the important limits? The constraints that change how it is used.
5. Where is the deeper detail, and how do I get help? Links to canonical owners.

Structure that usually follows:

1. Name, one-sentence purpose, badges, and a compact status note.
2. What it adds: outcome bullets plus a short component map.
3. Quick start: requirements, install, verify, expected result.
4. Choose a workflow: a `Need → Skill` (or `Task → Command`) table.
5. How it works: one diagram, optional worked example in a collapsed section.
6. Important constraints and where the deeper docs live.

Discipline:

- Include only high-value orientation. Link the canonical owner (architecture,
  maintenance, security, policy) instead of restating it; duplicated detail
  makes the important content harder to find and drifts out of date.
- Make the quick start procedural: state audience, prerequisites and expected
  result; keep only essential steps; put verification in its own step. Do not
  round exact supported versions (`Node.js >=24.15.0 <25`, not `Node.js 24`).
- Match formatting to information type: bullets for benefits and independent
  constraints, numbered lists for procedures, tables only for compact mappings,
  short prose for reasoning and trade-offs, and `<details>` for optional depth
  (full catalogues, installer edge cases, long examples). Do not shatter
  connected reasoning into fragments just to make more bullets.
- Use confident, neutral language. Show value through specific outcomes, not by
  contrasting the project with a worse reader or tool.

Keep status, roadmap, and changelog material out unless the repository uses the
README as the current-state authority. Preserve every load-bearing fact
(versions, commands, security-reporting path, machine-managed/generated blocks)
verbatim through a rewrite; a shorter README must not drop an obligation.

## ADR

Use when recording a durable architectural decision. Quick shape below; full guidance (status lifecycle, supersession, Nygard format) in `architecture-and-presentations.md`.

Structure: title (a decision), status, context (forces), decision (active voice, present tense), consequences (positive, negative, and neutral). One decision per record. Do not mix proposed and accepted decisions unless the status makes that explicit; supersede rather than edit an accepted ADR.

## Implementation Plan

Use when the reader must execute work.

Structure:

1. Goal and non-goals
2. Current state
3. Constraints and invariants
4. Ordered tasks with ownership boundaries
5. Tests and verification
6. Risks, rollback, and unresolved questions

Avoid vague phases such as `polish` or `hardening` unless they list concrete checks.

## Pull Request

Structure:

1. Problem
2. Change
3. User or system impact
4. Tests
5. Risks or follow-up

Keep implementation detail tied to review value.

## Commit Message

Follow the repository's established convention. Where it uses Conventional
Commits, prefer `<type>(<scope>): <imperative summary>` with a narrow optional
scope. Keep the subject specific and normally within 50 characters when that
does not hide meaning; never exceed a project/provider limit.

Good:

```text
Validate token expiry before refresh
```

Add a body only when the reason is not obvious or the change is breaking,
security-sensitive, a migration, or a revert. Preserve required issue and
attribution trailers. Agent-authored commits use one trailer per contributing
route, copied from the attempt receipt:

```text
Agent-Route: codex/gpt-6-luna@high
```

Use `adapter/model@effort`, not a family-only label. Native Claude subagents
use `claude/<model>@<effort> (anthropic; resolved)` from the Agent tool model
parameter, not self-report. The writing skill returns a message; staging, committing
or amending requires separate authority.

## Changelog and Release Notes

A changelog is the exhaustive, versioned record for developers; release notes are the curated, user-facing story of what a release means. Do not merge them: a changelog entry for every change, release notes only for changes a user notices.

Changelog (per keepachangelog.com):

1. One section per release, newest first, with version and date (ISO).
2. Group entries: Added, Changed, Deprecated, Removed, Fixed, Security.
3. One line per change, written for a user, not a copied commit subject.
4. Keep an Unreleased section at the top.

Release notes: lead with the change that matters most to the user, state what changed in their terms, then any action required (migration, config, re-auth). Breaking changes and deprecations first, never buried. Do not list internal refactors.

## Error Message

Say:

1. What failed
2. Why, if known
3. What to do next

Avoid blame and vague apologies.

## UI Text

- Use the same labels as the interface.
- Prefer verbs for actions.
- Keep labels short.
- Do not explain implementation details unless they affect user choice.

## Code Comments and Docstrings

Use comments for intent, invariants, side effects, traps, and trade-offs.
Do not narrate obvious syntax.

Docstrings should describe contract, parameters, return shape, errors, side effects, and gotchas.
Keep repository comment density and tone unless it is clearly unhelpful.

## Bug Report

Use when reporting a defect so someone else can reproduce and fix it.

Structure:

1. Summary: one line, symptom plus condition (`Login times out when the username contains a +`).
2. Environment: version, platform, config that matters.
3. Steps to reproduce: numbered, minimal, from a known starting state.
4. Expected result.
5. Actual result: exact error text, status codes, logs, screenshots.
6. Impact and frequency: who is affected, how often, workaround if known.

Discipline: separate observation from diagnosis; if you suspect a cause, say so under a marked hypothesis, not in the summary (the summary states the symptom, never a proposed fix). One defect per report; search for duplicates before filing. Reduce the reproduction to the minimum that still fails, from a clean environment: a bug a developer can reproduce is a bug that gets fixed.

## Incident Report / Postmortem

Use after an outage or serious defect, once service is restored. Blameless: name systems, gaps, and decisions, not culprits; assume people acted reasonably on the information they had.

Structure:

1. Summary: what broke, for how long, who was affected, severity.
2. Impact: measured (requests failed, users affected, data lost, money, SLO burn).
3. Timeline: timestamped (with timezone) detection, escalation, decisions, mitigation, resolution.
4. Root cause and contributing factors: the mechanism, not a person. `The deploy removed the index; the health check did not cover query latency.`
5. What went well / what went poorly / where we got lucky (detection, response, tooling).
6. Action items: each with an owner, a priority, and a tracking link; prevention and detection, not cleanup alone.

Discipline: write the timeline forward from before the incident, from logs and chat records, not memory; hindsight bias writes itself in otherwise. Distinguish the trigger (the immediate cause) from the root cause (the condition that made it possible). Do not soften the impact numbers. An action item without an owner is a wish. Have the postmortem reviewed before circulating it; an unreviewed postmortem might as well not exist (Google SRE).

## Runbook

Use for an operational procedure someone must execute correctly under time pressure, possibly at 3 am.

Structure:

1. When to use: the alert, symptom, or trigger this responds to.
2. Preconditions and access: permissions, tools, safety checks before starting.
3. Steps: numbered, one action each, exact commands with placeholders marked; expected output after each step that confirms it worked.
4. Verification: how to confirm the system is healthy again.
5. Rollback: how to undo if a step fails.
6. Escalation: when to stop and who to page.
7. Metadata: owner, last-reviewed date, dashboard and log links.

Discipline: write for the reader with the least context who is on call. Commands copy-pasteable; expected output after each step. No unexplained judgement calls (`if the queue looks bad` fails; `if depth > 10,000 for 5 minutes` works). Link the runbook from the alert that triggers it. Update it immediately after each use or postmortem. Test it by having someone else follow it verbatim; every place they stop to ask is a defect.

## Migration Guide and Deprecation Notice

A migration guide bridges a breaking change to user action; a deprecation notice starts the clock on one.

Migration guide structure: who must act and who can ignore it; before/after for each breaking change (old call, new call, mapping table for renamed options); ordered upgrade steps; how to verify the migration worked; rollback. Lead with the smallest change that keeps the user working.

Deprecation notice: what is deprecated (exact identifier and version), why, the replacement, the removal date or version, and where to get help. Announce once loudly and repeat in the changelog under Deprecated; never let removal arrive as a surprise.

## Contributing and Onboarding Docs

Reader: a new contributor or team member with zero context, whose questions are practical.

Structure: how to set up a working environment (with the one command that does it, if possible); the workflow (branching, commits, review expectations, CI); coding and testing conventions, by link rather than restatement; how to get help. Keep it current or delete it; a stale CONTRIBUTING doc costs more than none.

## Reports and Summaries

Lead with the finding.
Then give evidence, impact, uncertainty, and next action.

Use exact dates, versions, sample sizes, and measurement conditions when they affect the result.

## Review Findings

Use when reporting defects or risks.

Structure:

1. Severity
2. Finding
3. Evidence
4. Impact
5. Suggested fix

Put defects before summary. If there are no defects, say that directly and name residual risk or untested scope.
