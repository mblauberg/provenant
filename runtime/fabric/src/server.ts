import { actAsMcpServer } from "./seat-index.js";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { setTimeout as delay } from "node:timers/promises";
import { z } from "zod";

import { federatedLane, fabricHosts, releaseHostChecks } from "./hosts.js";
import { CommitShaError, databasePath, identify, resolveCommitSha } from "./identity.js";
import { reapAttemptRuns } from "./process-identity.js";
import { federatedStatusRows as statusRows, fabricOutput, resultTail } from "./run-status.js";
import { reply, digest, serverBuild, mailboxView, adapterView, runView, fullView, lanesDigest, cancelDigest } from "./surface.js";
import { catalogueSnapshot, liveModels, resetCooldowns } from "./catalogue.js";
import { handoffDispatch, resumeConfiguredProvider } from "./resume.js";
import { reconcileSession, sessionDispatch, sessionName, sessionView } from "./sessions.js";
import {
  cancelConfiguredRun,
  dispatchConfiguredBatch,
  dispatchConfiguredProvider,
  MAX_EXECUTION_WAIT_SECONDS,
  releaseActiveExecutions,
} from "./execution.js";
import { isSQLiteContention, Store, type Message } from "./store.js";
import { readEvents, readRuns } from "./run-reader.js";
import { canonicalMode, editDistance, rejected, ACCESS_MODES } from "./execution-input.js";
import { taskFields, taskShape, toolSchemas } from "./tool-schemas.js";

// Leave margin under the MCP SDK's 60-second default request timeout.
const MAX_WAIT_SECONDS = MAX_EXECUTION_WAIT_SECONDS;

/**
 * One MCP process per agent; announce at startup and retry lazily on contention.
 *
 * There is no handshake to fail, so there is no reconnect path, so there is no
 * class of error that reports "the daemon is unavailable" while the daemon is
 * running. If the file cannot be opened the caller is told which file and why.
 */
actAsMcpServer();
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
      "Writers require an owned worktree. Resume answers input_required. detail:full expands metadata. " +
      "To block on running lanes use the shell, not polling: provenant lanes --wait --all --timeout 900.",
  },
);
/**
 * A host restart must not end the runs it started. Each owner is a detached
 * session leader, so closing hands every run to its own owner instead of
 * cancelling it; the next host reads those runs from their records.
 */
server.server.onclose = async () => {
  releaseHostChecks();
  releaseActiveExecutions();
  store?.close();
  store = undefined;
};

/**
 * An external signal never reaches `onclose`, so the same hand-over is bound to
 * the signals a supervisor actually sends. SIGKILL cannot be caught at all: a
 * run left that way is reaped by the next dispatch in the same workspace.
 */
