# Engineer voice

Apply the `natural-writing` mechanisms first. This file
holds only what engineering text adds.

## Core test

Would an engineer write this sentence because the document needed it, or did a
model produce it to fill a shape? Delete the second kind.

## Internal process language

Agent, build and workspace vocabulary leaks into reader-facing text: `the
subagent found`, `the workflow ran`, `gate fired`, `per the manifest`, `the
review pass confirmed`, status enums, ticket numbers used as prose. Translate
to the engineering fact or cite the source. Reason: the reader cannot see the
process, so the sentence carries no evidence.

- Before: The perf agent reported that the gate passed.
- After: The load test recorded p95 of 210 ms against a 250 ms budget.

Placeholders (`TODO`, `TBD`, `FIXME`, `insert value`) are fine in drafts and
code; they are a defect in anything sent to a client or released.

## Engineering instances of the hub mechanisms

- Significance and participle tails: `ensures reliability` becomes the
  measurement or the failure it prevents.
- Copula avoidance: `serves as the entry point` becomes `is the entry point`.
- Unanchored claims: `robust error handling` becomes which errors and what
  happens: `a 5xx from upstream retries twice, then returns 503`.
- Ownership: `We chose Postgres over DynamoDB because joins dominate` beats
  `it was decided`. Passive fog hides the actor and the reason.
- Uncertainty in plain words: `appears`, `likely`, `unknown`, `not verified`,
  `assumed`. A budget overrun stays an overrun; never soften a real weakness.

## Structure

Bullets for parallel items (steps, fields, flags), prose for rationale,
headings only to navigate. Agent-written docs still sprawl into bold-lead
bullets and a heading on every paragraph; collapse those.

## Not humanising

No slang, jokes, anecdotes, rhetorical questions or invented personality, and
no editing to beat a detector.
