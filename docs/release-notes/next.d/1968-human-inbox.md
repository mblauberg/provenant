## Additions
### Chair, inbox and console

- `ask`, `inbox` and `answer` record human decisions, assumptions and questions without the menu bar app, and the `fabric_inbox` MCP operations expose the same inbox. An answer is checked against the revision the human viewed and is kept in an immutable history. `inbox` also lists and answers requests on peer hosts; closing and acknowledging stay on the owning host (#1968).
- Queued answers are offered to the Claude session that asked, at its Bash tool calls, until the session acknowledges them. The hook's JSON is limited to 8 KiB and an answer over the delivery limit is refused. Fabric mail fallback is published for all five providers; live injection is not available (#1968).
- A request can link to a run, task, attempt, repository and launch head, and can be replaced after it is viewed without losing its answers. A request with a UTC deadline reads as expired once the deadline passes and rejects late answers; nothing is approved on a deadline's behalf (#1968).