let shuttingDown = false;
// Lanes whose result tail this session has already shown; bounded, oldest evicted first.
const tailedLanes = new Set<string>();
for (const signal of ["SIGTERM", "SIGINT", "SIGHUP"] as const) {
  process.on(signal, () => {
    if (shuttingDown) return;
    shuttingDown = true;
    releaseHostChecks();
    releaseActiveExecutions();
    store?.close();
    store = undefined;
    process.exit(signal === "SIGINT" ? 130 : 143);
  });
}
process.on("exit", () => {
  releaseHostChecks();
  releaseActiveExecutions();
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

// The first listed target a tool actually has wins, so run_id is `id` on status and `ids` on runs.
const INPUT_SYNONYMS: Record<string, string[]> = {
  ttl_seconds: ["seconds"],
  prompt_path: ["prompt_file"], file: ["prompt_file"],
  dir: ["cwd"], path: ["cwd"], task: ["task_id", "id"],
  run_id: ["id", "ids"], task_id: ["id"], id: ["task_id", "ids"],
};
const NUMERIC_FIELDS = ["wait_seconds", "timeout_seconds", "concurrency", "max_bytes", "offset", "limit",
  "claim_seconds", "seconds", "context_ceiling", "generation", "after_seq", "tail_chars"];
const UNTYPED_NUMBERS = ["wait_seconds", "timeout_seconds", "concurrency", "max_bytes"];
const numericText = (value: unknown): value is string => typeof value === "string" && /^\s*\d+(\.\d+)?\s*$/u.test(value);
/** Numeric strings become numbers and a stray `until` is read for what it meant, each with a warning. */
function coerceInput(input: Record<string, any>, warnings: string[]): Record<string, any> {
  const result = { ...input };
  const number = (owner: Record<string, any>, key: string) => {
    if (numericText(owner[key])) { warnings.push(`read ${key} "${owner[key].trim()}" as a number`); owner[key] = Number(owner[key]); }
  };
  for (const key of NUMERIC_FIELDS) number(result, key);
  if (Array.isArray(result.tasks))
    result.tasks = result.tasks.map((item: unknown, index: number) => {
      if (!item || typeof item !== "object" || Array.isArray(item)) return item;
      const copy = { ...(item as Record<string, any>) };
      const before = warnings.length;
      number(copy, "timeout_seconds");
      for (let at = before; at < warnings.length; at++) warnings[at] = `tasks[${index}]: ${warnings[at]}`;
      return copy;
    });
  if (result.until !== undefined && result.until !== "any" && result.until !== "all") {
    const seconds = typeof result.until === "number" ? String(result.until) : result.until;
    if (numericText(seconds) && result.wait_seconds === undefined) {
      result.wait_seconds = Number(seconds);
      warnings.push(`until is any or all; read ${String(seconds).trim()} as wait_seconds`);
    } else if (typeof result.until === "string" && ["any", "all"].includes(result.until.trim().toLowerCase())) {
      warnings.push(`normalised until "${result.until}"`);
      result.until = result.until.trim().toLowerCase();
      return result;
    } else warnings.push(`ignored until ${JSON.stringify(result.until)}; use any or all`);
    delete result.until;
  }
  return result;
}
const inputKey = (text: string) => text.toLowerCase().replace(/[^a-z0-9]/gu, "");
function normaliseInputKeys(name: string, input: Record<string, any>, fields: string[], nestedTask = false) {
  const aliases: Record<string, string[]> = nestedTask ? { ...INPUT_SYNONYMS, task_id: ["id"], task: ["id"] } : INPUT_SYNONYMS;
  const result: Record<string, any> = {};
  const warnings: string[] = [];
  for (const [key, value] of Object.entries(input)) {
    const canonical = fields.find((field) => inputKey(field) === inputKey(key));
    const synonym = Object.entries(aliases).find(([alias]) => inputKey(alias) === inputKey(key))?.[1]
      .find((candidate) => fields.includes(candidate));
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
  if (name === "fabric_dispatch" && Array.isArray(result.tasks))
    result.tasks = result.tasks.map((item: unknown, index: number) => {
      if (!item || typeof item !== "object" || Array.isArray(item))
        return { id: `task-${index + 1}`, prompt: "", prompt_file: "" };
      const rawTask = item as Record<string, any>;
      try {
        const nested = normaliseInputKeys(name, rawTask, ["id", ...Object.keys(taskShape)], true);
        warnings.push(...nested.warnings.map((text) => `tasks[${index}]: ${text}`));
        return nested.value;
      }
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
  if (Object.hasOwn(accepted, "capabilities")) accepted.capabilities = z.unknown().optional()
    .meta({ description: "Array of distinct postgres or browser values.", type: "array", items: { type: "string", enum: ["postgres", "browser"] } }) as unknown as z.ZodRawShape[string];
  // Numeric strings and run_id/task_id spellings are normalised with a warning below, so the
  // protocol layer must not reject them first; the strict parse still enforces the real types.
  for (const key of NUMERIC_FIELDS.filter((field) => !UNTYPED_NUMBERS.includes(field)))
    if (Object.hasOwn(accepted, key)) accepted[key] = z.unknown().optional().meta({ type: "number" }) as unknown as z.ZodRawShape[string];
  if (Object.hasOwn(accepted, "until")) accepted.until = z.unknown().optional().meta({ enum: ["any", "all"] }) as unknown as z.ZodRawShape[string];
  if (["fabric_cancel", "fabric_output"].includes(name)) accepted.id = z.string().optional();
  if (name === "fabric_dispatch") {
    const acceptedTask = { id: taskFields.id, ...taskShape, mode: z.string().optional().meta({ enum: ACCESS_MODES }) };
    acceptedTask.capabilities = z.unknown().optional()
      .meta({ description: "Array of distinct postgres or browser values.", type: "array", items: { type: "string", enum: ["postgres", "browser"] } }) as unknown as typeof acceptedTask.capabilities;
    const acceptedTasks = z.array(z.object(acceptedTask).catchall(z.unknown()).meta({ additionalProperties: false }))
      .min(1).max(64);
    accepted.tasks = acceptedTasks.optional();
  }
  const inputSchema = z.object(accepted).catchall(z.unknown()).meta({ additionalProperties: false });
  server.registerTool(name, { description, inputSchema }, async (rawInput, extra) => {
    const includeStructuredContent =
      !["fabric_dispatch", "fabric_status", "fabric_cancel", "fabric_runs", "fabric_graph"].includes(name) || rawInput.detail === "full";
    try {
      let corrected;
      try { corrected = normaliseInputKeys(name, rawInput, Object.keys(schema)); }
      catch (error) { return reply(error as Record<string, unknown>, includeStructuredContent); }
      corrected.value = coerceInput(corrected.value, corrected.warnings);
      const strictFields = { ...schema };
      if (name === "fabric_dispatch") strictFields.tasks = z.array(z.strictObject(taskFields)).min(1).max(64).optional();
      const parsed = z.strictObject(strictFields).safeParse(corrected.value);
      if (!parsed.success) {
        if (parsed.error.issues.some((issue) => issue.path.includes("capabilities")))
          return reply({ status: "rejected", error: "capabilities_invalid", fix: "Pass capabilities as a list of distinct postgres or browser values." }, includeStructuredContent);
        const issue = parsed.error.issues[0];
        const where = issue?.path.length ? `${issue.path.join(".")}: ` : "";
        return reply({ status: "rejected", error: "invalid_input", fix: issue ? `${where}${issue.message}` : "Check the supplied fields." }, includeStructuredContent);
      }
      const input = parsed.data as any;
      const result = await handler(input, extra) as Record<string, any>;
      const warning = corrected.warnings.length ? [`warning: ${corrected.warnings.join("; ")}`] : [];
      return reply(warning.length ? withWarnings(result, warning) : result, includeStructuredContent);
    } catch (error) {
      return {
        ...reply({
          status: "rejected",
          error: error instanceof CommitShaError ? error.code : "request_failed",
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
register("fabric_whoami", "Identify this seat and server.",
  toolSchemas.fabric_whoami, ({ detail }) => ({
  ...who,
  ...serverBuild(),
  database: (readyStore(), databasePath()),
  ...(detail === "full" ? { agents: readyStore().agents(who.project) } : {}),
}));
register(
  "fabric_send",
  "Send to a seat, team, chair or all.",
  toolSchemas.fabric_send,
  ({ to, body, kind, reply_to, task_id, output_path }) =>
    readyStore().send(who, to, body, { kind, replyTo: reply_to, taskId: task_id, outputPath: output_path }),
);
register(
  "fabric_inbox",
  "Peek headers or claim bodies.",
  toolSchemas.fabric_inbox,
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
  "Versioned, bounded run and lane reader. state filters (running, terminal, active, ok, failed...); limit caps rows (default 20); detail:full adds the structured rows.",
  toolSchemas.fabric_runs,
  async ({ ids, wait_seconds, state, limit, detail }, { signal }) => {
    const bounded = boundedWait(wait_seconds);
    if (bounded.error) return bounded.error;
    const result = await readRuns(who.project, ids, bounded.value, signal, limit ?? 20, state);
    return { ...result, digest: lanesDigest(result) };
  },
);
register(
  "fabric_events",
  "Read task terminal/input_required and inbox events; pass cursor to wait for new events.",
  toolSchemas.fabric_events,
  async ({ cursor, wait_seconds }, { signal }) => {
    const bounded = boundedWait(wait_seconds);
    if (bounded.error) return bounded.error;
    const deadline = Date.now() + (bounded.value ?? 0) * 1000;
    for (;;) {
      signal.throwIfAborted();
      const result = await readEvents(who.project, cursor, readyStore().inbox(who, { peek: true, limit: 100 }));
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
  "Run one prompt or tasks; resume or hand off a run; session names a resumable conversation. wait_seconds above 55 clamps to 55.",
  toolSchemas.fabric_dispatch,
  async (input, { signal }) => {
    const waitResult = boundedWait(input.wait_seconds);
    if (waitResult.error) return waitResult.error;
    const numericError = validateDispatchNumbers(input);
    if (numericError) return numericError;
    if ([input.tasks, input.resume, input.handoff, input.session].filter(Boolean).length > 1 || (input.tasks && (input.prompt || input.prompt_file)))
      return { status: "rejected", error: "dispatch_conflict", fix: "Pass one prompt, tasks, resume, handoff or session." };
    if (input.fresh !== undefined && !input.session)
      return { status: "rejected", error: "dispatch_conflict", fix: "Pass fresh only with session." };
    input.wait_seconds = waitResult.value;
    if (input.concurrency !== undefined && input.concurrency > 8) {
      const requestedConcurrency = input.concurrency;
      input.concurrency = 8;
      waitResult.warnings.push(`! concurrency ${requestedConcurrency} clamped to 8`);
    }
    const result = input.session
      ? await sessionDispatch(input, executionIdentity(), readyStore(), signal)
      : input.resume
      ? await resumeConfiguredProvider(input, executionIdentity(), signal)
      : input.handoff
        ? await handoffDispatch(input, executionIdentity(), signal)
        : input.tasks
        ? await dispatchConfiguredBatch({ ...input, wait_seconds: input.wait_seconds ?? 0 }, executionIdentity(), signal)
        : await dispatchConfiguredProvider(input, executionIdentity(), signal);
    if (result.host) return runView(withWarnings(result, waitResult.warnings), input.detail);
    const runId = result.id ?? (input.session ? result.run_id : undefined);
    if (!runId) return withWarnings(result, waitResult.warnings);
    const observed = await statusRows(who.project, [String(runId)], 0, "all", signal, input.detail, true, 20, false, true);
    if ((input.resume || input.session) && result.task_id) observed.runs = observed.runs?.filter((row) => row.task_id === result.task_id);
    acknowledgeRuns(observed);
    if (observed.runs?.length === 1) {
      const row = observed.runs[0]!;
      const value = runView(withWarnings({ ...result, ...row, paths: { ...(result.paths as object), ...row.paths } }, waitResult.warnings), input.detail);
      if (!input.session) return value;
      // Settle a turn that already finished, so the reply shows where the name now points.
      const session = await reconcileSession(readyStore(), who, input.session);
      const unsupported = (result.error === "continuation_unsupported" ? `\n  fix: ${String(result.fix)}` : "") +
        (result.session_error ? `\n  ! ${String(result.session_error)}` : "");
      const line = `  session ${input.session} ${result.session_turn}${session?.turnStatus === null ? " active" : ""}`;
      return { ...value, session: input.session, session_turn: result.session_turn,
        ...(result.error === "continuation_unsupported" ? { error: result.error, fix: result.fix } : {}),
        ...(result.session_error ? { session_error: result.session_error } : {}),
        session_digest: `${unsupported}\n${line}`,
        ...(typeof value.digest === "string" ? { digest: `${value.digest}${unsupported}\n${line}` } : {}) };
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
  "Read or wait for runs. Batch status shows each finished lane's tail once per session; request one lane or pass tail_chars to see it again. wait_seconds above 55 is clamped to 55; invalid numeric values return a typed rejection.",
  toolSchemas.fabric_status,
  async ({ ids, id, wait_seconds, until, detail, tail_chars, fields }, { signal }) => {
    const waitResult = boundedWait(wait_seconds);
    if (waitResult.error) return waitResult.error;
    const result = await statusRows(who.project, ids ?? (id ? [id] : undefined), waitResult.value, until, signal, detail, true, 20, false, true);
    acknowledgeRuns(result);
    const view = detail === "full"
      ? fullView(runView(withWarnings(result, waitResult.warnings), detail), fields)
      : runView(withWarnings(result, waitResult.warnings), detail);
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
    // A short answer should not cost a second call: a terminal row carries the end of its result
    // the first time this session sees it, so batch polls do not resend finished lanes.
    const rows: Record<string, any>[] = Array.isArray(result.runs) ? result.runs : [];
    const localHost = (result as Record<string, any>).hosts?.find((host: Record<string, any>) => host.local)?.host;
    const terminal = rows.filter((row) => row.state === "terminal" && (!row.host || row.host === localHost));
    const oneLaneRequested = rows.length === 1 && (
      (id !== undefined && id !== rows[0]?.batch_id) ||
      (ids?.length === 1 && ids[0] !== rows[0]?.batch_id)
    );
    const laneKey = (row: Record<string, any>) => `${who.project}:${row.run_id}:${row.task_id}:${row.attempt}`;
    const tailed = tail_chars === 0 || (detail === "full" && tail_chars === undefined) ? [] : terminal.filter((row) =>
      oneLaneRequested || tail_chars !== undefined || !tailedLanes.has(laneKey(row)));
    for (const row of tailed) {
      tailedLanes.delete(laneKey(row));
      tailedLanes.add(laneKey(row));
    }
    while (tailedLanes.size > 4096) tailedLanes.delete(tailedLanes.values().next().value as string);
    const each = Math.min(tail_chars ?? 1200, Math.floor(Math.max(3600, tail_chars ?? 0) / Math.max(1, tailed.length)));
    const tails = tailed.flatMap((row) => {
      const tail = resultTail(row, each);
      return tail ? [`result tail ${row.task_id ?? row.run_id}${tail.total > tail.text.length ? " (end)" : ""}:\n${tail.text}`] : [];
    });
    const running = rows.some((row) => row.state === "running" || row.state === "queued");
    const pollHint = wait_seconds === undefined && rows.length > 0 && rows.every((row) => row.state === "running")
      ? 'hint: pass wait_seconds: 55 to block (or use `provenant lanes --wait --all --timeout 900`)'
      : 'hint: shell `provenant lanes --wait --all --timeout 900` blocks until they finish';
    const ownership = [
      ...tails,
      ...(running ? [pollHint] : []),
      ...workClaims.map((claim) => `claim ${claim.issue ?? claim.paths.join(",")} ${claim.holder} g${claim.generation}`),
      ...(landingLease ? [`landing ${landingLease.holder} g${landingLease.generation} ${landingLease.expectedSha}`] : []),
    ];
    return { ...view,
      ...(detail === "full" || ownership.length ? { work_claims: workClaims, landing_lease: landingLease } : {}),
      ...(ownership.length ? { digest: `${digest(view)}\n${ownership.join("\n")}` } : {}) };
  },
);
register("fabric_work_claim", "Acquire, renew, verify or release an advisory issue or path claim.",
  toolSchemas.fabric_work_claim, ({ action, session_id, issue, paths, id, generation, seconds }) => {
  const ttl = Math.min(seconds ?? 900, 3600);
  const warnings = seconds > 3600 ? [`seconds ${seconds} clamped to 3600`] : [];
  if (action === "acquire") return withWarnings(readyStore().acquireWork(who, session_id, { issue, paths }, ttl), warnings);
  if (!id || generation === undefined) throw new Error("id and generation are required");
  if (action === "renew") return withWarnings(readyStore().renewWork(who, session_id, id, generation, ttl), warnings);
  if (action === "verify") return readyStore().verifyWork(who, session_id, id, generation);
  readyStore().releaseWork(who, session_id, id, generation);
  return { released: id, generation };
});
register("fabric_landing_lease",
  "Acquire, renew, verify or release the repository landing lease. action=wait blocks up to wait_seconds (max 55) " +
  "until the lease changes hands or frees; use it instead of sleep loops. expected_sha may be short or uppercase.",
  toolSchemas.fabric_landing_lease, async ({ action, session_id, expected_sha, generation, seconds, wait_seconds }, { signal }) => {
  if (action === "wait") {
    const bounded = boundedWait(wait_seconds ?? MAX_WAIT_SECONDS);
    if (bounded.error) return bounded.error;
    const waited = await readyStore().waitLanding(who.project, bounded.value!, signal);
    return withWarnings({ changed: waited.changed, landing_lease: waited.lease }, bounded.warnings);
  }
  if (action === "acquire") {
    if (!expected_sha) throw new Error("expected_sha is required: the integration branch's current remote SHA");
    return readyStore().acquireLanding(who, session_id, resolveCommitSha(who.cwd, expected_sha), seconds ?? 300);
  }
  if (generation === undefined)
    throw new Error("generation is required: pass the generation returned by action=acquire (fabric_status shows the live one)");
  if (action === "renew") return readyStore().renewLanding(who, session_id, generation, seconds ?? 300);
  if (action === "verify") {
    if (!expected_sha) throw new Error("expected_sha is required");
    return readyStore().verifyLanding(who, session_id, generation, resolveCommitSha(who.cwd, expected_sha));
  }
  readyStore().releaseLanding(who, session_id, generation);
  return { released: generation };
});
register(
  "fabric_session",
  "Inspect, list or forget named sessions; fabric_dispatch with session starts or resumes one.",
  toolSchemas.fabric_session,
  async ({ action, name }) => {
    const store = readyStore();
    if (action === "list") {
      const rows = [];
      for (const row of store.sessions(who.project)) {
        const current = await reconcileSession(store, who, row.name);
        if (current) rows.push(sessionView(current));
      }
      return { sessions: rows, digest: rows.map((row) => row.digest).join("\n") || "no sessions" };
    }
    let key: string;
    try { key = sessionName(name); } catch (error) { return rejected(error); }
    const row = await reconcileSession(store, who, key);
    if (!row) return { status: "rejected", error: "session_unknown", fix: `Start ${key} with fabric_dispatch{session, prompt}.` };
    if (action === "inspect") return sessionView(row);
    if (row.turnStatus === null || !store.forgetSession(who, key))
      return { status: "rejected", error: "session_busy", session: key, active_run_id: row.turnRunId,
        fix: `Wait for run ${row.turnRunId ?? "launch"} to finish, or cancel it, then forget.` };
    return { status: "ok", forgotten: key, digest: `forgot session ${key}; run ${row.runId ?? row.turnRunId} and provider history kept` };
  },
);
register("fabric_cancel", "Stop a run and its provider group, or one task of a batch by its task id.",
  toolSchemas.fabric_cancel, async ({ id, reason, operation_id, detail }) => {
  const result = await cancelConfiguredRun(id, who, reason, process.env, operation_id);
  acknowledgeRuns(result);
  const view = runView(result);
  return detail === "full" ? view : { ...view, digest: cancelDigest(view) };
});
register("fabric_graph",
  "Run a plan graph of dispatch nodes under a detached owner, or read, cancel or resume one. run takes file or document; status without id lists graphs and restarts a dead owner; wait_seconds up to 55.",
  toolSchemas.fabric_graph,
  async ({ action, file, document, id, node, force, wait_seconds, detail }, { signal }) => {
    const misuse = (fix: string) => ({ status: "rejected", error: "graph_input_invalid", fix });
    if (action === "run" && (file === undefined) === (document === undefined)) return misuse("Pass exactly one of file or document for run.");
    if (action !== "run" && (file !== undefined || document !== undefined)) return misuse(`Remove file and document: ${action} reads an existing graph.`);
    if ((action === "cancel" || action === "resume") && id === undefined) return misuse(`Pass the graph id to ${action}.`);
    if (force !== undefined && action !== "resume") return misuse("force applies only to resume.");
    if (node !== undefined && action !== "cancel") return misuse("node applies only to cancel.");
    const waitResult = boundedWait(wait_seconds);
    if (waitResult.error) return waitResult.error;
    if (waitResult.value !== undefined && action !== "status") return misuse("wait_seconds applies only to status.");
    const { graphCancel, graphResume, graphRun, graphStatus } = await import("./graph/surface.js");
    const chair = executionIdentity();
    const result = action === "run" ? graphRun(file !== undefined ? { file } : { document }, chair)
      : action === "status" ? await graphStatus(id, chair, process.env, { waitSeconds: waitResult.value ?? 0, resume: true, signal })
      : action === "cancel" ? graphCancel(id!, node, chair)
      : graphResume(id!, chair, process.env, force);
    const view = withWarnings(result, waitResult.warnings);
    return detail === "full" ? view : { status: view.status, ...(view.error ? { error: view.error, fix: view.fix } : {}),
      ...(typeof view.digest === "string" ? { digest: view.digest } : {}), ...(view.warnings ? { warnings: view.warnings } : {}) };
  },
);
register("fabric_fetch", "Fetch one terminal remote writer: verify on the owning host, import its commits and compare the bound head.",
  toolSchemas.fabric_fetch, async (input, { signal }) =>
    await federatedLane("fetch", who.cwd, input, signal) ??
      { status: "rejected", error: "remote_host_required", fix: "Qualify a remote writer ID with its host." },
);
register(
  "fabric_output",
  "Read output; continue at next_offset. max_bytes above 20000 is clamped with a warning; invalid numeric values return a typed rejection.",
  toolSchemas.fabric_output,
  async (input) => {
    if (input.max_bytes !== undefined &&
      (typeof input.max_bytes !== "number" || !Number.isInteger(input.max_bytes) || input.max_bytes < 1))
      return { status: "rejected", error: "max_bytes_invalid", fix: "Pass max_bytes as an integer from 1 to 20000." };
    const requested = input.max_bytes;
    const result = await fabricOutput(who.project, { ...input, max_bytes: requested === undefined ? undefined : Math.min(requested, 20000) });
    return requested !== undefined && requested > 20000
      ? withWarnings(result, [`! max_bytes ${requested} clamped to 20000`])
      : result;
  },
);
register(
  "fabric_acknowledge",
  "Acknowledge a claimed delivery.",
  toolSchemas.fabric_acknowledge,
  ({ message_id, claim_id }) => readyStore().acknowledge(who, message_id, claim_id),
);
register(
  "fabric_task",
  "Create, claim, update or list tasks.",
  toolSchemas.fabric_task,
  ({ action, task_id, objective, owner, depends_on, state, note }) => {
    if (action === "list") return { tasks: readyStore().tasks(who.project, state) };
    if (action === "create" && objective)
      return readyStore().createTask(who, objective, { taskId: task_id, owner, dependsOn: depends_on });
    if (action === "claim" && task_id) return readyStore().claimTask(who, task_id);
    if (action === "update" && task_id && state) return readyStore().updateTask(who, task_id, state, note);
    throw new Error("Supply objective for create; task_id for claim; task_id and state for update.");
  },
);
register("fabric_note", "Append an activity note.",
  toolSchemas.fabric_note, ({ detail }) => {
  readyStore().note(who, detail);
  return { noted: detail };
});
register(
  "fabric_activity",
  "Read recent activity or entries after a cursor.",
  toolSchemas.fabric_activity,
  ({ limit, after_seq }) => ({
    activity:
      after_seq === undefined
        ? readyStore().activity(who.project, limit ?? 20)
        : readyStore().activityAfter(who.project, after_seq, limit ?? 20),
  }),
);
register("fabric_hosts", "List hosts or diagnose peers over SSH.",
  toolSchemas.fabric_hosts, ({ action, hosts }, { signal }) => fabricHosts(action, hosts ?? [], who.cwd, signal));
register("fabric_adapters", "List routes and guarantees; full includes profiles; models lists one adapter's live models; action reset clears cooldowns.",
  toolSchemas.fabric_adapters, ({ detail, models, match, action, selector }) => {
  if (action === "reset") {
    const removed = resetCooldowns(selector);
    return { digest: removed.length ? `cleared: ${removed.join(" ")}` : "no cooldowns matched" };
  }
  return models === undefined ? adapterView(catalogueSnapshot(), detail) : liveModels(models, { match });
});
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
await reapAttemptRuns(who.cwd);
await server.connect(transport);
