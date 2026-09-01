import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { setTimeout as delay } from "node:timers/promises";
import { z } from "zod";

import { databasePath, identify } from "./identity.js";
import {
  cancelActiveExecutions,
  dispatchConfiguredBatch,
  dispatchConfiguredProvider,
  MAX_EXECUTION_WAIT_SECONDS,
} from "./execution.js";
import { isSQLiteContention, Store, type Message } from "./store.js";

// Leave margin under the MCP SDK's 60-second default request timeout.
const MAX_WAIT_SECONDS = MAX_EXECUTION_WAIT_SECONDS;

/**
 * One MCP process per agent, holding the store open directly.
 *
 * There is no handshake to fail, so there is no reconnect path, so there is no
 * class of error that reports "the daemon is unavailable" while the daemon is
 * running. If the file cannot be opened the caller is told which file and why.
 */
const who = identify();
let store: Store | undefined;
const initialiseStore = (busyTimeoutMs = 5000): Store => {
  const opened = new Store(databasePath(), busyTimeoutMs);
  try {
    opened.announce(who);
    opened.restoreDefaultBusyTimeout();
    store = opened;
    return opened;
  } catch (error) {
    opened.close();
    throw error;
  }
};

try {
  initialiseStore();
} catch {
  // Keep the transport available. The first tool call retries initialisation,
  // allowing transient SQLite locks to recover without a background process.
}

const readyStore = (busyTimeoutMs = 5000): Store => {
  if (store !== undefined) return store;
  try {
    return initialiseStore(busyTimeoutMs);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw new Error(`fabric startup failed: ${detail}`, { cause: error });
  }
};

const server = new McpServer(
  { name: "fabric", version: "2.0.0" },
  {
    instructions:
      "Fabric is a project-scoped mailbox, cooperative task ledger, and activity log. " +
      "Use fabric_inbox to claim requests, persist any response before calling " +
      "fabric_acknowledge, and correlate replies with reply_to. Create targeted " +
      "tasks with an owner; owner is cooperative routing metadata, not an access-control boundary. " +
      "Use fabric_dispatch or fabric_batch for ordinary configured-provider work; full output stays " +
      "in the returned run paths.",
  },
);
server.server.onclose = () => {
  cancelActiveExecutions();
  store?.close();
  store = undefined;
};

/** Errors reach the caller intact. Nothing is swallowed and relabelled. */
function reply(payload: unknown): { content: Array<{ type: "text"; text: string }> } {
  return { content: [{ type: "text", text: JSON.stringify(payload, null, 2) }] };
}

const waitForInbox = async (
  options: {
    limit?: number;
    peek?: boolean;
    claimTtlMs?: number;
    busyTimeoutMs?: number;
  },
  waitMs: number,
  signal: AbortSignal,
) => {
  const deadline = performance.now() + waitMs;
  for (;;) {
    if (waitMs > 0) await delay(0, undefined, { signal });
    signal.throwIfAborted();
    const remainingBeforeClaim = deadline - performance.now();
    if (waitMs > 0 && remainingBeforeClaim <= 0) return [];
    let messages: Message[];
    try {
      const busyTimeoutMs = waitMs === 0
        ? undefined
        : 1;
      messages = readyStore(busyTimeoutMs).inbox(who, {
        ...options,
        busyTimeoutMs,
      });
    } catch (error) {
      if (waitMs === 0 || !isSQLiteContention(error)) throw error;
      messages = [];
    }
    if (messages.length > 0 || waitMs === 0) return messages;
    const remaining = deadline - performance.now();
    if (remaining <= 0) return [];
    await delay(Math.min(100, remaining), undefined, { signal });
  }
};

server.registerTool(
  "fabric_whoami",
  {
    description: "Who am I, which project am I in, and who else is here.",
    inputSchema: {},
  },
  () => reply({ ...who, database: databasePath(), agents: readyStore().agents(who.project) }),
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
    reply(readyStore().send(who, to, body, { kind, replyTo: reply_to })),
);

server.registerTool(
  "fabric_inbox",
  {
    description:
      "Claim my unacknowledged messages. Peek observes without claiming; expired claims redeliver. " +
      "Set wait_seconds for one bounded wait inside this MCP call; never poll SQLite or start a watcher.",
    inputSchema: {
      limit: z.number().int().positive().optional(),
      peek: z.boolean().optional(),
      claim_seconds: z.number().int().min(1).max(3600).optional(),
      wait_seconds: z.number().int().min(0).max(MAX_WAIT_SECONDS).optional(),
    },
  },
  async ({ limit, peek, claim_seconds, wait_seconds }, { signal }) =>
    reply(await waitForInbox({
      limit,
      peek,
      claimTtlMs: claim_seconds === undefined ? undefined : claim_seconds * 1000,
    }, (wait_seconds ?? 0) * 1000, signal)),
);

