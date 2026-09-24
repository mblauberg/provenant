import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { setTimeout as delay } from "node:timers/promises";
import { z } from "zod";

import { databasePath, identify } from "./identity.js";
import { statusRows, fabricOutput } from "./run-registry.js";
import { reply, digest, serverBuild, mailboxView, adapterView, runView } from "./surface.js";
import { catalogueSnapshot } from "./catalogue.js";
import { handoffDispatch, resumeConfiguredProvider } from "./resume.js";
import {
  cancelActiveExecutions,
  cancelConfiguredRun,
  dispatchConfiguredBatch,
  dispatchConfiguredProvider,
  MAX_EXECUTION_WAIT_SECONDS,
  terminateActiveExecutionGroups,
} from "./execution.js";
import { isSQLiteContention, Store, type Message } from "./store.js";
import { readEvents, readRuns } from "./run-reader.js";
import { canonicalMode, editDistance, ACCESS_MODES } from "./execution-input.js";

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
const executionIdentity = () => ({ ...who, registeredProjects: readyStore().projects() });

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
const ids = (maximum?: number) => z.union([
  maximum === undefined ? z.array(z.string()) : z.array(z.string()).max(maximum), z.string(),
]).transform((value) => typeof value === "string" ? [value] : value).optional();
const wait = z.unknown().optional();
const optionalNumber = z.unknown().optional();
const route = {
  adapter: str,
  alias: str,
  model: str,
  effort: str,
  mode: z.enum(ACCESS_MODES).optional(),
  worktree: str,
  cwd: str,
  network: z.boolean().optional(),
  sandbox: z.enum(["read-only", "workspace-write", "full"]).optional(),
  add_dirs: z.array(z.string()).optional(),
  allow_secrets: z.boolean().optional(),
  fallback: z
    .union([z.boolean(), z.literal("any"), z.array(z.union([z.string(), z.record(z.string(), z.unknown())]))])
    .optional(),
};
const task = { prompt: str, prompt_file: str, timeout_seconds: optionalNumber, ...route };
const taskFields = { id: str, ...task };
const batch = {
  tasks: z.array(z.object(taskFields).catchall(z.unknown()).meta({ additionalProperties: false })).min(1).max(64),
  concurrency: optionalNumber,
  wait_seconds: wait,
};
const INPUT_SYNONYMS: Record<string, string> = {
  prompt_path: "prompt_file", file: "prompt_file",
  dir: "cwd", path: "cwd", task: "task_id",
};
const inputKey = (text: string) => text.toLowerCase().replace(/[^a-z0-9]/gu, "");
function normaliseInputKeys(name: string, input: Record<string, any>, fields: string[], nestedTask = false) {
  const aliases = nestedTask ? { ...INPUT_SYNONYMS, task_id: "id", task: "id" } : INPUT_SYNONYMS;
  const result: Record<string, any> = {};
  const warnings: string[] = [];
  for (const [key, value] of Object.entries(input)) {
    const canonical = fields.find((field) => inputKey(field) === inputKey(key));
    const synonym = Object.entries(aliases).find(([alias]) => inputKey(alias) === inputKey(key))?.[1];
    let target = canonical ?? synonym;
    if (!target || !fields.includes(target)) {
      const matches = fields.map((field) => ({ field, score: editDistance(inputKey(key), inputKey(field)) }))
        .sort((a, b) => a.score - b.score);
      const closest = matches.filter((match) => match.score === matches[0]?.score && match.score <= 3).map((match) => match.field);
      throw { status: "rejected", error: "argument_unknown", fix: `Unknown ${name} field ${key}; closest valid field${closest.length === 1 ? "" : "s"}: ${closest.join(", ") || fields.join(", ")}.` };
    }
    if (Object.hasOwn(result, target))
      throw { status: "rejected", error: "argument_ambiguous", fix: `Pass only one of the fields that map to ${target}.` };
    result[target] = target === "mode" && typeof value === "string" ? canonicalMode(value) : value;
    if (target !== key) warnings.push(`corrected ${key} to ${target}`);
  }
  if ((name === "fabric_dispatch" || name === "fabric_batch") && Array.isArray(result.tasks))
    result.tasks = result.tasks.map((item: unknown, index: number) => {
      if (!item || typeof item !== "object" || Array.isArray(item))
        return { id: `task-${index + 1}`, prompt: "", prompt_file: "" };
      const rawTask = item as Record<string, any>;
      try { return normaliseInputKeys(name, rawTask, ["id", ...Object.keys(task)], true).value; }
      catch {
        const id = rawTask.id ?? rawTask.task_id ?? rawTask.task;
        return { id: typeof id === "string" ? id : `task-${index + 1}`, prompt: "", prompt_file: "" };
      }
    });
  return { value: result, warnings };
}
// Catch domain errors at the boundary; SDK validation errors retain its protocol error envelope.
function register(
  name: string,
  description: string,
  schema: z.ZodRawShape,
  handler: (input: any, extra: any) => unknown | Promise<unknown>,
) {
  const accepted = { ...schema };
  if (Object.hasOwn(accepted, "mode")) accepted.mode = z.string().optional().meta({ enum: ACCESS_MODES });
  if (name === "fabric_dispatch" || name === "fabric_batch") {
    const acceptedTask = { id: str, ...task, mode: z.string().optional().meta({ enum: ACCESS_MODES }) };
    const acceptedTasks = z.array(z.object(acceptedTask).catchall(z.unknown()).meta({ additionalProperties: false }))
      .min(1).max(64);
    accepted.tasks = name === "fabric_batch" ? acceptedTasks : acceptedTasks.optional();
  }
  const inputSchema = z.object(accepted).catchall(z.unknown()).meta({ additionalProperties: false });
  server.registerTool(name, { description, inputSchema }, async (rawInput, extra) => {
    const includeStructuredContent =
      !["fabric_dispatch", "fabric_status"].includes(name) || rawInput.detail === "full";
    try {
      let corrected;
      try { corrected = normaliseInputKeys(name, rawInput, Object.keys(schema)); }
      catch (error) { return reply(error as Record<string, unknown>, includeStructuredContent); }
      const strictFields = { ...schema };
      if (name === "fabric_dispatch") strictFields.tasks = z.array(z.strictObject(taskFields)).min(1).max(64).optional();
      if (name === "fabric_batch") strictFields.tasks = z.array(z.strictObject(taskFields)).min(1).max(64);
      const parsed = z.strictObject(strictFields).safeParse(corrected.value);
      if (!parsed.success) return reply({ status: "rejected", error: "invalid_input", fix: parsed.error.issues[0]?.message ?? "Check the supplied fields." }, includeStructuredContent);
      const input = parsed.data as any;
      const result = await handler(input, extra) as Record<string, any>;
      const warning = corrected.warnings.length ? [`warning: ${corrected.warnings.join("; ")}`] : [];
      return reply(warning.length ? withWarnings(result, warning) : result, includeStructuredContent);
    } catch (error) {
      return {
        ...reply({
          status: "rejected",
          error: "request_failed",
          fix: String(error instanceof Error ? error.message : error).replace(/\s+/gu, " "),
        }, includeStructuredContent),
        isError: true,
      };
    }
  });
}
function boundedWait(value: unknown): { value?: number; warnings: string[]; error?: Record<string, string> } {
  if (value === undefined) return { warnings: [] };
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0)
    return { warnings: [], error: { status: "rejected", error: "wait_invalid", fix: "Pass wait_seconds from 0 to 55." } };
  if (value > MAX_WAIT_SECONDS)
    return { value: MAX_WAIT_SECONDS, warnings: [`! wait_seconds ${value} clamped to ${MAX_WAIT_SECONDS}`] };
  return { value, warnings: [] };
}
function validateDispatchNumbers(input: Record<string, any>): Record<string, string> | undefined {
  for (const candidate of [input, ...(Array.isArray(input.tasks) ? input.tasks : [])]) {
    if (candidate.timeout_seconds !== undefined &&
      (typeof candidate.timeout_seconds !== "number" || !Number.isFinite(candidate.timeout_seconds) || candidate.timeout_seconds <= 0))
      return { status: "rejected", error: "timeout_invalid", fix: "Pass timeout_seconds as a finite positive number." };
  }
  if (input.concurrency !== undefined &&
    (typeof input.concurrency !== "number" || !Number.isInteger(input.concurrency) || input.concurrency < 1))
    return { status: "rejected", error: "concurrency_invalid", fix: "Pass concurrency as an integer from 1 to 8." };
  return undefined;
}
function withWarnings<T extends Record<string, any>>(value: T, warnings: string[]): T {
  if (!warnings.length) return value;
  return { ...value, warnings: [...(value.warnings ?? []), ...warnings] };
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
    ids: ids(100),
    claim: z.boolean().optional(),
    limit: z.number().int().min(1).max(100).optional(),
    wait_seconds: wait,
    digest: z.boolean().optional(),
  },
  async ({ ids, claim, peek: explicitPeek, task_id, claim_seconds, limit, wait_seconds, digest }, { signal }) => {
    if (digest) {
      if (ids !== undefined || claim === true) return { status: "rejected", error: "digest_conflict", fix: "Fetch message bodies by id in a separate inbox call." };
      return readyStore().inboxDigest(who, task_id);
    }
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
register(
  "fabric_runs",
  "Versioned, bounded run and lane reader with root-relative paths.",
  { ids: ids(20), wait_seconds: wait },
  async ({ ids, wait_seconds }, { signal }) => {
    const bounded = boundedWait(wait_seconds);
    if (bounded.error) return bounded.error;
    return readRuns(who.cwd, ids, bounded.value, signal);
  },
);
register(
  "fabric_events",
  "Read task terminal/input_required and inbox events; pass cursor to wait for new events.",
  { cursor: z.string().max(8192).optional(), wait_seconds: wait },
  async ({ cursor, wait_seconds }, { signal }) => {
    const bounded = boundedWait(wait_seconds);
    if (bounded.error) return bounded.error;
    const deadline = Date.now() + (bounded.value ?? 0) * 1000;
    for (;;) {
      signal.throwIfAborted();
      const result = await readEvents(who.cwd, cursor, readyStore().inbox(who, { peek: true, limit: 100 }));
      if (result.status !== "ok" || result.events.length || Date.now() >= deadline) return result;
      await delay(Math.min(250, deadline - Date.now()), undefined, { signal });
    }
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
  "Run one prompt or tasks; resume or hand off a run. wait_seconds above 55 clamps to 55.",
  {
    ...task,
    task_id: str,
    tasks: batch.tasks.optional(),
    concurrency: batch.concurrency,
    resume: str,
    handoff: str,
    context_ceiling: z.number().optional(),
    wait_seconds: wait,
    detail,
  },
  async (input, { signal }) => {
    const waitResult = boundedWait(input.wait_seconds);
    if (waitResult.error) return waitResult.error;
    const numericError = validateDispatchNumbers(input);
    if (numericError) return numericError;
    if ([input.tasks, input.resume, input.handoff].filter(Boolean).length > 1 || (input.tasks && (input.prompt || input.prompt_file)))
      return { status: "rejected", error: "dispatch_conflict", fix: "Pass one prompt, tasks, resume or handoff." };
    input.wait_seconds = waitResult.value;
    if (input.concurrency !== undefined && input.concurrency > 8) {
      const requestedConcurrency = input.concurrency;
      input.concurrency = 8;
      waitResult.warnings.push(`! concurrency ${requestedConcurrency} clamped to 8`);
    }
    const result = input.resume
      ? await resumeConfiguredProvider(input, who, signal)
      : input.handoff
        ? await handoffDispatch(input, who, signal)
        : input.tasks
        ? await dispatchConfiguredBatch({ ...input, wait_seconds: input.wait_seconds ?? 0 }, executionIdentity(), signal)
        : await dispatchConfiguredProvider(input, executionIdentity(), signal);
    if (!result.id) return withWarnings(result, waitResult.warnings);
    const observed = await statusRows(who.cwd, [String(result.id)], 0, "all", signal, input.detail);
    if (input.resume && result.task_id) observed.runs = observed.runs?.filter((row) => row.task_id === result.task_id);
    acknowledgeRuns(observed);
    if (observed.runs?.length === 1) {
      const row = observed.runs[0]!;
      return runView(withWarnings({ ...result, ...row, paths: { ...(result.paths as object), ...row.paths } }, waitResult.warnings), input.detail);
    }
    const value = observed.runs ? {
      ...observed,
      ...(result.tasks === undefined ? {} : { tasks: result.tasks }),
      ...(result.warnings === undefined ? {} : { warnings: result.warnings }),
    } : result;
    return runView(withWarnings(value, waitResult.warnings), input.detail);
  },
);
register(
  "fabric_status",
  "Read or wait for runs. wait_seconds above 55 is clamped to 55; invalid numeric values return a typed rejection.",
  {
    ids: ids(),
    id: str,
    wait_seconds: wait,
    until: z.enum(["any", "all"]).optional(),
    detail,
  },
  async ({ ids, id, wait_seconds, until, detail }, { signal }) => {
    const waitResult = boundedWait(wait_seconds);
    if (waitResult.error) return waitResult.error;
    const result = await statusRows(who.cwd, ids ?? (id ? [id] : undefined), waitResult.value, until, signal, detail);
    acknowledgeRuns(result);
    const view = runView(withWarnings(result, waitResult.warnings), detail);
    // Claims are an addendum; run status must stay readable when the store cannot open.
    let workClaims: ReturnType<Store["workClaims"]> = [];
    let landingLease: ReturnType<Store["landingLease"]> | undefined;
    try {
      const ownershipStore = readyStore();
      workClaims = ownershipStore.workClaims(who.project);
      landingLease = ownershipStore.landingLease(who.project);
    } catch {
      // Fall through with no ownership rows.
    }
    const ownership = [
      ...workClaims.map((claim) => `claim ${claim.issue ?? claim.paths.join(",")} ${claim.holder} g${claim.generation}`),
      ...(landingLease ? [`landing ${landingLease.holder} g${landingLease.generation} ${landingLease.expectedSha}`] : []),
    ];
    return { ...view,
      ...(detail === "full" || ownership.length ? { work_claims: workClaims, landing_lease: landingLease } : {}),
      ...(ownership.length ? { digest: `${digest(view)}\n${ownership.join("\n")}` } : {}) };
  },
);
register("fabric_work_claim", "Acquire, renew, verify or release an advisory issue or path claim.", {
  action: z.enum(["acquire", "renew", "verify", "release"]), session_id: z.string().min(1),
  issue: str, paths: z.array(z.string()).optional(), id: str,
  generation: z.number().int().positive().optional(), seconds: z.number().int().min(1).max(3600).optional(),
}, ({ action, session_id, issue, paths, id, generation, seconds }) => {
  if (action === "acquire") return readyStore().acquireWork(who, session_id, { issue, paths }, seconds ?? 900);
  if (!id || generation === undefined) throw new Error("id and generation are required");
  if (action === "renew") return readyStore().renewWork(who, session_id, id, generation, seconds ?? 900);
  if (action === "verify") return readyStore().verifyWork(who, session_id, id, generation);
  readyStore().releaseWork(who, session_id, id, generation);
  return { released: id, generation };
});
register("fabric_landing_lease", "Acquire, renew, verify or release the repository landing lease.", {
  action: z.enum(["acquire", "renew", "verify", "release"]), session_id: z.string().min(1),
  expected_sha: str, generation: z.number().int().positive().optional(),
  seconds: z.number().int().min(1).max(3600).optional(),
}, ({ action, session_id, expected_sha, generation, seconds }) => {
  if (action === "acquire") {
    if (!expected_sha) throw new Error("expected_sha is required");
    return readyStore().acquireLanding(who, session_id, expected_sha, seconds ?? 300);
  }
  if (generation === undefined) throw new Error("generation is required");
  if (action === "renew") return readyStore().renewLanding(who, session_id, generation, seconds ?? 300);
  if (action === "verify") {
    if (!expected_sha) throw new Error("expected_sha is required");
    return readyStore().verifyLanding(who, session_id, generation, expected_sha);
  }
  readyStore().releaseLanding(who, session_id, generation);
  return { released: generation };
});
register("fabric_cancel", "Stop a run and its provider group.", { id: z.string(), reason: str }, async ({ id, reason }) => {
  const result = await cancelConfiguredRun(id, who, reason);
  acknowledgeRuns(result);
  return runView(result);
});
register(
  "fabric_output",
  "Read output; continue at next_offset. max_bytes above 20000 is clamped with a warning; invalid numeric values return a typed rejection.",
  {
    id: z.string(),
    part: z.enum(["result", "stderr", "events", "receipt"]).optional(),
    offset: z.number().int().nonnegative().optional(),
    tail: z.boolean().optional(),
    max_bytes: optionalNumber,
  },
  async (input) => {
    if (input.max_bytes !== undefined &&
      (typeof input.max_bytes !== "number" || !Number.isInteger(input.max_bytes) || input.max_bytes < 1))
      return { status: "rejected", error: "max_bytes_invalid", fix: "Pass max_bytes as an integer from 1 to 20000." };
    const requested = input.max_bytes;
    const result = await fabricOutput(who.cwd, { ...input, max_bytes: requested === undefined ? undefined : Math.min(requested, 20000) });
    return requested !== undefined && requested > 20000
      ? withWarnings(result, [`! max_bytes ${requested} clamped to 20000`])
      : result;
  },
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
    dispatchConfiguredBatch(input, executionIdentity(), signal),
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
