import { execFile } from "node:child_process";
import { existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { promisify } from "node:util";

import {
  normaliseRoute, namesExplicitRoute, preflight, applyDispatchDefaults, dispatchDefaults,
  validatePrompt, timeoutSeconds, ownerPromptPath, readRoots, workingIdentity, rejected,
  InputError, type BatchInput, type BatchTaskInput, type DispatchDefaults,
} from "./execution-input.js";
import { catalogueSnapshot, type CatalogueSnapshot } from "./catalogue.js";
import { compactBatch } from "./owner-output.js";
import { expandPools, nativeFirst, usesPool, withoutEmptySelectors, POOL_FIELDS } from "./pools.js";
import { runRoot, projectRoot, type Identity } from "./identity.js";
import { pythonOwner, pythonOwnerEnvironment } from "./python-owner.js";
import { assertFixtureRunRoot, executableOwner, productRoot } from "./run-root.js";
import { shortRunId, findRecordedRun, terminateRecordedRun } from "./run-registry.js";
import { statusRows } from "./run-status.js";
import { federatedLane } from "./hosts.js";
import {
  activeOwners, initialiseRun, observeOwner, rejectNestedSandboxDispatch, rejectOwnSeatClaude,
  requestOwnerCancellation, running, stagingPath, startOwner, withWarnings, type LaunchObserver,
} from "./execution.js";

const execFileAsync = promisify(execFile);
const FIRST_BATCH_ID = "batch-001";

/** Marks a batch task whose caller named its route (a task, a batch default or `models`), before pool expansion. */
const OWN_SEAT_GUARD = "_own_seat_guard";

function normaliseTask(
  task: BatchTaskInput,
  index: number,
  identity: Identity,
  catalogue: CatalogueSnapshot,
  defaults: DispatchDefaults = {},
): Record<string, unknown> {
  const { [OWN_SEAT_GUARD]: ownSeatGuard, ...plain } = task as BatchTaskInput & { [OWN_SEAT_GUARD]?: boolean };
  task = plain;
  const initialRoute = normaliseRoute(task, identity, catalogue, ownSeatGuard === true);
  task = { ...task, mode: initialRoute.access_mode };
  validatePrompt(task.prompt, task.prompt_file);
  const providerIdentity = workingIdentity(task, identity);
  task = { ...task, ...(task.cwd === undefined ? {} : { cwd: providerIdentity.cwd }) };
  const route = applyDispatchDefaults(normaliseRoute(task, identity, catalogue, ownSeatGuard === true), defaults);
  route.warnings = [...new Set([...(initialRoute.warnings ?? []), ...(route.warnings ?? [])])];
  const roots = readRoots(identity, task.cwd, task.prompt_file);
  if (roots.length) route.read_roots = roots;
  return {
    id: task.id ?? `task-${index + 1}`,
    ...(task.prompt === undefined ? { prompt_file: ownerPromptPath(identity, task.prompt_file!) } : { prompt: task.prompt }),
    timeout: timeoutSeconds(task.timeout_seconds ?? defaults.timeout_seconds, task.mode),
    ...route,
  };
}

export function batchDefaults(input: BatchInput): Record<string, any> {
  return withoutEmptySelectors(Object.fromEntries(Object.entries(input).filter(([key]) =>
    ["adapter", "alias", "model", "effort", "account", "mode", "worktree", "cwd", "network", "sandbox", "capabilities", "add_dirs", "fallback", "timeout_seconds", "context_ceiling", "allow_secrets", "confidential", "delivery_run_id", "knobs", ...POOL_FIELDS].includes(key))));
}

export function mergeBatchTask(defaults: Record<string, any>, input: BatchTaskInput, index: number): BatchTaskInput {
  const task = withoutEmptySelectors(input);
  // A task's selector replaces a default pool; its pool replaces a default selector.
  const own = usesPool(task) ? Object.fromEntries(Object.entries(defaults).filter(([key]) => !["alias", "model"].includes(key)))
    : task.alias !== undefined || task.model !== undefined
      ? Object.fromEntries(Object.entries(defaults).filter(([key]) => !(POOL_FIELDS as readonly string[]).includes(key)))
      : defaults;
  return { ...own, ...task, id: task.id ?? `task-${index + 1}` };
}

export async function dispatchConfiguredBatchUnchecked(
  input: BatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
  launch: LaunchObserver = {},
): Promise<Record<string, unknown>> {
  if (input.tasks.length < 1 || input.tasks.length > 64)
    throw new InputError("invalid_input", "tasks must contain 1-64 items");
  if (input.concurrency !== undefined && (!Number.isInteger(input.concurrency) || input.concurrency < 1 || input.concurrency > 8)) {
    throw new InputError("invalid_input", "concurrency must be an integer from 1 to 8");
  }
  if (!Number.isInteger(input.wait_seconds ?? 0) || (input.wait_seconds ?? 0) < 0 || (input.wait_seconds ?? 0) > 55) {
    throw new InputError("wait_invalid", "Pass wait_seconds from 0 to 55.");
  }
  const callStarted = Date.now();
  const root = productRoot(env);
  const catalogue = catalogueSnapshot(root, env);
  const policy = dispatchDefaults(identity.cwd);
  const errors: Record<string, unknown>[] = [];
  const defaults = batchDefaults(input);
  let merged: BatchTaskInput[] = input.tasks.map((task) => withoutEmptySelectors(task)).flatMap((task, index) => {
    const taskError = (task as BatchTaskInput & { _fabric_error?: Record<string, unknown> })._fabric_error;
    if (taskError) {
      errors.push({ task_id: task.id ?? `task-${index + 1}`, ...taskError });
      return [];
    }
    return [mergeBatchTask(defaults, task, index)];
  });
  const poolWarnings: string[] = [];
  merged = merged.map((task) => {
    // Never trust the field from the caller: it is recomputed here and only ever set by this function.
    const marked = { ...task, [OWN_SEAT_GUARD]: !launch.continuation &&
      (namesExplicitRoute(task) || task.models !== undefined || launch.guardedCouncil === true) };
    const preferred = nativeFirst(marked, identity);
    poolWarnings.push(...preferred.warnings);
    return preferred.task;
  });
  let councils = false;
  if (merged.some(usesPool)) {
    const expanded = await expandPools(merged, await pythonOwner(root, env), root, identity, env, signal);
    errors.push(...expanded.errors);
    poolWarnings.push(...expanded.warnings);
    merged = expanded.tasks;
    councils = expanded.council;
    if (merged.length > 64) throw new InputError("invalid_input", "tasks must contain 1-64 items after councils expand");
  }
  // Size the default after councils expand, so a nested council does not run serially; an explicit limit stands.
  const concurrency = input.concurrency ?? Math.max(1, Math.min(councils ? 8 : 4, merged.length));
  const tasks = merged.flatMap((task, index) => {
    try {
      return [normaliseTask(task, index, identity, catalogue, policy.defaults)];
    } catch (error) {
      errors.push({ task_id: task.id ?? `task-${index + 1}`, ...rejected(error) });
      return [];
    }
  });
  if (tasks.length === 0) return { status: "rejected", error: errors[0]!.error, fix: errors[0]!.fix, tasks: errors.map((row) => ({ ...row, status: "rejected", state: "terminal" })) };
  assertFixtureRunRoot(runRoot(identity.cwd), tasks);
  const owner = executableOwner(root, "skills/orchestrate/scripts/batch_run.py");
  const controls = executableOwner(root, "skills/orchestrate/scripts/run_controls.py");
  const python = await pythonOwner(root, env);
  let checked = await preflight(
    python,
    executableOwner(root, "skills/orchestrate/scripts/dispatch_run.py"),
    tasks,
    identity,
    env,
    signal,
  );
  signal.throwIfAborted();
  if (checked.status === "rejected") {
    const preflightErrors = Array.isArray(checked.errors) ? checked.errors as Record<string, unknown>[] : [];
    errors.push(...preflightErrors);
    const rejectedIds = new Set(preflightErrors.map((error) => String(error.task_id)));
    tasks.splice(0, tasks.length, ...tasks.filter((task) => !rejectedIds.has(String(task.id))));
    if (!preflightErrors.length || tasks.length === 0)
      return { status: "rejected", error: checked.error, fix: checked.fix,
        tasks: errors.map((row) => ({ ...row, status: "rejected", state: "terminal" })) };
    checked = await preflight(python, executableOwner(root, "skills/orchestrate/scripts/dispatch_run.py"), tasks, identity, env, signal);
    if (checked.status === "rejected") return rejected(new InputError(String(checked.error ?? "preflight_unavailable"), String(checked.fix ?? "Check task preflight inputs.")));
  }
  const runDir = await initialiseRun(identity, env, root, signal, "batch", tasks);
  if (signal.aborted) rmSync(runDir, { recursive: true, force: true });
  signal.throwIfAborted();
  const manifestPath = stagingPath(runDir, "task-manifest.json");
  writeFileSync(
    manifestPath,
    JSON.stringify(
      {
        schema_version: 1,
        tasks: tasks.map((task) => task.model === undefined ? task : Object.fromEntries(
          Object.entries(task).filter(([key]) => key !== "alias"))),
      },
      null,
      2,
    ) + "\n",
    { flag: "wx", mode: 0o600 },
  );
  launch.onLaunch?.({ runId: shortRunId(runDir), taskId: "*", attempt: 1, runDir });
  const started = startOwner(
    python,
    [owner, "--run-dir", runDir, "--manifest", manifestPath, "--concurrency", String(concurrency)],
    identity,
    env,
    runDir,
    {
      command: python,
      args: [controls, "cancel", "--run-dir", runDir, "--batch-id", FIRST_BATCH_ID, "--wait-seconds", "5"],
      targetDirectory: join(runDir, "dispatch", "batches", FIRST_BATCH_ID),
      cwd: identity.cwd,
      env,
    },
    {
      kind: "batch",
      identifier: FIRST_BATCH_ID,
      routes: checked.routes,
      taskIds: tasks.map((task) => String(task.id)),
      // A writer works in its worktree, a reader in its cwd; either names the lane's project.
      cwdProjects: Object.fromEntries(tasks.flatMap((task) => {
        const directory = task.cwd ?? task.worktree;
        return typeof directory === "string" ? [[String(task.id), projectRoot(directory, env)]] : [];
      })),
      timeout: Math.max(...tasks.map((task) => Number(task.timeout))),
    },
    [manifestPath],
  );
  const completion = await observeOwner(
    started,
    Math.max(0, Math.min(input.wait_seconds ?? 0, Math.floor(55 - (Date.now() - callStarted) / 1000))),
    signal,
  );
  const result = completion === undefined
    ? running(started, "batch", identity, FIRST_BATCH_ID)
    : compactBatch(started, completion);
  const rejectedTasks = errors.map((row) => ({ ...row, status: "rejected", state: "terminal" }));
  const warnings = [...policy.warnings, ...poolWarnings, ...tasks.flatMap((task) => Array.isArray(task.warnings) ? task.warnings : [])];
  return withWarnings({
    ...result,
    ...(rejectedTasks.length ? { tasks: [...((result.tasks as Record<string, unknown>[] | undefined) ?? []), ...rejectedTasks] } : {}),
  }, warnings);
}


export async function dispatchConfiguredBatch(
  input: BatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
  launch: LaunchObserver = {},
): Promise<Record<string, unknown>> {
  try {
    try {
      rejectNestedSandboxDispatch(env);
      rejectOwnSeatClaude(input as unknown as Record<string, any>, identity, env, launch);
    } catch (error) {
      // Refused before placement: every task of the batch is reported, as for any rejected batch.
      const refusal = rejected(error);
      return { ...refusal, tasks: (input.tasks ?? []).map((task, index) => ({ ...refusal, task_id: task.id ?? `task-${index + 1}`, state: "terminal" })) };
    }
    const remote = env.PROVENANT_HOST_LOCAL_ONLY === "1" ? undefined : await federatedLane("dispatch", identity.cwd, input as unknown as Record<string, unknown>, signal);
    if (remote) return remote;
    const localEnv = { ...env, PROVENANT_HOST_LOCAL_ONLY: "1", ...(input.placement_fallback ? { PROVENANT_PLACEMENT_FALLBACK: JSON.stringify(input.placement_fallback) } : {}) };
    const result = await dispatchConfiguredBatchUnchecked(input, identity, signal, localEnv, launch);
    return input.placement_fallback ? { ...result, placement_fallback: input.placement_fallback,
      warnings: [...((result.warnings as string[]) ?? []), `local fallback from ${input.placement_fallback.from_host}: ${input.placement_fallback.reason}${input.placement_fallback.detail ? `: ${input.placement_fallback.detail}` : ""}`] } : result;
  } catch (error) {
    if (signal.aborted && !(error instanceof InputError)) throw error;
    return rejected(error);
  }
}

/**
 * A task of a running batch, named by its own id: cancelling it must not stop
 * its siblings. The owner honours a per-attempt marker that run_controls writes.
 */
async function cancelBatchTask(
  target: Record<string, any>,
  identity: Identity,
  env: NodeJS.ProcessEnv,
): Promise<Record<string, unknown> | undefined> {
  // The row's own attempt is the current one (a queued resume announces attempt N before its history has it).
  const attempt = Number(target.attempt ?? target.attempts?.at(-1)?.attempt);
  if (!Number.isSafeInteger(attempt) || attempt < 1)
    return { status: "rejected", error: "task_not_started",
      fix: `Task ${target.task_id} has not started; cancel the whole batch with its run id ${target.run_id}.` };
  const attemptId = `attempt-${String(attempt).padStart(3, "0")}`;
  if (!["tasks", "dispatch/tasks"].some((tree) => existsSync(join(String(target.run_dir), tree, String(target.task_id), attemptId))))
    return { status: "rejected", error: "attempt_not_started",
      fix: `Task ${target.task_id} attempt ${attempt} is queued and not yet published; retry cancel once it starts, or cancel the whole batch with its run id ${target.run_id}.` };
  const root = productRoot(env);
  const python = await pythonOwner(root, env);
  const controls = executableOwner(root, "skills/orchestrate/scripts/run_controls.py");
  try {
    await execFileAsync(python, [controls, "cancel", "--run-dir", String(target.run_dir), "--task-id", String(target.task_id),
      "--attempt-id", attemptId, "--wait-seconds", "5"], {
      cwd: dirname(runRoot(identity.cwd)), env: pythonOwnerEnvironment(env), timeout: 10_000, maxBuffer: 64 * 1024,
    });
  } catch {
    return { status: "rejected", error: "cancel_unconfirmed",
      fix: `Task ${target.task_id} did not confirm cancellation; check fabric_status, or cancel the batch with ${target.run_id}.` };
  }
  return undefined;
}

export async function cancelConfiguredRun(
  id: string,
  identity: Identity,
  reason?: string,
  env: NodeJS.ProcessEnv = process.env,
  operationId?: string,
): Promise<Record<string, unknown>> {
  const request = { id, reason, operation_id: operationId };
  const remote = await federatedLane("cancel", identity.cwd, request);
  if (remote) return remote;
  id = request.id;
  const rows = await statusRows(identity.cwd, [id]);
  if (!rows.runs) return rows;
  // A task id names that task alone, whatever the metadata says; only a run or batch id reaches
  // the whole-run path below. run_controls resolves the task's own scope.
  const task = rows.runs.find((row) => row.task_id === id && row.run_id !== id);
  if (task) {
    if (task.state === "terminal") return rows;
    const failure = await cancelBatchTask(task, identity, env);
    return failure ?? { ...(await statusRows(identity.cwd, [id])), ...(reason ? { reason } : {}) };
  }
  // A manifest task with no published attempt resolves to a legacy run row; it is still a task.
  const unresolved = rows.runs.find((row) => {
    if (row.run_id === id || row.task_id === id || typeof row.run_dir !== "string") return false;
    try {
      const metadata = JSON.parse(readFileSync(join(row.run_dir, "dispatch-status.json"), "utf8"));
      return Array.isArray(metadata.task_ids) && metadata.batch_id && metadata.task_ids.includes(id);
    } catch { return false; }
  });
  if (unresolved)
    return { status: "rejected", error: "task_not_started",
      fix: `Task ${id} has not started; cancel the whole batch with its run id ${unresolved.run_id}.` };
  // Only a positively matched run or batch identifier may stop a whole run; an unmatched id is never widened.
  const row = rows.runs.find((r) => [r.run_id, r.batch_id, r.run_dir, typeof r.run_dir === "string" ? basename(r.run_dir) : undefined].includes(id));
  if (!row)
    return { status: "rejected", error: "run_not_matched",
      fix: `${id} matches no run or batch id; pass the run id from fabric_status, or a task id whose batch has started.` };
  if (rows.runs.every((row) => row.state === "terminal")) return rows;
  const started = [...activeOwners].find((owner) => owner.runDir === row.run_dir);
  if (started) {
    await requestOwnerCancellation(started);
    await observeOwner(started, 5, new AbortController().signal);
  } else {
    const recorded = findRecordedRun(identity.cwd, row.run_dir);
    if (!recorded)
      return {
        status: "rejected",
        error: "owner_unavailable",
        fix: "Inspect the retained owner record before cancellation.",
      };
    const outcome = await terminateRecordedRun(recorded, undefined, "cancelled");
    if (outcome.reason === "still running")
      return {
        status: "rejected",
        error: "cancel_unconfirmed",
        fix: "Inspect owner/provider liveness; cancellation was not confirmed.",
      };
  }
  return { ...(await statusRows(identity.cwd, [id])), ...(reason ? { reason } : {}) };
}
