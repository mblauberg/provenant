## Additions
### Fabric and MCP

- Fabric MCP refusals for an unknown argument or action now name the one valid name it most likely meant when the match is close and unambiguous, such as `sessionId` for `session`. The call is still refused and nothing is corrected automatically (#2124).
