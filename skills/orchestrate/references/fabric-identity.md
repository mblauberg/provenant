# Fabric connection identity

Fabric fixes the project from the MCP server process's working directory when
it starts, not from the caller's. A client whose server started elsewhere
attaches a work claim to the wrong project, where no other agent sees it.
Before every
`fabric_work_claim`, prove the connection's project; never hardcode a checkout
path.

1. **Expected project.** Take the parent of
   `git rev-parse --path-format=absolute --git-common-dir`, resolved through
   symlinks. A registered linked worktree resolves to its primary checkout.
2. **Check the active connection.** Call `fabric_whoami` on the connection
   that will claim. If its `project` equals the expected project, claim there.
3. **Otherwise open a second client.**
   1. Run `command -v provenant`. If it is absent, fail closed.
   2. Resolve `provenant root`. Through the MCP SDK stdio transport or an
      equivalent host API, create a client whose server process is
      `<root>/runtime/fabric/bin/fabric-mcp`, with this repository or worktree
      as its working directory and the current client's `AGENT_FABRIC_*`
      environment (seat, labels, state directory). Spawning the process alone
      gives no callable connection; if the host cannot create and hold a
      client, fail closed.
   3. Call `fabric_whoami` on the new client. Fail closed unless its `project`
      equals the expected project and its `database` equals the active
      connection's.
   4. Claim, renew and release through that client only. Do not return to the
      original connection for the operation.

Fail closed means: report the route unavailable and do not claim, so do not
dispatch a writer or land on that claim.
