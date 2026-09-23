/** Continue a provider session, or hand its result to a fresh session. */
import { randomUUID } from "node:crypto";
import { closeSync, constants, fstatSync, mkdirSync, openSync, readFileSync, readSync, realpathSync, unlinkSync, writeFileSync } from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";

import { preflight, validatePrompt, rejected, timeoutSeconds, InputError, type DispatchInput } from "./execution-input.js";
import {
  dispatchConfiguredProvider,
  executableOwner,
  observeOwner,
  productRoot,
  pythonOwner,
  stagingPath,
  startOwner,
} from "./execution.js";
import type { Identity } from "./identity.js";
import { fabricStatus, processMatches, readOwnerRecord, statusRows } from "./run-registry.js";

/** The whole injected handoff text, prefix and result tail together. */
export const HANDOFF_BYTES = 8000;

/** One task row: a run id with task_id for a batch, or a task's own id. */
async function targetTask(cwd: string, id: string, taskId: string | undefined, verb: string) {
  const result = await statusRows(cwd, [id]);
  if (!result.runs) return { rejected: result };
  const rows = taskId === undefined ? result.runs : result.runs.filter((row) => row.task_id === taskId);
  if (rows.length === 0) throw new InputError(`${verb}_task_unknown`, `Pass a task_id from run ${id}.`);
  if (rows.length > 1) throw new InputError(`${verb}_task_required`, `Pass task_id to ${verb} one task of batch ${id}.`);
  return { row: rows[0]! };
}

export async function resumeConfiguredProvider(
  input: DispatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
): Promise<Record<string, unknown>> {
  let lock: string | undefined,
    launched = false;
  try {
    validatePrompt(input.prompt, input.prompt_file);
    if (
      !Number.isInteger(input.wait_seconds ?? 55) ||
      (input.wait_seconds ?? 55) < 0 ||
      (input.wait_seconds ?? 55) > 55
    )
      throw new InputError("wait_invalid", "Pass wait_seconds from 0 to 55.");
    const allowed = ["resume", "task_id", "context_ceiling", "timeout_seconds", "prompt", "prompt_file", "wait_seconds", "detail"];
    const changed = Object.keys(input).filter((key) => !allowed.includes(key));
    if (changed.length)
      throw new InputError("resume_route_change", `Resume keeps the route, mode and controls; drop ${changed.join(", ")} or dispatch a new run.`);
    const target = await targetTask(identity.cwd, input.resume!, input.task_id, "resume");
    if (target.rejected) return target.rejected;
    const previous = target.row!;
    if (previous.state !== "terminal")
      throw new InputError("resume_not_ready", "Resume one terminal task; wait for its active attempt to finish.");
    if(previous.attempts?.at(-1)?.state === "running") throw new InputError("resume_not_ready", "Dispatch a new run; the owner did not terminalise this attempt.");
    const root = productRoot(env),
      runDir = String(previous.run_dir),
      taskId = String(previous.task_id);
    const ownerRecord = readOwnerRecord(runDir);
    const alive = (pid: number) => {
      try {
        process.kill(pid, 0);
        return true;
      } catch {
        return false;
      }
    };
    if (ownerRecord && processMatches(ownerRecord.owner_pid, ownerRecord.owner_started_at))
      throw new InputError("resume_not_ready", "Wait for the current owner to exit.");
    mkdirSync(join(runDir, "_owner"), { recursive: true, mode: 0o700 });
    const lockPath = join(runDir, "_owner/resume.lock");
    try {
      const old = JSON.parse(readFileSync(lockPath, "utf8"));
      if (Number.isInteger(old.pid) && !alive(old.pid)) unlinkSync(lockPath);
    } catch {
      /* A competing live claim is rejected by exclusive creation. */
    }
    try {
      writeFileSync(lockPath, JSON.stringify({ pid: process.pid }), { flag: "wx", mode: 0o600 });
      lock = lockPath;
    } catch {
      throw new InputError("resume_not_ready", "Wait for the existing resume owner to finish.");
    }
    const saved = JSON.parse(readFileSync(join(runDir, "dispatch-status.json"), "utf8")) as Record<string, any>;
    const batch = saved.batch_id !== undefined && Array.isArray(saved.task_ids) ? saved : undefined;
    const timeout = input.timeout_seconds === undefined
      ? Number(saved.timeout_seconds ?? (previous.mode === "worktree_write" ? 10800 : 3600))
      : timeoutSeconds(input.timeout_seconds);
    const executionIdentity = { ...identity, cwd: typeof previous.cwd === "string" ? previous.cwd : identity.cwd };
    const python = await pythonOwner(root, identity, env);
    const owner = executableOwner(root, "skills/orchestrate/scripts/dispatch_run.py");
    const controls = executableOwner(root, "skills/orchestrate/scripts/run_controls.py");
    const path =
      input.prompt === undefined
        ? resolve(identity.cwd, input.prompt_file!)
        : stagingPath(runDir, `resume-${randomUUID()}.md`);
    const requested = previous.provenance?.requested ?? {};
    const checked = await preflight(python, owner, [{
      id: taskId, adapter: requested.adapter ?? previous.adapter ?? identity.provider,
      model: previous.provenance?.resolved_model ?? requested.model,
      effort: previous.provenance?.effort_applied || undefined,
      access_mode: previous.mode ?? "read_only", worktree: previous.worktree ?? undefined,
      cwd: previous.mode === "worktree_write" ? undefined : executionIdentity.cwd,
      ...Object.fromEntries(Object.entries(previous.applied ?? {}).filter(([key, value]) =>
        ["sandbox", "network", "add_dirs"].includes(key) && value !== null)),
      ...(input.prompt === undefined ? { prompt_file: path } : { prompt: input.prompt }),
    }], identity, env, signal);
    if (checked.status === "rejected") return { status: "rejected", error: checked.error, fix: checked.fix };
    if (input.prompt !== undefined) writeFileSync(path, input.prompt, { mode: 0o600, flag: "wx" });
    const next = Math.max(0,...(previous.attempts ?? []).map((row:Record<string,any>)=>Number(row.attempt) || 0)) + 1;
    // A batch keeps its manifest ids and routes, so its other tasks stay listed.
    const taskIds: string[] = batch ? batch.task_ids : [taskId];
    const routes = batch
      ? taskIds.map((id, index) => (id === taskId ? (checked.routes as unknown[])?.[0] : batch.routes?.[index]))
      : checked.routes;
    const started = startOwner(
      python,
      [
        owner,
        "--run-dir",
        runDir,
        "--resume",
        String(previous.run_id),
        "--task-id",
        taskId,
        "--prompt-file",
        path,
        "--timeout",
        String(timeout),
        ...(input.context_ceiling === undefined ? [] : ["--context-ceiling", String(input.context_ceiling)]),
        ...(previous.mode === "worktree_write" ? [] : ["--cwd", executionIdentity.cwd]),
      ],
      identity,
      env,
      runDir,
      {
        command: python,
        args: [
          controls,
          "cancel",
          "--run-dir",
          runDir,
          "--task-id",
          taskId,
          "--attempt-id",
          `attempt-${String(next).padStart(3, "0")}`,
          "--wait-seconds",
          "5",
        ],
        targetDirectory: runDir,
        cwd: identity.cwd,
        env,
      },
      {
        workspace: identity.cwd,
        kind: "dispatch",
        identifier: taskId,
        taskIds,
        resume: true,
        ...(batch?.batch_id ? { batchId: String(batch.batch_id) } : {}),
        routes,
        nextAttempt: next,
        timeout,
      },
      [lockPath, ...(input.prompt === undefined ? [] : [path])],
    );
    launched = true;
    await observeOwner(started, input.wait_seconds ?? 55, signal);
    const status = await fabricStatus(identity.cwd, String(previous.run_id));
    return Array.isArray(status.runs) ? status.runs.find((row: Record<string, any>) => row.task_id === taskId) ?? status : status;
  } catch (error) {
    if (signal.aborted) throw error;
    return rejected(error);
  } finally {
    if (lock && !launched) {
      try {
        unlinkSync(lock);
      } catch {
        /* Already released. */
      }
    }
  }
}

