import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";

import { databasePath, identify } from "./identity.js";
import { Store } from "./store.js";

/**
 * One MCP process per agent, holding the store open directly.
 *
 * There is no handshake to fail, so there is no reconnect path, so there is no
 * class of error that reports "the daemon is unavailable" while the daemon is
 * running. If the file cannot be opened the caller is told which file and why.
 */
const who = identify();
const store = new Store(databasePath());
store.announce(who);

const server = new McpServer({ name: "fabric", version: "2.0.0" });

/** Errors reach the caller intact. Nothing is swallowed and relabelled. */
function reply(payload: unknown): { content: Array<{ type: "text"; text: string }> } {
  return { content: [{ type: "text", text: JSON.stringify(payload, null, 2) }] };
}

server.registerTool(
  "fabric_whoami",
  {
    description: "Who am I, which project am I in, and who else is here.",
    inputSchema: {},
  },
  () => reply({ ...who, database: databasePath(), agents: store.agents(who.project) }),
);

server.registerTool(
  "fabric_send",
  {
    description:
      "Send a message to another agent, a team, or 'all' for everyone else in this project.",
    inputSchema: {
      to: z.string().describe("agent id, team id, or 'all'"),
      body: z.string(),
      kind: z.string().optional().describe("note, request, response, or anything you like"),
      reply_to: z.string().optional().describe("message id this replies to"),
    },
  },
  ({ to, body, kind, reply_to }) =>
    reply(store.send(who, to, body, { kind, replyTo: reply_to })),
);

server.registerTool(
  "fabric_inbox",
  {
    description:
      "Claim my unacknowledged messages. Peek observes without claiming; expired claims redeliver.",
    inputSchema: {
      limit: z.number().int().positive().optional(),
      peek: z.boolean().optional(),
      claim_seconds: z.number().int().min(1).max(3600).optional(),
    },
  },
  ({ limit, peek, claim_seconds }) =>
    reply(store.inbox(who, {
      limit,
      peek,
      claimTtlMs: claim_seconds === undefined ? undefined : claim_seconds * 1000,
    })),
);

server.registerTool(
  "fabric_acknowledge",
  {
    description: "Acknowledge one delivery using the claim token returned by fabric_inbox.",
    inputSchema: { message_id: z.string(), claim_id: z.string() },
  },
  ({ message_id, claim_id }) => reply(store.acknowledge(who, message_id, claim_id)),
);

server.registerTool(
  "fabric_team_create",
  {
    description: "Create a team or atomically replace all members of an existing team.",
    inputSchema: { team_id: z.string(), members: z.array(z.string()).min(1) },
  },
  ({ team_id, members }) => reply(store.createTeam(who, team_id, members)),
);

server.registerTool(
  "fabric_task_create",
  {
    description: "Record a task others can see, own and depend on.",
    inputSchema: {
      objective: z.string(),
      task_id: z.string().optional(),
      owner: z.string().optional(),
      depends_on: z.array(z.string()).optional(),
    },
  },
  ({ objective, task_id, owner, depends_on }) =>
    reply(store.createTask(who, objective, { taskId: task_id, owner, dependsOn: depends_on })),
);

server.registerTool(
  "fabric_task_update",
  {
    description: "Change a task's state, for example to claimed, blocked or done.",
    inputSchema: { task_id: z.string(), state: z.string(), note: z.string().optional() },
  },
  ({ task_id, state, note }) => reply(store.updateTask(who, task_id, state, note)),
);

server.registerTool(
  "fabric_task_claim",
  {
    description:
      "Atomically claim an open, unowned task. Retrying as the winning owner is idempotent.",
    inputSchema: { task_id: z.string() },
  },
  ({ task_id }) => reply(store.claimTask(who, task_id)),
);

server.registerTool(
  "fabric_tasks",
  {
    description: "List tasks in this project, optionally filtered by state.",
    inputSchema: { state: z.string().optional() },
  },
  ({ state }) => reply(store.tasks(who.project, state)),
);

server.registerTool(
  "fabric_note",
  {
    description: "Append a line to the project's activity log, for oversight.",
    inputSchema: { detail: z.string() },
  },
  ({ detail }) => {
    store.note(who, detail);
    return reply({ noted: detail });
  },
);

server.registerTool(
  "fabric_activity",
  {
    description:
      "Project activity. With after_seq, returns forward cursor order; otherwise newest first.",
    inputSchema: {
      limit: z.number().int().positive().optional(),
      after_seq: z.number().int().nonnegative().optional(),
    },
  },
  ({ limit, after_seq }) => reply(after_seq === undefined
    ? store.activity(who.project, limit)
    : store.activityAfter(who.project, after_seq, limit)),
);

await server.connect(new StdioServerTransport());
