import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { setTimeout as delay } from "node:timers/promises";
import { z } from "zod";

import { databasePath, identify } from "./identity.js";
import { fabricStatus } from "./run-registry.js";
import { catalogueSnapshot } from "./catalogue.js";
import {
  cancelActiveExecutions,
  DISPATCH_ADAPTERS,
  dispatchConfiguredBatch,
  dispatchConfiguredProvider,
  MAX_EXECUTION_WAIT_SECONDS,
  terminateActiveExecutionGroups,
} from "./execution.js";
import { isSQLiteContention, Store, type Message } from "./store.js";

// Leave margin under the MCP SDK's 60-second default request timeout.
const MAX_WAIT_SECONDS = MAX_EXECUTION_WAIT_SECONDS;

/**
 * One MCP process per agent; coordination tools open the store on first use.
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
/**
 * Nothing this host started may outlive it. The transport closing is the
 * ordinary path, so cancellation is awaited before the store is released:
 * closing first let the process exit while the cancel was still in flight.
 */
server.server.onclose = async () => {
  await cancelActiveExecutions();
  store?.close();
  store = undefined;
};

/**
 * An external signal never reaches `onclose`, so the same teardown is bound to
 * the signals a supervisor actually sends. The group signal is delivered
 * synchronously first, because the process may not survive long enough to
 * finish the awaited path. SIGKILL cannot be caught at all: a run orphaned that
 * way is reaped by the next dispatch in the same workspace.
 */
let shuttingDown = false;
for (const signal of ["SIGTERM", "SIGINT", "SIGHUP"] as const) {
  process.on(signal, () => {
    if (shuttingDown) return;
    shuttingDown = true;
    terminateActiveExecutionGroups();
    void cancelActiveExecutions().finally(() => {
      store?.close();
      store = undefined;
      process.exit(signal === "SIGINT" ? 130 : 143);
    });
  });
}
process.on("exit", () => { terminateActiveExecutionGroups(); });

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
    taskId?: string;
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
      task_id: z.string().min(1).optional().describe("existing Fabric task to link"),
      output_path: z.string().min(1).optional().describe("opaque output or run path metadata"),
    },
  },
  ({ to, body, kind, reply_to, task_id, output_path }) =>
    reply(readyStore().send(who, to, body, {
      kind,
      replyTo: reply_to,
      taskId: task_id,
      outputPath: output_path,
    })),
);

server.registerTool(
  "fabric_inbox",
  {
    description:
      "Claim my unacknowledged messages. Peek observes without claiming; expired claims redeliver. " +
      "Set wait_seconds for one bounded wait inside this MCP call; task_id limits the same atomic " +
      "claim/peek to one Fabric task; never poll SQLite or start a watcher.",
    inputSchema: {
      limit: z.number().int().positive().optional(),
      peek: z.boolean().optional(),
      claim_seconds: z.number().int().min(1).max(3600).optional(),
      task_id: z.string().min(1).optional(),
      wait_seconds: z.number().int().min(0).max(MAX_WAIT_SECONDS).optional(),
    },
  },
  async ({ limit, peek, claim_seconds, task_id, wait_seconds }, { signal }) =>
    reply(await waitForInbox({
      limit,
      peek,
      claimTtlMs: claim_seconds === undefined ? undefined : claim_seconds * 1000,
      taskId: task_id,
    }, (wait_seconds ?? 0) * 1000, signal)),
);

// The whole routing surface: who runs it, which route, and how much access it
// gets. Assurance selectors are absent rather than accepted and ignored, and the
// schemas are strict, so a removed parameter is a typed input error.
const routeInputSchema = {
  // An enum, not a free string: an adapter the dispatcher cannot execute is a
  // typed schema error here, before any run directory exists.
  adapter: z.enum(DISPATCH_ADAPTERS).optional()
    .describe("provider adapter; defaults to the current Fabric seat"),
  alias: z.string().min(1).optional().describe("flagship, workhorse (default), scout, or a unique model name"),
  model: z.string().min(1).optional().describe("explicit model id; use instead of alias (required for brokers)"),
  effort: z.string().min(1).optional().describe("low, medium, high, xhigh, max or ultra; router validates model support"),
  mode: z.enum(["read_only", "worktree_write"]).optional()
    .describe("read_only (default), or worktree_write with the worktree the worker owns"),
  worktree: z.string().min(1).optional()
    .describe("Git worktree root the worker owns exclusively; requires mode worktree_write"),
};

const batchTaskSchema = z.strictObject({
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
      "Run a prompt (or workspace prompt_file) on an adapter with optional alias or model, effort, and mode read_only or worktree_write (+worktree). " +
      "Wait up to 55 seconds, then use fabric_status; full output stays in files and fabric_adapters lists read-only guarantees.",
    inputSchema: z.strictObject({
      prompt: z.string().optional(),
      prompt_file: z.string().min(1).optional(),
      task_id: z.string().regex(/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/u).optional(),
      timeout_seconds: z.number().positive().finite().optional(),
      wait_seconds: z.number().int().min(0).max(MAX_WAIT_SECONDS).optional(),
      ...routeInputSchema,
    }),
  },
  async (input, { signal }) => reply(await dispatchConfiguredProvider(input, who, signal)),
);

server.registerTool(
  "fabric_batch",
  {
    description:
      "Run 1–64 tasks: each takes prompt or prompt_file, adapter, optional alias or model, effort, and mode read_only or worktree_write (+distinct worktree). " +
      "All tasks validate before launch; wait up to 55 seconds, then use fabric_status for compact results.",
    inputSchema: z.strictObject({
      tasks: z.array(batchTaskSchema).min(1).max(64),
      concurrency: z.number().int().min(1).max(8).optional(),
      wait_seconds: z.number().int().min(0).max(MAX_WAIT_SECONDS).optional(),
    }),
  },
  async (input, { signal }) => reply(await dispatchConfiguredBatch(input, who, signal)),
);

server.registerTool(
  "fabric_status",
  {
    description: "Read a task_id, batch_id or run_dir; omit id for up to 20 workspace runs from the last 24 hours. Wait up to 55 seconds for completion; reports liveness, silence and result path without changing runs.",
    inputSchema: { id: z.string().min(1).optional(), wait_seconds: z.number().int().min(0).max(55).optional() },
  },
  async ({ id, wait_seconds }, { signal }) => reply(await fabricStatus(who.cwd, id, wait_seconds, signal)),
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
      task_id: z.string().min(1).optional(),
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

server.registerTool(
  "fabric_adapters",
  {
    description:
      "List configured providers from the product catalogue: dispatch state, aliases, " +
      "read-only guarantee, writable modes and endpoint profiles. Read-only and store-free; " +
      "everything fabric_dispatch accepts is answerable from one call. Use the adapter name " +
      "and optionally an alias directly in fabric_dispatch.",
    inputSchema: {},
  },
  () => {
    const snapshot = catalogueSnapshot();
    return reply(snapshot.adapters.length === 0
      ? { error: "adapter catalogue unavailable", adapters: [], endpoints: {} }
      : snapshot);
  },
);

const transport = new StdioServerTransport();
process.stdin.once("end", () => { void transport.close(); });
await server.connect(transport);