function resultTail(row: Record<string, any>, budget: number): string {
  const raw = row.paths?.result ?? row.result_path;
  if (typeof raw !== "string" || budget <= 0) return "";
  let fd: number | undefined;
  try {
    const root = realpathSync(String(row.run_dir)),
      path = realpathSync(resolve(root, raw));
    const inside = relative(root, path);
    if (isAbsolute(inside) || inside === ".." || inside.startsWith(`..${sep}`)) return "";
    fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    const size = fstatSync(fd).size,
      bytes = Buffer.alloc(Math.min(size, budget));
    readSync(fd, bytes, 0, bytes.length, size - bytes.length);
    let start = 0;
    while (start < bytes.length && (bytes[start]! & 0xc0) === 0x80) start++;
    return bytes.subarray(start).toString("utf8");
  } catch {
    return "";
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

export function handoffBrief(previous: Record<string, any>): string {
  const route = String(previous.provenance?.line ?? "Route: unknown").replace(/^Route: /u, "").slice(0, 300);
  const head =
    `Fresh session handed off from Fabric run ${previous.run_id} task ${previous.task_id} (${route}). ` +
    "Its session was not resumed. ";
  const open = "Tail of its result:\n<<<\n",
    close = "\n>>>\n\n";
  const tail = resultTail(previous, HANDOFF_BYTES - Buffer.byteLength(head + open + close));
  return head + (tail ? open + tail + close : "It left no result.\n\n");
}

/** A fresh session primed with the prior result tail: the cheap alternative to a large resume. */
export async function handoffDispatch(
  input: DispatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
): Promise<Record<string, unknown>> {
  try {
    validatePrompt(input.prompt, input.prompt_file);
    const { handoff, task_id, ...rest } = input;
    const target = await targetTask(identity.cwd, handoff!, task_id, "handoff");
    if (target.rejected) return target.rejected;
    const previous = target.row!;
    if (previous.state !== "terminal")
      throw new InputError("handoff_not_ready", "Wait for the prior task to finish, then hand off.");
    let prompt = input.prompt;
    if (prompt === undefined) {
      try {
        prompt = readFileSync(resolve(identity.cwd, input.prompt_file!), "utf8");
      } catch {
        throw new InputError("prompt_unavailable", "Pass an existing prompt_file.");
      }
    }
    const brief = handoffBrief(previous);
    const requested = previous.provenance?.requested ?? {};
    const inherit = rest.adapter === undefined && rest.alias === undefined && rest.model === undefined;
    const writer = rest.mode === undefined && rest.worktree === undefined && rest.cwd === undefined &&
      previous.mode === "worktree_write" && typeof previous.worktree === "string";
    return await dispatchConfiguredProvider({
      ...rest,
      ...(inherit
        ? {
            adapter: requested.adapter,
            model: previous.provenance?.resolved_model || requested.model || undefined,
            ...(rest.effort === undefined && previous.provenance?.effort_applied
              ? { effort: previous.provenance.effort_applied }
              : {}),
          }
        : {}),
      ...(writer ? { mode: "worktree_write" as const, worktree: previous.worktree } : {}),
      prompt: brief + prompt,
      prompt_file: undefined,
    }, identity, signal, env);
  } catch (error) {
    if (signal.aborted) throw error;
    return rejected(error);
  }
}
