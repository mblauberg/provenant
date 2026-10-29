---
name: engineering-writing
description: "Use for software docs, READMEs, commits, PR bodies, runbooks, error text, release notes and thesis LaTeX invariants, written for readers who will not review closely. Not for general or academic prose (natural-writing), Australian legal drafting (legal-writing), or durable-doc placement and ADRs (engineering-docs)."
---

# Engineering writing

The engineering overlay on `natural-writing`: that hub owns Australian
English, the mechanism catalogue, the condense pass and claim classes. This
skill adds what a reader of agent-written engineering text needs when they
will not check every line.

## Reader contract

1. **Decision first.** Open with what changed, what was decided, or what the
   reader must do. Background follows only if it changes their action.
2. **Every claim traces** to code, a command, a test result or a link. Verify
   each package, API, version, flag and URL against the source; otherwise mark
   `[FLAG: verify]`. Never invent a specific to fill a shape.
3. **Say what was not done or verified.** `Not load-tested above 500 users`
   and `retry is out of scope here` are the most valuable lines in a PR.
4. **Observed vs inferred.** `CPU sat at 100% for 4 minutes` is observed; `the
   regex likely backtracks` is inferred, and marked as such.
5. **One term per concept.** Repeat the precise noun; do not cycle `endpoint`,
   `route` and `URL` for variety.
6. **A command or path beats a paragraph** that describes it.

## Workflow

1. Name the reader, what they already know and what they must do. If the job
   is unclear, say so before drafting.
2. Lock identifiers, commands, flags, error text, versions, numbers, dates,
   obligations and honest negatives. They survive any edit.
3. Pick the artefact shape from
   [artefacts.md](references/artefacts.md): README, PR, commit, error,
   bug report, runbook, postmortem, migration note. For release notes and
   fragments use [release-notes.md](references/release-notes.md). For thesis
   and paper LaTeX use [academic-artefacts.md](references/academic-artefacts.md).
4. Draft structure before sentences, then apply the hub's mechanisms and
   [engineer-voice.md](references/engineer-voice.md) for the internal-process
   leaks agents add to deliverables.
5. Audit fidelity independently when a style pass touched facts: give a
   read-only reviewer the before and after, on fixed axes (dropped facts,
   promoted hedges, weakened warnings, invented content). Adopt only on an
   explicit verdict.

## Bounds

- Edit living docs in place; do not append dated entries. Placement and ADR
  scope belong to `engineering-docs`.
- Never style-pass gates, credential custody, decision records or literal
  runbooks; edit them by hand, and check first whether a test matches their
  exact wording.
- Agent-authored commits keep the repository's trailer convention. This skill
  returns a message; staging or committing needs its own authority.
- Specs, issue bodies and plans belong to `scope` and `tracker`, not here.
- Flag what you cannot settle: `[FLAG: verify source]`,
  `[FLAG: define audience]`, `[FLAG: shortening drops obligation]`.
