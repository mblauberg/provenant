# Fabric connection identity

Fabric fixes the project from the MCP server process's working directory when
it starts, not from the caller's. A client whose server started elsewhere
attaches a work claim to the wrong project, where no other agent sees it.
Before every `fabric_work_claim`, prove the connection's project; never derive
the root by hand or hardcode a checkout path.

1. **Expected identity.** If `command -v provenant` finds nothing, fail closed.
   Otherwise run `provenant fabric whoami` from this repository or worktree,
   with the same `AGENT_FABRIC_*` environment as the MCP client (seat, labels,
   state directory). Its `project` and `database` are the expected identity:
   it uses Fabric's own resolver, which checks worktree registration and the
   linked worktree's back-pointer and ignores Git redirect variables, so a
   copied or separate-git-dir checkout resolves exactly as Fabric resolves it.
2. **Check the active connection.** Call `fabric_whoami` on the connection
   that will claim. If its `project` and `database` both match, claim there.
3. **Otherwise open a second client.**
   1. Resolve `provenant root`. Through the MCP SDK stdio transport or an
      equivalent host API, create a client whose server process is
      the installed `provenant` binary (MCP entry point), with this repository or worktree
      as its working directory and the same `AGENT_FABRIC_*` environment.
      Spawning the process alone gives no callable connection; if the host
      cannot create and hold a client, fail closed.
   2. Call `fabric_whoami` on the new client. Fail closed unless its `project`
      and `database` match the expected identity.
   3. Claim, renew and release through that client only. Do not return to the
      original connection for the operation.

Fail closed means: report the route unavailable and do not claim, so do not
dispatch a writer or land on that claim.
