import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { setTimeout as delay } from "node:timers/promises";
import { z } from "zod";

import { databasePath, identify } from "./identity.js";
import { statusRows, fabricOutput } from "./run-registry.js";
import { reply, serverBuild, mailboxView, adapterView, runView } from "./surface.js";
import { catalogueSnapshot } from "./catalogue.js";
import {
  cancelActiveExecutions,
  resumeConfiguredProvider,
  cancelConfiguredRun,
  dispatchConfiguredBatch,
  dispatchConfiguredProvider,
  MAX_EXECUTION_WAIT_SECONDS,
  terminateActiveExecutionGroups,
} from "./execution.js";
import { isSQLiteContention, Store, type Message } from "./store.js";

// Leave margin under the MCP SDK's 60-second default request timeout.
const MAX_WAIT_SECONDS = MAX_EXECUTION_WAIT_SECONDS;

/**
 * One MCP process per agent; announce at startup and retry lazily on contention.
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
      "Dispatch prompt or tasks (same prompt/route fields per task, optional id); status waits up to 55s. Copy the returned Route line for provenance. " +
      "Full output stays in files; output reads bounded slices. Inbox peeks; claim ids, then acknowledge after processing. " +
      "Writers require an owned worktree. Resume answers input_required. detail:full expands metadata.",
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
process.on("exit", () => {
  terminateActiveExecutionGroups();
});

const waitForInbox = async (
  options: {
    ids?: string[];
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
      const busyTimeoutMs = waitMs === 0 ? undefined : 1;
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

const detail = z.enum(["brief", "full"]).optional();
const str = z.string().optional();
const wait = z.number().int().min(0).max(55).optional();
const route = {
  adapter: str,
  alias: str,
  model: str,
  effort: str,
  mode: z.enum(["read_only", "worktree_write"]).optional(),
  worktree: str,
  cwd: str,
  network: z.boolean().optional(),
  sandbox: z.enum(["read-only", "workspace-write", "full"]).optional(),
  add_dirs: z.array(z.string()).optional(),
  fallback: z
    .union([z.boolean(), z.literal("any"), z.array(z.union([z.string(), z.record(z.string(), z.unknown())]))])
    .optional(),
};
const task = { prompt: str, prompt_file: str, timeout_seconds: z.number().positive().optional(), ...route };
const batch = {
  tasks: z
    .array(z.record(z.string(), z.unknown()).pipe(z.strictObject({ id: str, ...task })))
    .min(1)
    .max(64),
  concurrency: z.number().int().min(1).max(8).optional(),
  wait_seconds: wait,
};
// Catch domain errors at the boundary; SDK validation errors retain its protocol error envelope.
function register(
  name: string,
  description: string,
  schema: z.ZodRawShape,
  handler: (input: any, extra: any) => unknown | Promise<unknown>,
) {
  server.registerTool(name, { description, inputSchema: z.strictObject(schema) }, async (input, extra) => {
    try {
      return reply(await handler(input, extra));
    } catch (error) {
      return {
        ...reply({
          status: "rejected",
          error: "request_failed",
          fix: String(error instanceof Error ? error.message : error).replace(/\s+/gu, " "),
        }),
        isError: true,
      };
    }
  });
}
register("fabric_whoami", "Identify this seat and server.", { detail }, ({ detail }) => ({
  ...who,
  ...serverBuild(),
  database: (readyStore(), databasePath()),
  ...(detail === "full" ? { agents: readyStore().agents(who.project) } : {}),
}));
register(
  "fabric_send",
  "Send to a seat, team, chair or all.",
  { to: z.string(), body: z.string(), kind: str, reply_to: str, task_id: str, output_path: str },
  ({ to, body, kind, reply_to, task_id, output_path }) =>
    readyStore().send(who, to, body, { kind, replyTo: reply_to, taskId: task_id, outputPath: output_path }),
);
register(
  "fabric_inbox",
  "Peek headers or claim bodies.",
  {
    peek: z.boolean().optional(),
    task_id: str,
    claim_seconds: z.number().int().min(1).max(3600).optional(),
    ids: z.array(z.string()).max(100).optional(),
    claim: z.boolean().optional(),
    limit: z.number().int().min(1).max(100).optional(),
    wait_seconds: wait,
  },
  async ({ ids, claim, peek: explicitPeek, task_id, claim_seconds, limit, wait_seconds }, { signal }) => {
    const peek = explicitPeek ?? (ids === undefined && claim !== true);
    const rows = await waitForInbox(
      {
        ids,
        limit: limit ?? ids?.length ?? 10,
        peek,
        taskId: task_id,
        claimTtlMs: claim_seconds === undefined ? undefined : claim_seconds * 1000,
      },
      (wait_seconds ?? 0) * 1000,
      signal,
    );
    return { messages: rows.map((row) => mailboxView(row, peek)) };
  },
);
function acknowledgeRuns(result: { runs?: Record<string, any>[] }) {
  for (const row of result.runs ?? []) if (row.state === "terminal") {
    try { readyStore(1).acknowledgeTerminal(who, row, 1); }
    catch { /* Mailbox contention must not hide durable run evidence. */ }
  }
}
register(
  "fabric_dispatch",
  "Run one prompt, tasks, or resume a run.",
  {
    ...task,
    task_id: str,
    tasks: batch.tasks.optional(),
    concurrency: batch.concurrency,
    resume: str,
    wait_seconds: wait,
    detail,
  },
  async (input, { signal }) => {
    if (input.tasks && (input.prompt || input.prompt_file || input.resume))
      return { status: "rejected", error: "dispatch_conflict", fix: "Pass one prompt, tasks, or resume." };
    const result = input.resume
      ? await resumeConfiguredProvider(input, who, signal)
      : input.tasks
        ? await dispatchConfiguredBatch({ ...input, wait_seconds: input.wait_seconds ?? 0 }, who, signal)
        : await dispatchConfiguredProvider(input, who, signal);
    if (!result.id) return result;
    const observed = await statusRows(who.cwd, [String(result.id)], 0, "all", signal, input.detail);
    acknowledgeRuns(observed);
    if (observed.runs?.length === 1) {
      const row = observed.runs[0]!;
      return runView({ ...result, ...row, paths: { ...(result.paths as object), ...row.paths } }, input.detail);
    }
    return runView(observed.runs ? observed : result, input.detail);
  },
);
register(
  "fabric_status",
  "Read or wait for runs.",
  {
    ids: z.array(z.string()).optional(),
    id: str,
    wait_seconds: wait,
    until: z.enum(["any", "all"]).optional(),
    detail,
  },
  async ({ ids, id, wait_seconds, until, detail }, { signal }) => {
    const result = await statusRows(who.cwd, ids ?? (id ? [id] : undefined), wait_seconds, until, signal, detail);
    acknowledgeRuns(result);
    return runView(result, detail);
  },
);
register("fabric_cancel", "Stop a run and its provider group.", { id: z.string(), reason: str }, async ({ id, reason }) => {
  const result = await cancelConfiguredRun(id, who, reason);
  acknowledgeRuns(result);
  return runView(result);
});
register(
  "fabric_output",
  "Read output; continue at next_offset.",
  {
    id: z.string(),
    part: z.enum(["result", "stderr", "events", "receipt"]).optional(),
    offset: z.number().int().nonnegative().optional(),
    tail: z.boolean().optional(),
    max_bytes: z.number().int().min(1).max(20000).optional(),
  },
  (input) => fabricOutput(who.cwd, input),
);
register(
  "fabric_acknowledge",
  "Acknowledge a claimed delivery.",
  { message_id: z.string(), claim_id: z.string() },
  ({ message_id, claim_id }) => readyStore().acknowledge(who, message_id, claim_id),
);
register(
  "fabric_task",
  "Create, claim, update or list tasks.",
  {
    action: z.enum(["create", "claim", "update", "list"]),
    task_id: str,
    objective: str,
    owner: str,
    depends_on: z.array(z.string()).optional(),
    state: str,
    note: str,
  },
  ({ action, task_id, objective, owner, depends_on, state, note }) => {
    if (action === "list") return { tasks: readyStore().tasks(who.project, state) };
    if (action === "create" && objective)
      return readyStore().createTask(who, objective, { taskId: task_id, owner, dependsOn: depends_on });
    if (action === "claim" && task_id) return readyStore().claimTask(who, task_id);
    if (action === "update" && task_id && state) return readyStore().updateTask(who, task_id, state, note);
    throw new Error("Supply objective for create; task_id for claim; task_id and state for update.");
  },
);
register("fabric_note", "Append an activity note.", { detail: z.string() }, ({ detail }) => {
  readyStore().note(who, detail);
  return { noted: detail };
});
register(
  "fabric_activity",
  "Read recent activity or entries after a cursor.",
  { limit: z.number().int().min(1).max(100).optional(), after_seq: z.number().int().nonnegative().optional() },
  ({ limit, after_seq }) => ({
    activity:
      after_seq === undefined
        ? readyStore().activity(who.project, limit ?? 20)
        : readyStore().activityAfter(who.project, after_seq, limit ?? 20),
  }),
);
register("fabric_adapters", "List routes and guarantees; full includes profiles.", { detail }, ({ detail }) =>
  adapterView(catalogueSnapshot(), detail),
);
if (process.env.FABRIC_LEGACY_TOOLS === "1") {
  register("fabric_batch", "Run a task batch.", batch, (input, { signal }) =>
    dispatchConfiguredBatch(input, who, signal),
  );
  register(
    "fabric_team_create",
    "Create a team.",
    { team_id: z.string(), members: z.array(z.string()) },
    ({ team_id, members }) => readyStore().createTeam(who, team_id, members),
  );
  register(
    "fabric_task_create",
    "Create a task.",
    { objective: z.string(), task_id: str, owner: str, depends_on: z.array(z.string()).optional() },
    ({ objective, task_id, owner, depends_on }) =>
      readyStore().createTask(who, objective, { taskId: task_id, owner, dependsOn: depends_on }),
  );
  register("fabric_task_claim", "Claim a task.", { task_id: z.string() }, ({ task_id }) =>
    readyStore().claimTask(who, task_id),
  );
  register(
    "fabric_task_update",
    "Update a task.",
    { task_id: z.string(), state: z.string(), note: str },
    ({ task_id, state, note }) => readyStore().updateTask(who, task_id, state, note),
  );
  register("fabric_tasks", "List tasks.", { state: str }, ({ state }) => readyStore().tasks(who.project, state));
}
try {
  initialiseStore(1);
} catch (error) {
  console.error(`fabric: presence deferred: ${String(error)}`);
}
const transport = new StdioServerTransport();
// SDK schema failures happen before tool callbacks; keep their presentation consistent.
const send = transport.send.bind(transport);
transport.send = async (message) => {
  if ("result" in message && message.result.isError === true) {
    const result = message.result;
    const blocks = Array.isArray(result.content) ? result.content : [];
    const text = blocks
      .map((block) => (typeof block.text === "string" ? block.text : ""))
      .join(" ")
      .replace(/\s+/gu, " ");
    const line = text.includes("fix:") ? text : `rejected invalid_input · fix: ${text}`;
    message = {
      ...message,
      result: {
        ...result,
        content: [{ type: "text", text: line }],
        structuredContent: result.structuredContent ?? { status: "rejected", error: "invalid_input", fix: text },
      },
    };
  }
  return send(message);
};
process.stdin.once("end", () => {
  void transport.close();
});
await server.connect(transport);