const routeInputSchema = {
  adapter: z.string().min(1).optional().describe("provider adapter; defaults to the current Fabric seat"),
  alias: z.string().min(1).optional().describe("route alias; defaults to workhorse"),
  task_class: z.string().min(1).optional(),
  model: z.string().min(1).optional(),
  role: z.string().min(1).optional().describe("worker role; defaults to worker"),
  effort: z.string().min(1).optional(),
  orchestrator_family: z.string().min(1).optional(),
  risk_tier: z.string().min(1).optional(),
  model_override_tier: z.enum(["routine", "substantial", "crucial", "terminal"]).optional(),
  reviewer_id: z.string().min(1).optional(),
};

const batchTaskSchema = z.object({
  id: z.string().regex(/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/u).optional(),
  prompt: z.string().optional(),
  prompt_file: z.string().min(1).optional(),
  timeout_seconds: z.number().positive().finite().optional(),
  ...routeInputSchema,
});

server.registerTool(
  "fabric_dispatch",
  {
    description:
      "Start one ordinary configured-provider task. Run custody is automatic and full prompt, " +
      "result and diagnostics stay in named files. The response is compact; set wait_seconds to " +
      "0 for immediate start or up to 55 for a terminal result and actual route when it finishes.",
    inputSchema: {
      prompt: z.string().optional(),
      prompt_file: z.string().min(1).optional(),
      task_id: z.string().regex(/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/u).optional(),
      timeout_seconds: z.number().positive().finite().optional(),
      wait_seconds: z.number().int().min(0).max(MAX_WAIT_SECONDS).optional(),
      ...routeInputSchema,
    },
  },
  async (input, { signal }) => reply(await dispatchConfiguredProvider(input, who, signal)),
);

server.registerTool(
  "fabric_batch",
  {
    description:
      "Start a fixed ordinary batch of 1-64 configured-provider tasks with concurrency capped at 8. " +
      "The existing batch owner keeps partial results and rejects shared-writer batches. Full output " +
      "stays in named files; set wait_seconds to 0 for immediate start or up to 55 to await completion.",
    inputSchema: {
      tasks: z.array(batchTaskSchema).min(1).max(64),
      concurrency: z.number().int().min(1).max(8).optional(),
      wait_seconds: z.number().int().min(0).max(MAX_WAIT_SECONDS).optional(),
    },
  },
  async (input, { signal }) => reply(await dispatchConfiguredBatch(input, who, signal)),
);

server.registerTool(
  "fabric_acknowledge",
  {
    description: "Acknowledge one delivery using the claim token returned by fabric_inbox.",
    inputSchema: { message_id: z.string(), claim_id: z.string() },
  },
  ({ message_id, claim_id }) => reply(readyStore().acknowledge(who, message_id, claim_id)),
);

server.registerTool(
  "fabric_team_create",
  {
    description: "Create a team or atomically replace all members of an existing team.",
    inputSchema: { team_id: z.string(), members: z.array(z.string()).min(1) },
  },
  ({ team_id, members }) => reply(readyStore().createTeam(who, team_id, members)),
);

server.registerTool(
  "fabric_task_create",
  {
    description:
      "Record a task others can see, own and depend on. Set owner for targeted routing; " +
      "an owner-bound task is already assigned and is not available to unowned-task claiming. " +
      "Task ownership is cooperative routing metadata, not an access-control boundary.",
    inputSchema: {
      objective: z.string(),
      task_id: z.string().optional(),
      owner: z.string().optional(),
      depends_on: z.array(z.string()).optional(),
    },
  },
  ({ objective, task_id, owner, depends_on }) =>
    reply(readyStore().createTask(who, objective, { taskId: task_id, owner, dependsOn: depends_on })),
);

server.registerTool(
  "fabric_task_update",
  {
    description: "Change a task's cooperative state, for example to blocked or done.",
    inputSchema: { task_id: z.string(), state: z.string(), note: z.string().optional() },
  },
  ({ task_id, state, note }) => reply(readyStore().updateTask(who, task_id, state, note)),
);

server.registerTool(
  "fabric_task_claim",
  {
    description:
      "Atomically claim an open, unowned task. Owner-bound tasks are already assigned and " +
      "are not available to other claimers; retrying as the winning owner is idempotent.",
    inputSchema: { task_id: z.string() },
  },
  ({ task_id }) => reply(readyStore().claimTask(who, task_id)),
);

server.registerTool(
  "fabric_tasks",
  {
    description: "List tasks in this project, optionally filtered by state.",
    inputSchema: { state: z.string().optional() },
  },
  ({ state }) => reply(readyStore().tasks(who.project, state)),
);

server.registerTool(
  "fabric_note",
  {
    description: "Append a line to the project's activity log, for oversight.",
    inputSchema: { detail: z.string() },
  },
  ({ detail }) => {
    readyStore().note(who, detail);
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
    ? readyStore().activity(who.project, limit)
    : readyStore().activityAfter(who.project, after_seq, limit)),
);

const transport = new StdioServerTransport();
process.stdin.once("end", () => { void transport.close(); });
await server.connect(transport);
