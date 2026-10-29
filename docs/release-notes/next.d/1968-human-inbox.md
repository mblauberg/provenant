## Additions
### Chair, inbox and console

- `ask`, `inbox` and `answer` record human decisions and questions without the menu bar app, and the `fabric_inbox` MCP operations expose the same inbox. `inbox` also lists and answers requests on peer hosts (#1968).
- Queued answers reach the Claude session that asked, at its Bash tool calls, until it acknowledges them. Other providers receive them through Fabric mail (#1968).
- A request can link to a run, task or attempt and can carry a UTC deadline, after which it reads as expired and rejects late answers (#1968).
