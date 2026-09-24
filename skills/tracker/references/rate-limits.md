# Rate limits

GitHub's GraphQL budget (points per hour) is separate from REST's and is
easier to exhaust: a full project-board snapshot that pages every item's
`fieldValues` burns it fast, and once it's gone every agent sharing that
account's quota is blocked until the reset.

- **Prefer REST.** Issue and PR state, labels, comments, search, closing, and
  sub-issues (`/issues/{n}/sub_issues`, `/sub_issue`) all go through REST.
  Reserve GraphQL for Projects v2 field reads/writes (Status, Horizon) REST
  cannot reach.
- **Read cheaply.** Ask for the one field you need
  (`fieldValueByName(name: "Status")`) rather than every field on every item,
  and read one issue's board item rather than the whole board when only one is
  needed.
- **Check the budget before a paged read.** `gh api rate_limit` is free.
  Below a safe floor, defer instead of attempting the read, and say so with
  the reset time.
- **Back off and report, never loop.** On `RATE_LIMIT` or a secondary rate
  limit, stop immediately. Report which call failed and the reset time from
  the response; do not retry, poll, or fall back to a workaround that spends
  the same budget a different way.
- **Batch writes.** Where several edits are queued (steward mode), apply them
  in one pass rather than round-tripping per item.

A project with its own tracker module (a cached board snapshot, conditional
REST with ETags, a budget guard) supersedes this section for that repo's
calls; use its command instead of raw `gh` when one exists.
