## Additions

- Add app-independent human decisions, assumptions and questions through `ask`, `inbox`, `answer` and discoverable `fabric_inbox` MCP operations. Answers compare the viewed revision, retain immutable history and queue correlated local Fabric mail atomically. Acknowledgement requires the originating host, project, session and actor. Claude hook delivery, remote inbox owners and the console/menu changes remain outside this standalone subset of #1968.
- Prepare internal Run-owned lookup support for future inbox correlations. Only host-minted root, resume and continuation attempts are indexed; nested work remains unindexed. Inbox correlations, supersession and expiry follow separately. Part of #1968.
