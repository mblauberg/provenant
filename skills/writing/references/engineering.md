# Engineering text

For readers who won't check every line. Where a doc lives and ADR scope belong
to `engineering-docs`.

## Reader contract

1. **Decision first.** Open with what changed, what was decided or what the
   reader must do. Background follows only if it changes their action.
2. **Every claim traces** to code, a command, a test result or a link. Verify
   each package, API, version, flag and URL against the source, or mark
   `[FLAG: verify]`. Never invent a specific to fill a shape.
3. **Say what was not done or verified.** `Not load-tested above 500 users` and
   `retry is out of scope here` are the most useful lines in a PR. Observed
   and inferred stay apart ([claims.md](claims.md)).
4. **A command or path beats a paragraph** that describes it.
5. Name the actor and the reason: `We chose Postgres because joins dominate`,
   not `it was decided`. Uncertainty in plain words (`likely`, `not verified`,
   `assumed`); a budget overrun stays an overrun.

## Process-language leaks

Agent and workspace vocabulary escapes into deliverables: `the subagent
found`, `the workflow ran`, `gate fired`, `per the manifest`, `the review pass
confirmed`, status enums, ticket numbers used as prose. The reader can't see
the process, so these carry no evidence. State the engineering fact or cite
the source.

- Before: The perf agent reported that the gate passed.
- After: The load test recorded p95 of 210 ms against a 250 ms budget.

`TODO`, `TBD` and `insert value` are fine in drafts and a defect in anything
sent or released. `robust error handling` becomes which errors and what
happens: `a 5xx from upstream retries twice, then returns 503`.

## Artefacts

- **README.** Prerequisites before the first command; a separate verify step
  with its expected result; exact supported versions. Status and roadmap stay out unless the README is the
  current-state authority. A shortened README keeps every load-bearing fact
  (versions, commands, security-reporting path, generated blocks) verbatim.
- **Pull request.** What changed, why, tests run with results, what was not
  verified, follow-ups, and net tests added and removed. The reader may be an
  agent with no memory of your session.
- **Commit.** Follow the repository's convention. Agent-authored commits keep
  its trailer convention, and issue and attribution trailers stay exact. This
  skill returns a message; staging or committing needs its own authority.
- **Bug report.** Observation stays apart from diagnosis: a suspected cause
  sits under a marked hypothesis, and the summary never proposes a fix. Exact
  error text; search for duplicates first.
- **Runbook.** Written for the on-call reader with least context. Copy-paste
  commands with expected output, rollback and escalation. No unexplained
  judgment: `if depth > 10,000 for 5 minutes`, not `if the queue looks bad`.
- **Postmortem.** Blameless. Timeline times and timezone come from logs, not
  memory; trigger and root cause are separate; impact numbers are never
  softened.
- **Deprecation.** Names the exact identifier and version, the replacement and
  the removal condition.
- **Error text.** What failed, why if known, what to do next.

## Release notes

The reader is an upgrader: should they upgrade, what must they do. Clerical,
not promotional.

1. Order: intro, breaking changes, behaviour changes, additions, fixes. Write
   the intro last. Breaking changes hold only entries needing user action, and
   each states the action; a change needing none is a behaviour change.
2. One to three sentences per entry, subject first in code font, one tense.
   State the user-visible effect, not the mechanism. Fixes name the symptom and
   the new behaviour, not the root cause.
3. Keep user-actionable numbers (defaults, limits, flags); drop internal ones
   (retry counts, schema versions) unless they change what the user must do.
4. Omit what no user notices: refactors, tests, CI, fixes to unreleased code,
   slice or phase numbers, process words.
5. Say each thing once. Merge entries on one topic, repeats between intro and
   entry included, citing every issue on the merged entry. Standing policy
   (stability, supported platforms) lives on one stable page.
6. End an entry with `(#123, #124)`; link a guide for anything longer. Follow
   the project's fragment guide for tags and grouping. Fragments are finished
   sentences, one change per file. Never paste a commit log.

- Before: The scheduler no longer reads the wall clock. It uses the monotonic
  clock since boot, which no clock change can move, so two jobs never start
  less than one interval apart.
- After: Job spacing no longer changes when the system clock does.

When rewriting notes, every fact and issue number survives; list merges and
drops.
