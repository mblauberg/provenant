import { execFile, spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
  accessSync,
  closeSync,
  constants,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  realpathSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { promisify } from "node:util";

import { withoutGitRedirects, type Identity } from "./identity.js";

const execFileAsync = promisify(execFile);
export const MAX_EXECUTION_WAIT_SECONDS = 55;
const DEFAULT_WAIT_SECONDS = 55;
const DEFAULT_TIMEOUT_SECONDS = 900;
const SUPPORTED_ADAPTERS = new Set(["agy", "claude", "codex", "cursor", "kiro", "opencode"]);

export interface RouteInput {
  adapter?: string;
  alias?: string;
  task_class?: string;
  model?: string;
  role?: string;
  effort?: string;
  orchestrator_family?: string;
  risk_tier?: string;
  model_override_tier?: string;
  reviewer_id?: string;
}

export interface DispatchInput extends RouteInput {
  prompt?: string;
  prompt_file?: string;
  task_id?: string;
  timeout_seconds?: number;
  wait_seconds?: number;
}

export interface BatchTaskInput extends RouteInput {
  id?: string;
  prompt?: string;
  prompt_file?: string;
  timeout_seconds?: number;
}

export interface BatchInput {
  tasks: BatchTaskInput[];
  concurrency?: number;
  wait_seconds?: number;
}

interface OwnerCompletion {
  exitCode: number | null;
  signal: NodeJS.Signals | null;
  error?: string;
}

interface StartedOwner {
  child: ChildProcess;
  completion: Promise<OwnerCompletion>;
  cancellation?: Promise<void>;
  cancelSpec: CancelSpec;
  runDir: string;
  stdoutPath: string;
  stderrPath: string;
}

interface CancelSpec {
  command: string;
  args: string[];
  targetDirectory: string;
  cwd: string;
  env: NodeJS.ProcessEnv;
}

type NormalisedRoute = Required<Pick<RouteInput, "adapter" | "role">> & Omit<RouteInput, "adapter" | "role">;

const activeOwners = new Set<StartedOwner>();

export function cancelActiveExecutions(): void {
  for (const started of activeOwners) {
    void requestOwnerCancellation(started);
  }
}

function inside(root: string, candidate: string): boolean {
  const path = relative(root, candidate);
  return path === "" || (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`));
}

function canonical(path: string): string {
  try {
    return realpathSync(path);
  } catch {
    return resolve(path);
  }
}

function productRoot(env: NodeJS.ProcessEnv): string {
  const configured = env.AGENT_FABRIC_PRODUCT_ROOT;
  if (configured !== undefined && !isAbsolute(configured)) {
    throw new Error("AGENT_FABRIC_PRODUCT_ROOT must be an absolute path");
  }
  return canonical(configured ?? resolve(import.meta.dirname, "../../.."));
}

function executableOwner(root: string, relativePath: string): string {
  const path = join(root, relativePath);
  let metadata;
  try {
    metadata = lstatSync(path);
    accessSync(path, constants.X_OK);
  } catch {
    throw new Error(`execution owner is unavailable: ${path}`);
  }
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    throw new Error(`execution owner must be a regular local file: ${path}`);
  }
  return path;
}

async function pythonOwner(root: string, identity: Identity, env: NodeJS.ProcessEnv): Promise<string> {
  const helper = join(root, "scripts/lib/harness-python.sh");
  const metadata = lstatSync(helper);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    throw new Error(`Python owner selector must be a regular local file: ${helper}`);
  }
  try {
    const { stdout } = await execFileAsync("/bin/bash", [
      "-c",
      'source "$1"; run_stdlib -c "import sys; print(sys.executable)"',
      "provenant-python-owner",
      helper,
    ], {
      cwd: identity.cwd,
      env: withoutGitRedirects(env),
      encoding: "utf8",
      timeout: 10_000,
      maxBuffer: 64 * 1024,
    });
    const selected = canonical(stdout.trim());
    const selectedMetadata = lstatSync(selected);
    accessSync(selected, constants.X_OK);
    if (!isAbsolute(selected) || !selectedMetadata.isFile()) throw new Error("selector returned a non-file");
    return selected;
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw new Error(`Python execution owner is unavailable: ${detail}`, { cause: error });
  }
}

function createRunDirectory(identity: Identity): string {
  const workspace = canonical(identity.cwd);
  const agentRun = join(workspace, ".agent-run");
  mkdirSync(agentRun, { recursive: true, mode: 0o700 });
  const metadata = lstatSync(agentRun);
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    throw new Error(`execution run root must be a local directory: ${agentRun}`);
  }
  const runRoot = canonical(agentRun);
  if (!inside(workspace, runRoot)) throw new Error("execution run root escapes the caller workspace");
  return mkdtempSync(join(runRoot, "mcp-"));
}

async function initialiseRun(identity: Identity, env: NodeJS.ProcessEnv, root: string): Promise<string> {
  const owner = executableOwner(root, "skills/orchestrate/scripts/run_dir_init.sh");
  const runDir = createRunDirectory(identity);
  try {
    await execFileAsync(owner, [runDir], {
      cwd: identity.cwd,
      env,
      timeout: 10_000,
      maxBuffer: 64 * 1024,
    });
  } catch (error) {
    rmSync(runDir, { recursive: true, force: true });
    const detail = error instanceof Error ? error.message : String(error);
    throw new Error(`execution run setup failed: ${detail}`, { cause: error });
  }
  return runDir;
}

function normaliseRoute(input: RouteInput, identity: Identity): NormalisedRoute {
  const adapter = input.adapter ?? (SUPPORTED_ADAPTERS.has(identity.provider) ? identity.provider : undefined);
  if (adapter === undefined) throw new Error("adapter is required when the Fabric seat is not a provider adapter");
  const selectors = [input.alias, input.task_class, input.model].filter((value) => value !== undefined);
  if (selectors.length > 1) throw new Error("provide at most one of alias, task_class or model");
  return {
    adapter,
    role: input.role ?? "worker",
    ...(input.task_class !== undefined
      ? { task_class: input.task_class }
      : input.model !== undefined ? { model: input.model } : { alias: input.alias ?? "workhorse" }),
    ...(input.effort === undefined ? {} : { effort: input.effort }),
    ...(input.orchestrator_family === undefined ? {} : { orchestrator_family: input.orchestrator_family }),
    ...(input.risk_tier === undefined ? {} : { risk_tier: input.risk_tier }),
    ...(input.model_override_tier === undefined ? {} : { model_override_tier: input.model_override_tier }),
    ...(input.reviewer_id === undefined ? {} : { reviewer_id: input.reviewer_id }),
  };
}

function routeArguments(route: NormalisedRoute): string[] {
  const args: string[] = [];
  for (const [key, value] of Object.entries(route)) {
    if (value !== undefined) args.push(`--${key.replaceAll("_", "-")}`, value);
  }
  return args;
}

function validatePrompt(prompt: string | undefined, promptFile: string | undefined): void {
  if ((prompt === undefined) === (promptFile === undefined)) {
    throw new Error("provide exactly one of prompt or prompt_file");
  }
}

function startOwner(
  owner: string,
  args: string[],
  identity: Identity,
  env: NodeJS.ProcessEnv,
  runDir: string,
  cancelSpec: CancelSpec,
  cleanupPaths: string[] = [],
): StartedOwner {
  const logPrefix = join(dirname(runDir), basename(runDir));
  const stdoutPath = `${logPrefix}-owner.stdout.jsonl`;
  const stderrPath = `${logPrefix}-owner.stderr.log`;
  const stdout = openSync(stdoutPath, "wx", 0o600);
  const stderr = openSync(stderrPath, "wx", 0o600);
  let child: ChildProcess;
  try {
    child = spawn(owner, args, {
      cwd: identity.cwd,
      env,
      stdio: ["ignore", stdout, stderr],
    });
  } finally {
    closeSync(stdout);
    closeSync(stderr);
  }
  let started: StartedOwner;
  const completion = new Promise<OwnerCompletion>((resolveCompletion) => {
    let spawnError: string | undefined;
    child.once("error", (error) => { spawnError = error.message; });
    child.once("close", (exitCode, signal) => {
      activeOwners.delete(started);
      for (const path of cleanupPaths) {
        try { unlinkSync(path); } catch { /* Exact staging input may already be absent. */ }
      }
      resolveCompletion({ exitCode, signal, ...(spawnError === undefined ? {} : { error: spawnError }) });
    });
  });
  started = { child, completion, cancelSpec, runDir, stdoutPath, stderrPath };
  activeOwners.add(started);
  return started;
}

function cancellationTargetReady(path: string): boolean {
  try {
    const metadata = lstatSync(path);
    return metadata.isDirectory() && !metadata.isSymbolicLink();
  } catch {
    return false;
  }
}

async function requestOwnerCancellation(started: StartedOwner): Promise<void> {
  if (started.cancellation !== undefined) return await started.cancellation;
  started.cancellation = (async () => {
    const deadline = Date.now() + 5_000;
    while (started.child.exitCode === null && started.child.signalCode === null
      && !cancellationTargetReady(started.cancelSpec.targetDirectory) && Date.now() < deadline) {
      await new Promise((resolveWait) => setTimeout(resolveWait, 25));
    }
    if (started.child.exitCode !== null || started.child.signalCode !== null) return;
    if (cancellationTargetReady(started.cancelSpec.targetDirectory)) {
      try {
        await execFileAsync(started.cancelSpec.command, started.cancelSpec.args, {
          cwd: started.cancelSpec.cwd,
          env: started.cancelSpec.env,
          timeout: 10_000,
          maxBuffer: 64 * 1024,
        });
      } catch {
        // The owner still receives a bounded graceful signal below.
      }
    }
    if (started.child.exitCode === null && started.child.signalCode === null) started.child.kill("SIGTERM");
  })();
  await started.cancellation;
}

async function observeOwner(
  started: StartedOwner,
  waitSeconds: number | undefined,
  signal: AbortSignal,
): Promise<OwnerCompletion | undefined> {
  const seconds = waitSeconds ?? DEFAULT_WAIT_SECONDS;
  if (!Number.isInteger(seconds) || seconds < 0 || seconds > MAX_EXECUTION_WAIT_SECONDS) {
    throw new Error(`wait_seconds must be an integer from 0 to ${MAX_EXECUTION_WAIT_SECONDS}`);
  }
  if (signal.aborted) {
    await requestOwnerCancellation(started);
    signal.throwIfAborted();
  }
  if (seconds === 0) {
    return started.child.exitCode === null && started.child.signalCode === null
      ? undefined
      : await started.completion;
  }
  return await new Promise<OwnerCompletion | undefined>((resolveWait, rejectWait) => {
    let settled = false;
    const finish = (value: OwnerCompletion | undefined): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal.removeEventListener("abort", aborted);
      resolveWait(value);
    };
    const aborted = (): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal.removeEventListener("abort", aborted);
      void requestOwnerCancellation(started).finally(() => {
        rejectWait(signal.reason instanceof Error ? signal.reason : new Error("execution wait cancelled"));
      });
    };
    const timer = setTimeout(() => { finish(undefined); }, seconds * 1000);
    signal.addEventListener("abort", aborted, { once: true });
    if (signal.aborted) aborted();
    void started.completion.then(finish);
  });
}

function parseOwnerOutput(path: string): Record<string, unknown> | undefined {
  const lines = readFileSync(path, "utf8").split(/\r?\n/u).filter((line) => line.trim().length > 0);
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

function objectValue(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function nonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

function validOwnerRecord(
  record: Record<string, unknown>,
  kind: "dispatch" | "batch",
  runDir: string,
): boolean {
  if (record.schema_version !== 1 || typeof record.status !== "string" || record.status.length === 0) return false;
  if (record.record_type === undefined) {
    return record.status !== "succeeded" && record.status !== "completed" &&
      typeof record.message === "string" && record.message.length > 0;
  }
  if (record.record_type !== (kind === "dispatch" ? "dispatch-attempt" : "dispatch-batch")) return false;
  if (kind === "dispatch" && record.status === "succeeded") {
    const result = objectValue(record.result);
    const stderr = objectValue(record.stderr);
    return nonEmptyString(record.outcome) && nonEmptyString(record.task_id) && nonEmptyString(record.attempt_id) &&
      retainedAbsolute(runDir, record.attempt_path) !== null &&
      retainedAbsolute(runDir, result?.path) !== null &&
      retainedAbsolute(runDir, stderr?.path) !== null && compactRoute(record.route) !== null;
  }
  if (kind === "batch" && record.status === "completed") {
    const tasks = Array.isArray(record.tasks) ? record.tasks : [];
    const counts = objectValue(record.counts);
    return nonEmptyString(record.batch_id) && Number.isInteger(record.task_count) && Number(record.task_count) > 0 &&
      Number.isInteger(record.concurrency) && Number(record.concurrency) > 0 &&
      tasks.length === record.task_count && tasks.every((value) => {
        const task = objectValue(value);
        return task !== undefined && nonEmptyString(task.task_id) && nonEmptyString(task.status);
      }) && counts !== undefined && Object.keys(counts).length > 0 &&
      retainedAbsolute(runDir, record.summary_path) !== null;
  }
  return true;
}

function completionConflict(completion: OwnerCompletion, successful: boolean): boolean {
  return completion.error !== undefined || completion.signal !== null || (successful && completion.exitCode !== 0);
}

function compactRoute(value: unknown): Record<string, string> | null {
  const route = objectValue(value);
  if (route === undefined) return null;
  const compact: Record<string, string> = {};
  for (const field of [
    "adapter", "provider_family", "model_family", "resolved_model", "endpoint_provider", "execution_intent",
  ]) {
    if (typeof route[field] === "string" && route[field].length > 0) compact[field] = route[field];
  }
  return Object.keys(compact).length === 0 ? null : compact;
}

function basePaths(started: StartedOwner): Record<string, string> {
  return {
    run_dir: started.runDir,
    owner_stdout: started.stdoutPath,
    owner_stderr: started.stderrPath,
  };
}

function compactDispatch(started: StartedOwner, completion: OwnerCompletion): Record<string, unknown> {
  const record = parseOwnerOutput(started.stdoutPath);
  if (record === undefined || !validOwnerRecord(record, "dispatch", started.runDir)) {
    return {
      schema_version: 1,
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
    status: record.status,
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

function compactBatch(started: StartedOwner, completion: OwnerCompletion): Record<string, unknown> {
  const record = parseOwnerOutput(started.stdoutPath);
  if (record === undefined || !validOwnerRecord(record, "batch", started.runDir)) {
    return {
      schema_version: 1,
      status: "owner_output_invalid",
      owner_exit: completion.exitCode,
      owner_signal: completion.signal,
      ...(completion.error === undefined ? {} : { message: completion.error }),
      paths: basePaths(started),
    };
  }
  const allTasksSucceeded = Array.isArray(record.tasks) && record.tasks.length > 0 &&
    record.tasks.every((value) => objectValue(value)?.status === "succeeded");
  if (completionConflict(completion, allTasksSucceeded)) {
    return {
      schema_version: 1,
      status: "owner_completion_conflict",
      owner_status: record.status,
      owner_exit: completion.exitCode,
      owner_signal: completion.signal,
      ...(completion.error === undefined ? {} : { message: completion.error }),
      paths: basePaths(started),
    };
  }
  const tasks = Array.isArray(record.tasks) ? record.tasks.map((value) => {
    const task = objectValue(value);
    if (task === undefined) return { status: "owner_task_invalid" };
    return {
      task_id: task.task_id,
      status: task.status,
      outcome: task.outcome,
      route: compactRoute(task.route),
      paths: {
        attempt: retainedAbsolute(started.runDir, task.attempt_path),
        result: retainedAbsolute(started.runDir, task.result_path),
      },
    };
  }) : [];
  return {
    schema_version: 1,
    status: record.status,
    batch_id: record.batch_id,
    task_count: record.task_count,
    concurrency: record.concurrency,
    counts: record.counts,
    tasks,
    owner_exit: completion.exitCode,
    paths: {
      ...basePaths(started),
      summary: retainedAbsolute(started.runDir, record.summary_path),
    },
  };
}

function running(started: StartedOwner, kind: "dispatch" | "batch", identity: Identity): Record<string, unknown> {
  return {
    schema_version: 1,
    status: "running",
    kind,
    route: null,
    route_status: "pending",
    workspace: identity.cwd,
    pid: started.child.pid ?? null,
    paths: basePaths(started),
  };
}

function timeoutSeconds(value: number | undefined): number {
  const timeout = value ?? DEFAULT_TIMEOUT_SECONDS;
  if (!Number.isFinite(timeout) || timeout <= 0) throw new Error("timeout_seconds must be finite and positive");
  return timeout;
}

function stagingPath(runDir: string, name: string): string {
  return join(dirname(runDir), `${basename(runDir)}-${name}`);
}

export async function dispatchConfiguredProvider(
  input: DispatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
): Promise<Record<string, unknown>> {
  validatePrompt(input.prompt, input.prompt_file);
  const route = normaliseRoute(input, identity);
  const timeout = timeoutSeconds(input.timeout_seconds);
  const taskId = input.task_id ?? `task-${randomUUID().slice(0, 8)}`;
  const root = productRoot(env);
  const owner = executableOwner(root, "skills/orchestrate/scripts/dispatch_run.py");
  const controls = executableOwner(root, "skills/orchestrate/scripts/run_controls.py");
  const python = await pythonOwner(root, identity, env);
  const runDir = await initialiseRun(identity, env, root);
  const promptPath = input.prompt === undefined ? input.prompt_file! : stagingPath(runDir, "prompt.md");
  if (input.prompt !== undefined) writeFileSync(promptPath, input.prompt, { flag: "wx", mode: 0o600 });
  const args = [
    "--run-dir", runDir,
    "--task-id", taskId,
    "--prompt-file", promptPath,
    "--intent", "ordinary",
    "--timeout", String(timeout),
    ...routeArguments(route),
  ];
  const started = startOwner(python, [owner, ...args], identity, env, runDir, {
    command: python,
    args: [controls, "cancel", "--run-dir", runDir, "--task-id", taskId,
      "--attempt-id", "attempt-001", "--wait-seconds", "5"],
    targetDirectory: join(runDir, "dispatch", "tasks", taskId, "attempt-001"),
    cwd: identity.cwd,
    env,
  }, input.prompt === undefined ? [] : [promptPath]);
  const completion = await observeOwner(started, input.wait_seconds, signal);
  return completion === undefined ? running(started, "dispatch", identity) : compactDispatch(started, completion);
}

function normaliseTask(task: BatchTaskInput, index: number, identity: Identity): Record<string, unknown> {
  validatePrompt(task.prompt, task.prompt_file);
  return {
    id: task.id ?? `task-${index + 1}`,
    ...(task.prompt === undefined ? { prompt_file: task.prompt_file } : { prompt: task.prompt }),
    timeout: timeoutSeconds(task.timeout_seconds),
    ...normaliseRoute(task, identity),
  };
}

export async function dispatchConfiguredBatch(
  input: BatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
): Promise<Record<string, unknown>> {
  if (input.tasks.length < 1 || input.tasks.length > 64) throw new Error("tasks must contain 1-64 items");
  const concurrency = input.concurrency ?? Math.min(4, input.tasks.length);
  if (!Number.isInteger(concurrency) || concurrency < 1 || concurrency > 8) {
    throw new Error("concurrency must be an integer from 1 to 8");
  }
  const tasks = input.tasks.map((task, index) => normaliseTask(task, index, identity));
  const root = productRoot(env);
  const owner = executableOwner(root, "skills/orchestrate/scripts/batch_run.py");
  const controls = executableOwner(root, "skills/orchestrate/scripts/run_controls.py");
  const python = await pythonOwner(root, identity, env);
  const runDir = await initialiseRun(identity, env, root);
  const manifestPath = stagingPath(runDir, "task-manifest.json");
  writeFileSync(manifestPath, JSON.stringify({
    schema_version: 1,
    tasks,
  }, null, 2) + "\n", { flag: "wx", mode: 0o600 });
  const started = startOwner(python, [owner,
    "--run-dir", runDir,
    "--manifest", manifestPath,
    "--concurrency", String(concurrency),
  ], identity, env, runDir, {
    command: python,
    args: [controls, "cancel", "--run-dir", runDir, "--batch-id", "batch-001",
      "--wait-seconds", "5"],
    targetDirectory: join(runDir, "dispatch", "batches", "batch-001"),
    cwd: identity.cwd,
    env,
  }, [manifestPath]);
  const completion = await observeOwner(started, input.wait_seconds, signal);
  return completion === undefined ? running(started, "batch", identity) : compactBatch(started, completion);
}
