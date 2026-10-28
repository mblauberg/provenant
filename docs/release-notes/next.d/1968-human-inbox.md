## Additions

- Add app-independent human decisions, assumptions and questions through `ask`, `inbox`, `answer` and discoverable `fabric_inbox` MCP operations. Answers compare the viewed revision, retain immutable history and queue correlated local Fabric mail atomically. Acknowledgement requires the originating host, project, session and actor.
- Add discoverable MCP console and chair tools, and route inbox reads and answers to configured owners.
- Prepare internal Run-owned lookup support for future inbox correlations. Only host-minted root, resume and continuation attempts are indexed; nested work remains unindexed. Part of #1968.
