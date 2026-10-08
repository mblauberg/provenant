/** Continue a provider session, or hand its result to a fresh session. */
import { randomUUID } from "node:crypto";
import { closeSync, constants, fstatSync, mkdirSync, openSync, readFileSync, readSync, realpathSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";

import { preflight, validatePrompt, rejected, timeoutSeconds, InputError, ownerPromptPath, savedReadRoots, type DispatchInput, type RouteInput } from "./execution-input.js";
import { usesPool } from "./pools.js";
import {
  dispatchConfiguredProvider,
  executableOwner,
  observeOwner,
  productRoot,
  pythonOwner,
  stagingPath,
  startOwner,
  type LaunchObserver,
} from "./execution.js";
import type { Identity } from "./identity.js";
import { processMatches, readOwnerRecord, statusRows } from "./run-registry.js";

/** The whole injected handoff text, prefix and result tail together. */
import { federatedLane } from "./hosts.js";
export const HANDOFF_BYTES = 8000;

/** The effort a run sent; one the provider only reported was never sent, so it is not re-sent. */
function sentEffort(previous: Record<string, any>): string | undefined {
  return previous.provenance?.effort_observed_source ? undefined : previous.provenance?.effort_applied || undefined;
}

/**
 * One earlier attempt of a task row, standing in for the row: a named session
 * continues from its last clean attempt, not from whatever attempt came last.
 */
function pinnedAttempt(row: Record<string, any>, attempt: number | undefined): Record<string, any> {
  if (attempt === undefined) return row;
  const pinned = (row.attempts ?? []).find((item: Record<string, any>) => Number(item.attempt) === attempt);
  if (!pinned) throw new InputError("resume_attempt_unknown", `Attempt ${attempt} of run ${row.run_id} is no longer recorded.`);
  const result = pinned.paths?.result;
  return { ...row, ...pinned, run_dir: row.run_dir,
    result_path: typeof result === "string" ? resolve(String(row.run_dir), result) : null };
}

/** Continue from one saved attempt and exactly its provider session, never a relaunch. */
export interface PinnedContinuation extends LaunchObserver {
  attempt?: number;
  session?: string;
}

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
  pin: PinnedContinuation = {},
): Promise<Record<string, unknown>> {
  const remote = env.PROVENANT_HOST_LOCAL_ONLY === "1" ? undefined : await federatedLane("resume", identity.cwd, input as Record<string, unknown>, signal);
  if (remote) return remote;
  env = { ...env, PROVENANT_HOST_LOCAL_ONLY: "1" };
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
    const allowed = ["resume", "task_id", "context_ceiling", "timeout_seconds", "prompt", "prompt_file", "wait_seconds", "detail", "allow_secrets"];
    const changed = Object.keys(input).filter((key) => !allowed.includes(key));
    if (changed.length)
      throw new InputError("resume_route_change", `Resume keeps the route, mode and controls; drop ${changed.join(", ")} or dispatch a new run.`);
    const target = await targetTask(identity.cwd, input.resume!, input.task_id, "resume");
    if (target.rejected) return target.rejected;
    const latest = target.row!;
    if (latest.state !== "terminal")
      throw new InputError("resume_not_ready", "Resume one terminal task; wait for its active attempt to finish.");
    if(latest.attempts?.at(-1)?.state === "running") throw new InputError("resume_not_ready", "Dispatch a new run; the owner did not terminalise this attempt.");
    const previous = pinnedAttempt(latest, pin.attempt);
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
        ? ownerPromptPath(identity, input.prompt_file!)
        : stagingPath(runDir, `resume-${randomUUID()}.md`);
    const roots = savedReadRoots(identity, previous.read_roots, previous.mode === "read_only" ? previous.cwd : undefined);
    const requested = previous.provenance?.requested ?? {};
    const checked = await preflight(python, owner, [{
      id: taskId, adapter: requested.adapter ?? previous.adapter ?? identity.provider,
      model: previous.provenance?.resolved_model ?? requested.model,
      effort: sentEffort(previous),
      access_mode: previous.mode ?? "read_only", worktree: previous.worktree ?? undefined,
      cwd: previous.mode === "worktree_write" ? undefined : executionIdentity.cwd,
      ...Object.fromEntries(Object.entries(previous.applied ?? {}).filter(([key, value]) =>
        ["sandbox", "network", "add_dirs", "capabilities"].includes(key) && value !== null)),
      ...(input.prompt === undefined ? { prompt_file: path } : { prompt: input.prompt }),
      ...(roots.length ? { read_roots: roots } : {}),
      allow_secrets: input.allow_secrets ?? false,
      ...(requested.confidential === true ? { confidential: true } : {}),
    }], identity, env, signal);
    if (checked.status === "rejected") return { status: "rejected", error: checked.error, fix: checked.fix };
    if (input.prompt !== undefined) writeFileSync(path, input.prompt, { mode: 0o600, flag: "wx" });
    const next = Math.max(0,...(latest.attempts ?? []).map((row:Record<string,any>)=>Number(row.attempt) || 0)) + 1;
    // A batch keeps its manifest ids and routes, so its other tasks stay listed.
    const taskIds: string[] = batch ? batch.task_ids : [taskId];
    const routes = batch
      ? taskIds.map((id, index) => (id === taskId ? (checked.routes as unknown[])?.[0] : batch.routes?.[index]))
      : checked.routes;
    try {
      pin.onLaunch?.({ runId: String(previous.run_id), taskId, attempt: next });
    } catch (error) {
      if (input.prompt !== undefined) rmSync(path, { force: true });
      throw error;
    }
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
        ...(input.allow_secrets === true ? ["--allow-secrets"] : []),
        ...(previous.mode === "worktree_write" ? [] : ["--cwd", executionIdentity.cwd]),
        ...(pin.attempt === undefined ? [] : ["--resume-attempt", String(pin.attempt)]),
        ...(pin.session === undefined ? [] : ["--require-session", pin.session]),
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
    const status = await statusRows(identity.cwd, [String(previous.run_id)]);
    return status.runs?.find((row: Record<string, any>) => row.task_id === taskId) ?? status;
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
/** A handoff keeps the previous route unless the caller names any selector, pool selectors included. */
export function inheritsPreviousRoute(input: RouteInput): boolean {
  return input.adapter === undefined && input.alias === undefined && input.model === undefined && !usesPool(input);
}

/** A handoff copies the previous result into its prompt, so a confidential run stays confidential unless the caller sets it. */
export function inheritedConfidential(input: RouteInput, requested: { confidential?: unknown }): { confidential?: true } {
  return input.confidential === undefined && requested.confidential === true ? { confidential: true } : {};
}

export async function handoffDispatch(
  input: DispatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
  pin: PinnedContinuation = {},
): Promise<Record<string, unknown>> {
  const remote = env.PROVENANT_HOST_LOCAL_ONLY === "1" ? undefined : await federatedLane("handoff", identity.cwd, input as Record<string, unknown>, signal);
  if (remote) return remote;
  env = { ...env, PROVENANT_HOST_LOCAL_ONLY: "1" };
  try {
    validatePrompt(input.prompt, input.prompt_file);
    const { handoff, task_id, ...rest } = input;
    const target = await targetTask(identity.cwd, handoff!, task_id, "handoff");
    if (target.rejected) return target.rejected;
    if (target.row!.state !== "terminal")
      throw new InputError("handoff_not_ready", "Wait for the prior task to finish, then hand off.");
    const previous = pinnedAttempt(target.row!, pin.attempt);
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
    const inherit = inheritsPreviousRoute(rest);
    const writer = rest.mode === undefined && rest.worktree === undefined && rest.cwd === undefined &&
      previous.mode === "worktree_write" && typeof previous.worktree === "string";
    return await dispatchConfiguredProvider({
      ...rest,
      ...(inherit
        ? {
            adapter: requested.adapter ?? previous.adapter ?? identity.provider,
            model: previous.provenance?.resolved_model || requested.model || undefined,
            ...(rest.effort === undefined && sentEffort(previous) ? { effort: sentEffort(previous) } : {}),
          }
        : {}),
      ...(writer ? { mode: "worktree_write" as const, worktree: previous.worktree } : {}),
      ...inheritedConfidential(rest, requested),
      prompt: brief + prompt,
      prompt_file: undefined,
    }, identity, signal, env, pin);
  } catch (error) {
    if (signal.aborted) throw error;
    return rejected(error);
  }
}
