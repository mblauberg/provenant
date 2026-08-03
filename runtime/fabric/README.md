# Fabric

Message passing, shared tasks and an activity log for agents working in the
same project. One SQLite file, no daemon, no setup.

## Use it

Nothing to install, trust, bootstrap or provision. The first call creates the
database and registers the caller.

    fabric whoami                       # who am I, who else is here
    fabric send codex "look at auth.ts" # to an agent, a team, or "all"
    fabric inbox                        # my unread messages
    fabric watch                        # tail everything my agents are doing
    fabric --help                       # every command and flag

The same operations are exposed over MCP as `fabric_whoami`, `fabric_send`,
`fabric_inbox`, `fabric_team_create`, `fabric_task_create`, `fabric_task_update`,
`fabric_tasks`, `fabric_note` and `fabric_activity`.

## Identity

An agent is `(project, agent_id)`.

- `project` is the git toplevel of the working directory, or the directory
  itself when it is not a repository. Both are equally valid projects.
- `agent_id` comes from `AGENT_FABRIC_LABEL`, falling back to
  `AGENT_FABRIC_SEAT` or `AGENT_FABRIC_CLIENT_LABEL`, which the providers
  already set.

Identity is derived from the process, never issued to it. Nothing expires, so
nothing needs renewing.

Two agents sharing a label share an inbox. Set `AGENT_FABRIC_LABEL` when you
want several agents of one provider to be addressed separately.

## Why there is no daemon

The predecessor mediated every call through a resident process so it could
validate a capability token. On a single-user machine that boundary is not
real: every agent runs as the same uid and can read every other agent's
capability file, so the token could only inconvenience an honest caller.

Removing it removes the socket, the protocol handshake, the reconnect path,
the seat store, the trust records and the eighteen preconditions that stood
between a new directory and its first message. SQLite in WAL mode already
supports many processes reading and writing one file, which was the only
technical job the daemon had left.

The cost of this choice: provider sessions are not held open between calls.
Skills dispatch `codex exec`, `agy` and `cursor-agent` per lane, which is how
they already work.

## Configuration

`AGENT_FABRIC_STATE_DIRECTORY` chooses where the database lives. It defaults
to `~/.local/state/agent-harness/fabric`. The directory's mode is the security
boundary.

## Layout

    schema.sql      9 tables
    src/identity.ts derive the caller and the project
    src/store.ts    every operation, one class
    src/server.ts   MCP surface
    src/cli.ts      shell surface
