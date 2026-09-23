/** Wave-1 owner output compatibility; v1 attempt evidence stays authoritative. */
import { lstatSync, readFileSync, realpathSync } from "node:fs";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { shortRunId } from "./run-registry.js";

interface OwnerFiles {
  runDir: string;
  stdoutPath: string;
  stderrPath: string;
}
function inside(root: string, candidate: string) {
  const path = relative(root, candidate);
  return path === "" || (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`));
}
function canonical(path: string) {
  try {
    return realpathSync(path);
  } catch {
    return resolve(path);
  }
}

export interface OwnerCompletion {
  exitCode: number | null;
  signal: NodeJS.Signals | null;
  error?: string;
}

const DISPATCH_TERMINAL_STATUSES = new Set(["succeeded", "failed", "blocked", "timed_out", "cancelled"]);
const BATCH_TERMINAL_STATUSES = new Set(["completed", "failed", "cancelled"]);
const RESERVED_UNTYPED_STATUSES = new Set([...DISPATCH_TERMINAL_STATUSES, ...BATCH_TERMINAL_STATUSES, "running"]);

function parseOwnerOutput(path: string): Record<string, unknown> | undefined {
  const lines = readFileSync(path, "utf8")
    .split(/\r?\n/u)
    .filter((line) => line.trim().length > 0);
  for (const line of lines.reverse()) {
    try {
      const value: unknown = JSON.parse(line);
      if (typeof value === "object" && value !== null && !Array.isArray(value)) {
        return value as Record<string, unknown>;
      }
    } catch {
      // Owner diagnostics can precede its final JSON record.
    }
  }
  return undefined;
}

function retainedAbsolute(runDir: string, value: unknown): string | null {
  if (typeof value !== "string" || value.length === 0 || isAbsolute(value)) return null;
  const path = resolve(runDir, value);
  return inside(runDir, path) ? path : null;
}

function retainedRegularFile(runDir: string, value: unknown): string | null {
  const path = retainedAbsolute(runDir, value);
  if (path === null) return null;
  try {
    const metadata = lstatSync(path);
    return metadata.isFile() && !metadata.isSymbolicLink() && inside(canonical(runDir), realpathSync(path))
      ? path
      : null;
  } catch {
    return null;
  }
}

function objectValue(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function nonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function completeRoute(value: unknown): boolean {
  const route = objectValue(value);
  return (
    route !== undefined &&
    ["adapter", "provider_family", "resolved_model", "execution_intent"].every((field) => nonEmptyString(route[field]))
  );
}

function validOwnerRecord(record: Record<string, unknown>, kind: "dispatch" | "batch", runDir: string): boolean {
  if (record.schema_version !== 1 || typeof record.status !== "string" || record.status.length === 0) return false;
  if (record.record_type === undefined) {
    return (
      !RESERVED_UNTYPED_STATUSES.has(record.status) && typeof record.message === "string" && record.message.length > 0
    );
  }
  if (record.record_type !== (kind === "dispatch" ? "dispatch-attempt" : "dispatch-batch")) return false;
  const terminalStatuses = kind === "dispatch" ? DISPATCH_TERMINAL_STATUSES : BATCH_TERMINAL_STATUSES;
  if (!terminalStatuses.has(record.status)) return false;
  if (kind === "dispatch" && record.status === "succeeded") {
    const result = objectValue(record.result);
    const stderr = objectValue(record.stderr);
    return (
      nonEmptyString(record.outcome) &&
      nonEmptyString(record.task_id) &&
      nonEmptyString(record.attempt_id) &&
      retainedRegularFile(runDir, record.attempt_path) !== null &&
      retainedRegularFile(runDir, result?.path) !== null &&
      retainedRegularFile(runDir, stderr?.path) !== null &&
      completeRoute(record.route)
    );
  }
  if (kind === "batch" && record.status === "completed") {
    const tasks = Array.isArray(record.tasks) ? record.tasks : [];
    const counts = objectValue(record.counts);
    return (
      nonEmptyString(record.batch_id) &&
      Number.isInteger(record.task_count) &&
      Number(record.task_count) > 0 &&
      Number.isInteger(record.concurrency) &&
      Number(record.concurrency) > 0 &&
      tasks.length === record.task_count &&
      tasks.every((value) => {
        const task = objectValue(value);
        if (task === undefined || !nonEmptyString(task.task_id) || !DISPATCH_TERMINAL_STATUSES.has(String(task.status)))
          return false;
        return (
          task.status !== "succeeded" ||
          (nonEmptyString(task.outcome) &&
            retainedRegularFile(runDir, task.attempt_path) !== null &&
            retainedRegularFile(runDir, task.result_path) !== null &&
            completeRoute(task.route))
        );
      }) &&
      counts !== undefined &&
      Object.keys(counts).length > 0 &&
      retainedRegularFile(runDir, record.summary_path) !== null
    );
  }
  return true;
}

function completionConflict(completion: OwnerCompletion, successful: boolean): boolean {
  return completion.error !== undefined || completion.signal !== null || (successful && completion.exitCode !== 0);
}

export function compactRoute(value: unknown): Record<string, string> | null {
  const route = objectValue(value);
  if (route === undefined) return null;
  const compact: Record<string, string> = {};
  for (const field of [
    "adapter",
    "alias",
    "model",
    "effort",
    "provider_family",
    "model_family",
    "resolved_model",
    "endpoint_provider",
    "execution_intent",
  ]) {
    if (typeof route[field] === "string" && route[field].length > 0) compact[field] = route[field];
  }
  return Object.keys(compact).length === 0 ? null : compact;
}

export function basePaths(started: OwnerFiles): Record<string, string> {
  return {
    run_dir: started.runDir,
    owner_stdout: started.stdoutPath,
    owner_stderr: started.stderrPath,
  };
}

function emptyProviderResult(record: Record<string, unknown>, runDir: string): boolean {
  if (record.record_type !== "dispatch-attempt" || record.status !== "succeeded") return false;
  const result = objectValue(record.result);
  const path = retainedRegularFile(runDir, result?.path);
  return path !== null && lstatSync(path).size === 0;
}

export function compactDispatch(started: OwnerFiles, completion: OwnerCompletion): Record<string, unknown> {
  const record = parseOwnerOutput(started.stdoutPath);
  if (record?.schema === "fabric.attempt.v1")
    return {
      ...record,
      schema: "fabric.status.v1",
      id: record.run_id,
      run_dir: started.runDir,
      attempts: [record],
      attempt_count: record.attempt,
      paths: { ...basePaths(started), ...objectValue(record.paths) },
    };
  if (record !== undefined && emptyProviderResult(record, started.runDir)) {
    const result = objectValue(record.result);
    const stderr = objectValue(record.stderr);
    return {
      schema_version: 1,
      id: shortRunId(started.runDir),
      status: "failed",
      outcome: "empty_output",
      task_id: record.task_id,
      attempt_id: record.attempt_id,
      route: compactRoute(record.route),
      owner_exit: completion.exitCode,
      paths: {
        ...basePaths(started),
        attempt: retainedAbsolute(started.runDir, record.attempt_path),
        result: retainedAbsolute(started.runDir, result?.path),
        stderr: retainedAbsolute(started.runDir, stderr?.path),
      },
    };
  }
  if (record === undefined || !validOwnerRecord(record, "dispatch", started.runDir)) {
    return {
      schema_version: 1,
      id: shortRunId(started.runDir),
      status: "owner_output_invalid",
      owner_exit: completion.exitCode,
      owner_signal: completion.signal,
      ...(completion.error === undefined ? {} : { message: completion.error }),
      paths: basePaths(started),
    };
  }
  if (completionConflict(completion, record.status === "succeeded")) {
    return {
      schema_version: 1,
      id: shortRunId(started.runDir),
      status: "owner_completion_conflict",
      owner_status: record.status,
      owner_exit: completion.exitCode,
      owner_signal: completion.signal,
      ...(completion.error === undefined ? {} : { message: completion.error }),
      paths: basePaths(started),
    };
  }
  const result = objectValue(record.result);
  const stderr = objectValue(record.stderr);
  return {
    schema_version: 1,
    id: shortRunId(started.runDir),
    status: record.status,
    ...(record.message === undefined ? {} : { message: record.message }),
    outcome: record.outcome,
    task_id: record.task_id,
    attempt_id: record.attempt_id,
    route: compactRoute(record.route),
    owner_exit: completion.exitCode,
    paths: {
      ...basePaths(started),
      attempt: retainedAbsolute(started.runDir, record.attempt_path),
      result: retainedAbsolute(started.runDir, result?.path),
      stderr: retainedAbsolute(started.runDir, stderr?.path),
    },
  };
}

export function compactBatch(started: OwnerFiles, completion: OwnerCompletion): Record<string, unknown> {
  const record = parseOwnerOutput(started.stdoutPath);
  if (record?.schema === "fabric.status.v1" || record?.schema === "fabric.batch.v1")
    return { ...record, id: shortRunId(started.runDir), paths: basePaths(started) };
  if (record === undefined || !validOwnerRecord(record, "batch", started.runDir)) {
    return {
      schema_version: 1,
      id: shortRunId(started.runDir),
      status: "owner_output_invalid",
      owner_exit: completion.exitCode,
      owner_signal: completion.signal,
      ...(completion.error === undefined ? {} : { message: completion.error }),
      paths: basePaths(started),
    };
  }
  const allTasksSucceeded =
    Array.isArray(record.tasks) &&
    record.tasks.length > 0 &&
    record.tasks.every((value) => {
      const task = objectValue(value);
      const path = retainedRegularFile(started.runDir, task?.result_path);
      return task?.status === "succeeded" && path !== null && lstatSync(path).size > 0;
    });
  if (completionConflict(completion, allTasksSucceeded)) {
    return {
      schema_version: 1,
      id: shortRunId(started.runDir),
      status: "owner_completion_conflict",
      owner_status: record.status,
      owner_exit: completion.exitCode,
      owner_signal: completion.signal,
      ...(completion.error === undefined ? {} : { message: completion.error }),
      paths: basePaths(started),
    };
  }
  const tasks = Array.isArray(record.tasks)
    ? record.tasks.map((value) => {
        const task = objectValue(value);
        if (task === undefined) return { status: "owner_task_invalid" };
        const resultPath = retainedRegularFile(started.runDir, task.result_path);
        const empty = task.status === "succeeded" && resultPath !== null && lstatSync(resultPath).size === 0;
        return {
          task_id: task.task_id,
          status: empty ? "failed" : task.status,
          outcome: empty ? "empty_output" : task.outcome,
          route: compactRoute(task.route),
          paths: {
            attempt: retainedAbsolute(started.runDir, task.attempt_path),
            result: retainedAbsolute(started.runDir, task.result_path),
          },
        };
      })
    : [];
  return {
    schema_version: 1,
    id: shortRunId(started.runDir),
    status: record.status,
    ...(record.message === undefined ? {} : { message: record.message }),
    batch_id: record.batch_id,
    task_count: record.task_count,
    concurrency: record.concurrency,
    counts: tasks.reduce<Record<string, number>>((counts, task) => {
      const key = String(task.status);
      counts[key] = (counts[key] ?? 0) + 1;
      return counts;
    }, {}),
    tasks,
    owner_exit: completion.exitCode,
    paths: {
      ...basePaths(started),
      summary: retainedAbsolute(started.runDir, record.summary_path),
    },
  };
}
