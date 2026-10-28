## Additions

- Add app-independent human decisions, assumptions and questions through `ask`, `inbox`, `answer` and discoverable `fabric_inbox` MCP operations. Answers compare the viewed revision, retain immutable history and queue correlated local Fabric mail atomically. Acknowledgement requires the originating host, project, session and actor. Claude hook delivery, remote inbox owners and the console/menu changes remain outside this standalone subset of #1968.

- Add a Run-owned bounded work lookup for request correlation. Host-sealed project, run, task, attempt, repository and observed head facts cover nested launches and report missing coverage explicitly. Inbox correlation, atomic replacement and deadlines follow separately. Part of #1968.
