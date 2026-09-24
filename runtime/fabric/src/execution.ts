import {
  normaliseRoute,
  preflight,
  routeArguments,
  validatePrompt,
  timeoutSeconds,
  workingIdentity,
  rejected,
  InputError,
  type DispatchInput,
  type BatchInput,
  type BatchTaskInput,
} from "./execution-input.js";
export { ACCESS_MODES, DISPATCH_ADAPTERS } from "./execution-input.js";
export type { AccessMode, RouteInput, DispatchInput, BatchInput, BatchTaskInput } from "./execution-input.js";
import { compactRoute, basePaths, compactDispatch, compactBatch, type OwnerCompletion } from "./owner-output.js";
import { execFileSync, execFile, spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
  accessSync,
  appendFileSync,
  closeSync,
  constants,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  realpathSync,
  statSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { promisify } from "node:util";

import { catalogueSnapshot, type CatalogueSnapshot } from "./catalogue.js";
import { runRoot, databasePath, withoutGitRedirects, type Identity } from "./identity.js";
import {
  shortRunId,
  fabricStatus,
  statusRows,
  findRecordedRun,
  readOwnerRecord,
  processStartedAt,
  processMatches,
  pruneDispatchRuns,
  reapOrphanedRuns,
  readProviderRecord,
  signalRunGroup,
  terminateRecordedRun,
  writeOwnerRecord,
  type OwnerRecord,
  type RecordedRun,
} from "./run-registry.js";

import { Store } from "./store.js";
import { digest } from "./surface.js";

const execFileAsync = promisify(execFile);
export const MAX_EXECUTION_WAIT_SECONDS = 55;
const DEFAULT_WAIT_SECONDS = 55;
const FIRST_ATTEMPT_ID = "attempt-001";
const FIRST_BATCH_ID = "batch-001";
interface StartedOwner {
  child: ChildProcess;
  completion: Promise<OwnerCompletion>;
  cancellation?: Promise<void>;
  cancelSpec: CancelSpec;
  runDir: string;
  stdoutPath: string;
  stderrPath: string;
  record?: OwnerRecord;
}

interface CancelSpec {
  command: string;
  args: string[];
  targetDirectory: string;
  cwd: string;
  env: NodeJS.ProcessEnv;
}

const activeOwners = new Set<StartedOwner>();

/** Await bounded owner cancellation before releasing the host. */
export async function cancelActiveExecutions(): Promise<void> {
  await Promise.allSettled(
    [...activeOwners].map(async (started) => {
      await requestOwnerCancellation(started);
    }),
  );
}

/** Synchronous signal path when the host cannot await teardown. */
export function terminateActiveExecutionGroups(): void {
  for (const started of activeOwners) {
    const record = started.record;
    if (record === undefined) continue;
    signalRunGroup(record.owner_pid, record.owner_pgid, record.owner_started_at, "SIGTERM");
  }
}

function canonical(path: string): string {
  try {
    return realpathSync(path);
  } catch {
    return resolve(path);
  }
}

export function productRoot(env: NodeJS.ProcessEnv): string {
  const configured = env.AGENT_FABRIC_PRODUCT_ROOT;
  if (configured !== undefined && !isAbsolute(configured)) {
    throw new Error("AGENT_FABRIC_PRODUCT_ROOT must be an absolute path");
  }
  return canonical(configured ?? resolve(import.meta.dirname, "../../.."));
}

export function executableOwner(root: string, relativePath: string): string {
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

export async function pythonOwner(root: string, identity: Identity, env: NodeJS.ProcessEnv): Promise<string> {
  const helper = join(root, "scripts/lib/harness-python.sh");
  const metadata = lstatSync(helper);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    throw new Error(`Python owner selector must be a regular local file: ${helper}`);
  }
  try {
    const { stdout } = await execFileAsync(
      "/bin/bash",
      ["-c", 'source "$1"; run_stdlib -c "import sys; print(sys.executable)"', "provenant-python-owner", helper],
      {
        cwd: identity.cwd,
        env: withoutGitRedirects(env),
        encoding: "utf8",
        timeout: 10_000,
        maxBuffer: 64 * 1024,
      },
    );
    const selected = resolve(stdout.trim());
    const selectedMetadata = statSync(selected);
    accessSync(selected, constants.X_OK);
    if (!isAbsolute(selected) || !selectedMetadata.isFile()) throw new Error("selector returned a non-file");
    return selected;
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw new Error(`Python execution owner is unavailable: ${detail}`, { cause: error });
  }
}

function createRunDirectory(identity: Identity, kind: "dispatch" | "batch"): string {
  const agentRun = runRoot(identity.cwd);
  mkdirSync(agentRun, { recursive: true, mode: 0o700 });
  if (lstatSync(agentRun).isSymbolicLink()) throw new Error("run root must not be a symlink");
  const runs = join(agentRun, "runs");
  mkdirSync(runs, { recursive: true, mode: 0o700 });
  if (lstatSync(runs).isSymbolicLink()) throw new Error("runs directory must not be a symlink");
  try {
    const common = execFileSync(
      "git",
      ["-C", identity.cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"],
      { env: withoutGitRedirects(process.env), encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
    ).trim();
    const exclude = join(common, "info/exclude");
    mkdirSync(dirname(exclude), { recursive: true });
    let current = "";
    try {
      current = readFileSync(exclude, "utf8");
    } catch {
      /* first creation */
    }
    const entries = ["/.agent-run/", "/.worktrees/", "/.work/"].filter((line) => !current.split("\n").includes(line));
    if (entries.length) appendFileSync(exclude, `\n${entries.join("\n")}\n`);
  } catch {
    /* Non-Git workspace or read-only metadata. */
  }
  const stamp = new Date().toISOString().replace(/[-:]/gu, "").replace("T", "-").slice(0, 13);
  const slug =
    basename(identity.cwd)
      .replace(/[^a-zA-Z0-9-]/gu, "-")
      .toLowerCase()
      .slice(0, 32) || "workspace";
  const dir = mkdtempSync(join(runs, `${stamp}-${kind}-${slug}-`));
  return dir;
}

/**
 * The dispatch path is the only scheduler this needs. Before a new run is
 * staged, orphans from a dead host are signalled and runs past their retention
 * are removed with the owner logs written beside them. Reaping runs first, so
 * a run it has just ended can age out on the same pass. Neither step is ever
 * allowed to fail a dispatch.
 */
async function maintainRunRoot(identity: Identity, env: NodeJS.ProcessEnv): Promise<void> {
  const workspace = canonical(identity.cwd);
  try {
    await reapOrphanedRuns(workspace);
  } catch {
    /* Reaping is best effort; the run it protects still starts. */
  }
  try {
    pruneDispatchRuns(workspace, env);
  } catch {
    /* Pruning is best effort; the run it tidies still starts. */
  }
}

async function initialiseRun(
  identity: Identity,
  env: NodeJS.ProcessEnv,
  root: string,
  signal: AbortSignal,
  kind: "dispatch" | "batch" = "dispatch",
): Promise<string> {
  const owner = executableOwner(root, "skills/orchestrate/scripts/run_dir_init.sh");
  await maintainRunRoot(identity, env);
  signal.throwIfAborted();
  const runDir = createRunDirectory(identity, kind);
  try {
    await execFileAsync(owner, [runDir, "--owner-logs"], {
      cwd: identity.cwd,
      env: withoutGitRedirects(env),
      signal,
      killSignal: "SIGKILL",
      timeout: 10_000,
      maxBuffer: 64 * 1024,
    });
    signal.throwIfAborted();
  } catch (error) {
    rmSync(runDir, { recursive: true, force: true });
    const detail = error instanceof Error ? error.message : String(error);
    throw new Error(`execution run setup failed: ${detail}`, { cause: error });
  }
  return runDir;
}

export interface OwnerIdentification {
  kind: "dispatch" | "batch";
  identifier: string;
  routes?: unknown;
  taskIds?: string[];
  timeout?: number;
  resume?: boolean;
  batchId?: string;
  nextAttempt?: number;
  workspace?: string;
}

export function startOwner(
  owner: string,
  args: string[],
  identity: Identity,
  env: NodeJS.ProcessEnv,
  runDir: string,
  cancelSpec: CancelSpec,
  identification: OwnerIdentification,
  cleanupPaths: string[] = [],
): StartedOwner {
  // A token the owner echoes into its provider record, so a stale file left in
  // a reused run directory can never be mistaken for this run's provider.
  const runToken = randomUUID();
  const ownerEnv = {
    ...withoutGitRedirects(env),
    PROVENANT_RUN_TOKEN: runToken,
    PROVENANT_RUN_DIR: runDir,
    PROVENANT_RUN_ID: shortRunId(runDir),
    PROVENANT_CHAIR: env.PROVENANT_CHAIR || identity.agentId,
    PROVENANT_PARENT: identity.agentId,
    PROVENANT_PREFLIGHT_ROUTES: JSON.stringify(
      Object.fromEntries(
        (identification.taskIds ?? []).map((id, index) => [
          id,
          compactRoute((identification.routes as unknown[] | undefined)?.[index]),
        ]),
      ),
    ),
  };
  const suffix = identification.resume ? `-resume-${Date.now()}` : "";
  const stdoutPath = join(runDir, "_owner", `stdout${suffix}.jsonl`);
  const stderrPath = join(runDir, "_owner", `stderr${suffix}.log`);
  // Persist required status before spawning: a failed write cannot orphan a provider.
  writeFileSync(
    join(runDir, "dispatch-status.json"),
    JSON.stringify({
      id: shortRunId(runDir),
      ...(identification.kind === "dispatch"
        ? { task_id: identification.identifier, ...(identification.batchId ? { batch_id: identification.batchId } : {}) }
        : { batch_id: identification.identifier }),
      next_attempt: identification.nextAttempt,
      kind: identification.kind,
      started_at: new Date().toISOString(),
      routes: identification.routes,
      task_ids: identification.taskIds,
      timeout_seconds: identification.timeout,
      status: "running",
      owner_stdout: stdoutPath,
      owner_stderr: stderrPath,
    }) + "\n",
    { mode: 0o600 },
  );
  const stdout = openSync(stdoutPath, "wx", 0o600);
  const stderr = openSync(stderrPath, "wx", 0o600);
  let child: ChildProcess;
  try {
    // Detached, so the owner leads its own process group: that group is what
    // cancellation signals, and it is what makes the recorded pid actionable
    // from a process that never spawned it.
    child = spawn(owner, args, {
      cwd: identity.cwd,
      env: ownerEnv,
      stdio: ["ignore", stdout, stderr],
      detached: true,
    });
  } finally {
    closeSync(stdout);
    closeSync(stderr);
  }
  let started: StartedOwner;
  const completion = new Promise<OwnerCompletion>((resolveCompletion) => {
    let spawnError: string | undefined;
    child.once("error", (error) => {
      spawnError = error.message;
    });
    child.once("close", (exitCode, signal) => {
      void terminateStartedRun(started, started.cancellation === undefined ? "interrupted" : "cancelled")
        // Closure is best effort; a rejection here must never crash the MCP server.
        .catch(() => undefined)
        .finally(async () => {
        activeOwners.delete(started);
        for (const path of cleanupPaths) {
          try {
            unlinkSync(path);
          } catch {
            /* Exact staging input may already be absent. */
          }
        }
        const completed = { exitCode, signal, ...(spawnError === undefined ? {} : { error: spawnError }) };
        try {
          const path = join(runDir, "dispatch-status.json");
          const previous = JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>;
          const compact =
            identification.kind === "dispatch" ? compactDispatch(started, completed) : compactBatch(started, completed);
          // A cancel that killed the owner before it wrote a result stays a
          // cancel; the empty owner output must not overwrite that closure.
          const cancelled =
            (started.cancellation !== undefined || previous.status === "cancelled") &&
            compact.status === "owner_output_invalid";
          const result = cancelled ? { ...compact, status: "cancelled", outcome: "cancelled" } : compact;
          const { fix: _staleFix, ...cleanPrevious } = previous;
          writeFileSync(
            path,
            JSON.stringify({ ...cleanPrevious, ...Object.fromEntries(Object.entries(result).filter(([, value]) => value !== undefined)), finished_at: new Date().toISOString() }) + "\n",
            { mode: 0o600 },
          );
        } catch {
          /* The retained owner/attempt files remain the status fallback. */
        }
        try {
          const result = await statusRows(identification.workspace ?? identity.cwd, [shortRunId(runDir)]);
          const mailbox = new Store(databasePath(env));
          try {
            mailbox.announce(identity);
            for (const row of result.runs ?? [])
              if (row.state === "terminal")
                mailbox.send(identity, identity.agentId, digest(row), {
                  kind: "run_terminal",
                  outputPath: `${row.run_id}:${row.task_id}:${row.attempt ?? row.attempt_count}`,
                });
          } finally {
            mailbox.close();
          }
        } catch {
          /* Durable run evidence still survives unavailable mailbox storage. */
        }
        resolveCompletion(completed);
      });
    });
  });
  started = {
    child,
    completion,
    cancelSpec: { ...cancelSpec, env: ownerEnv },
    runDir,
    stdoutPath,
    stderrPath,
  };
  const pid = child.pid;
  if (pid !== undefined) {
    const record: OwnerRecord = {
      schema_version: 1,
      kind: identification.kind,
      run_dir: runDir,
      workspace: identity.cwd,
      run_token: runToken,
      owner_pid: pid,
      // A detached child leads the group it was placed in, so the leader is
      // the child itself.
      owner_pgid: pid,
      owner_started_at: processStartedAt(pid),
      host_pid: process.pid,
      host_started_at: processStartedAt(process.pid),
      started_at: new Date().toISOString(),
      owner_stdout: stdoutPath,
      owner_stderr: stderrPath,
      ...(identification.kind === "dispatch"
        ? { task_id: identification.identifier, ...(identification.batchId ? { batch_id: identification.batchId } : {}) }
        : { batch_id: identification.identifier }),
    };
    started.record = record;
    try {
      writeOwnerRecord(record);
    } catch {
      /* An unwritable record costs reaping, never the run. */
    }
  }
  activeOwners.add(started);
  return started;
}

function cancellationTargetReady(path: string): boolean {
  try {
    let metadata;
    try {
      metadata = lstatSync(path);
    } catch {
      metadata = lstatSync(path.replace("/dispatch/tasks/", "/tasks/"));
    }
    return metadata.isDirectory() && !metadata.isSymbolicLink();
  } catch {
    return false;
  }
}

/** This host's live view of a run it started, including the provider it recorded. */
function recordedRun(started: StartedOwner): RecordedRun | undefined {
  const record = started.record;
  if (record === undefined) return undefined;
  return {
    ...record,
    run_id: shortRunId(record.run_dir),
    running: true,
    orphaned: false,
    provider: readProviderRecord(record.run_dir, record.run_token),
  };
}

/** Use the durable record even after its owner exits, while a provider may remain. */
async function terminateStartedRun(started: StartedOwner, terminalStatus: "interrupted" | "cancelled" = "interrupted"): Promise<void> {
  const run = recordedRun(started);
  if (run !== undefined) {
    const outcome = await terminateRecordedRun(run, undefined, terminalStatus);
    // A failed process-group signal must not leave this host's direct owner
    // alive. The ChildProcess handle identifies the exact child we spawned.
    if (outcome.reason === "still running" && started.child.exitCode === null && started.child.signalCode === null) {
      started.child.kill("SIGKILL");
    }
    return;
  }
  if (started.child.exitCode === null && started.child.signalCode === null) {
    started.child.kill("SIGTERM");
  }
}

async function requestOwnerCancellation(started: StartedOwner): Promise<void> {
  if (started.cancellation !== undefined) return await started.cancellation;
  started.cancellation = (async () => {
    const deadline = Date.now() + 5_000;
    while (
      started.child.exitCode === null &&
      started.child.signalCode === null &&
      !cancellationTargetReady(started.cancelSpec.targetDirectory) &&
      Date.now() < deadline
    ) {
      await new Promise((resolveWait) => setTimeout(resolveWait, 25));
    }
    if (started.child.exitCode !== null || started.child.signalCode !== null) {
      await terminateStartedRun(started, "cancelled");
      return;
    }
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
    // The owner leads a process group and may hold a provider in a session of
    // its own. Its durable record remains actionable after the owner exits.
    await terminateStartedRun(started, "cancelled");
  })();
  await started.cancellation;
}

export async function observeOwner(
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
    return started.child.exitCode === null && started.child.signalCode === null ? undefined : await started.completion;
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
    const timer = setTimeout(() => {
      finish(undefined);
    }, seconds * 1000);
    signal.addEventListener("abort", aborted, { once: true });
    if (signal.aborted) aborted();
    void started.completion.then(finish);
  });
}

function running(
  started: StartedOwner,
  kind: "dispatch" | "batch",
  identity: Identity,
  identifier: string,
): Record<string, unknown> {
  const paths =
    kind === "dispatch"
      ? {
          ...basePaths(started),
          attempt: join(started.runDir, "dispatch", "tasks", identifier, FIRST_ATTEMPT_ID, "attempt.json"),
        }
      : { ...basePaths(started), summary: join(started.runDir, "dispatch", "batches", FIRST_BATCH_ID, "summary.json") };
  return {
    schema_version: 1,
    id: shortRunId(started.runDir),
    status: "running",
    kind,
    ...(kind === "dispatch" ? { task_id: identifier } : { batch_id: identifier }),
    route: null,
    route_status: "pending",
    workspace: identity.cwd,
    pid: started.child.pid ?? null,
    paths,
  };
}

export function stagingPath(runDir: string, name: string): string {
  return join(runDir, "_owner", name);
}

async function dispatchConfiguredProviderUnchecked(
  input: DispatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
): Promise<Record<string, unknown>> {
  const workspaceIdentity = identity;
  const root = productRoot(env);
  const snapshotStarted = performance.now(), catalogue = catalogueSnapshot(root, env);
  const initialRoute = normaliseRoute(input, identity, catalogue);
  input = { ...input, mode: initialRoute.access_mode };
  const providerIdentity = workingIdentity(input, identity);
  input = { ...input, ...(input.cwd === undefined ? {} : { cwd: providerIdentity.cwd }),
    ...(input.prompt_file === undefined ? {} : { prompt_file: resolve(identity.cwd, input.prompt_file) }) };
  const route = normaliseRoute(input, identity, catalogue);
  route.warnings = [...new Set([...(initialRoute.warnings ?? []), ...(route.warnings ?? [])])];
  validatePrompt(input.prompt, input.prompt_file);
  if (
    !Number.isInteger(input.wait_seconds ?? DEFAULT_WAIT_SECONDS) ||
    (input.wait_seconds ?? 0) < 0 ||
    (input.wait_seconds ?? 0) > 55
  ) {
    throw new InputError("wait_invalid", "Pass wait_seconds from 0 to 55.");
  }
  const callStarted = Date.now(), validateStarted = performance.now();
  const snapshotMs = performance.now() - snapshotStarted;
  const timeout = timeoutSeconds(input.timeout_seconds, input.mode);
  const taskId = input.task_id ?? `task-${randomUUID().slice(0, 8)}`;
  const owner = executableOwner(root, "skills/orchestrate/scripts/dispatch_run.py");
  const controls = executableOwner(root, "skills/orchestrate/scripts/run_controls.py");
  const python = await pythonOwner(root, identity, env);
  const checked = await preflight(
    python,
    owner,
    [
      {
        id: taskId,
        ...route,
        ...(input.prompt === undefined ? { prompt_file: input.prompt_file } : { prompt: input.prompt }),
      },
    ],
    identity,
    env,
    signal,
  );
  signal.throwIfAborted();
  if (checked.status === "rejected") return { status: "rejected", error: checked.error, fix: checked.fix };
  const validateMs = performance.now() - validateStarted - snapshotMs;
  const initStarted = performance.now(), runDir = await initialiseRun(workspaceIdentity, env, root, signal);
  const initMs = performance.now() - initStarted;
  if (signal.aborted) rmSync(runDir, { recursive: true, force: true });
  signal.throwIfAborted();
  const promptPath = input.prompt === undefined ? input.prompt_file! : stagingPath(runDir, "prompt.md");
  if (input.prompt !== undefined) writeFileSync(promptPath, input.prompt, { flag: "wx", mode: 0o600 });
  const args = [
    "--run-dir",
    runDir,
    "--task-id",
    taskId,
    "--prompt-file",
    promptPath,
    "--intent",
    "ordinary",
    "--timeout",
    String(timeout),
    ...routeArguments(route),
  ];
  const started = startOwner(
    python,
    [owner, ...args],
    identity,
    { ...env, PROVENANT_FABRIC_PHASES: JSON.stringify({ validate: validateMs, snapshot: snapshotMs, run_dir_init: initMs, owner_started_at_ms: Date.now() }) },
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
        FIRST_ATTEMPT_ID,
        "--wait-seconds",
        "5",
      ],
      targetDirectory: join(runDir, "dispatch", "tasks", taskId, FIRST_ATTEMPT_ID),
      cwd: identity.cwd,
      env,
    },
    {
      workspace: workspaceIdentity.cwd,
      kind: "dispatch",
      identifier: taskId,
      routes: checked.routes,
      taskIds: [taskId],
      timeout,
    },
    input.prompt === undefined ? [] : [promptPath],
  );
  const completion = await observeOwner(
    started,
    Math.max(
      0,
      Math.min(input.wait_seconds ?? DEFAULT_WAIT_SECONDS, Math.floor(55 - (Date.now() - callStarted) / 1000)),
    ),
    signal,
  );
  const result = completion === undefined
    ? running(started, "dispatch", identity, taskId)
    : compactDispatch(started, completion);
  return route.warnings?.length ? { ...result, warnings: [...((result.warnings as string[] | undefined) ?? []), `warning: ${route.warnings.join("; ")}`] } : result;
}

function normaliseTask(
  task: BatchTaskInput,
  index: number,
  identity: Identity,
  catalogue: CatalogueSnapshot,
): Record<string, unknown> {
  const initialRoute = normaliseRoute(task, identity, catalogue);
  task = { ...task, mode: initialRoute.access_mode };
  validatePrompt(task.prompt, task.prompt_file);
  const providerIdentity = workingIdentity(task, identity);
  task = { ...task, ...(task.cwd === undefined ? {} : { cwd: providerIdentity.cwd }) };
  const route = normaliseRoute(task, identity, catalogue);
  route.warnings = [...new Set([...(initialRoute.warnings ?? []), ...(route.warnings ?? [])])];
  return {
    id: task.id ?? `task-${index + 1}`,
    ...(task.prompt === undefined ? { prompt_file: resolve(identity.cwd, task.prompt_file!) } : { prompt: task.prompt }),
    timeout: timeoutSeconds(task.timeout_seconds, task.mode),
    ...route,
  };
}

async function dispatchConfiguredBatchUnchecked(
  input: BatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
): Promise<Record<string, unknown>> {
  if (input.tasks.length < 1 || input.tasks.length > 64)
    throw new InputError("invalid_input", "tasks must contain 1-64 items");
  const concurrency = input.concurrency ?? Math.min(4, input.tasks.length);
  if (!Number.isInteger(concurrency) || concurrency < 1 || concurrency > 8) {
    throw new InputError("invalid_input", "concurrency must be an integer from 1 to 8");
  }
  if (!Number.isInteger(input.wait_seconds ?? 0) || (input.wait_seconds ?? 0) < 0 || (input.wait_seconds ?? 0) > 55) {
    throw new InputError("wait_invalid", "Pass wait_seconds from 0 to 55.");
  }
  const callStarted = Date.now();
  const root = productRoot(env);
  const catalogue = catalogueSnapshot(root, env);
  const errors: Record<string, unknown>[] = [];
  const tasks = input.tasks.flatMap((task, index) => {
    try {
      const taskError = (task as BatchTaskInput & { _fabric_error?: Record<string, unknown> })._fabric_error;
      if (taskError) {
        errors.push({ task_id: task.id ?? `task-${index + 1}`, ...taskError });
        return [];
      }
      const defaults = Object.fromEntries(Object.entries(input).filter(([key]) =>
        ["adapter", "alias", "model", "effort", "mode", "worktree", "cwd", "network", "sandbox", "add_dirs", "fallback", "timeout_seconds", "context_ceiling", "allow_secrets"].includes(key)));
      return [normaliseTask({ ...defaults, ...task }, index, identity, catalogue)];
    } catch (error) {
      errors.push({ task_id: task.id ?? `task-${index + 1}`, ...rejected(error) });
      return [];
    }
  });
  if (tasks.length === 0) return { status: "rejected", error: errors[0]!.error, fix: errors[0]!.fix, tasks: errors.map((row) => ({ ...row, status: "rejected", state: "terminal" })) };
  const owner = executableOwner(root, "skills/orchestrate/scripts/batch_run.py");
  const controls = executableOwner(root, "skills/orchestrate/scripts/run_controls.py");
  const python = await pythonOwner(root, identity, env);
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
  const runDir = await initialiseRun(identity, env, root, signal, "batch");
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
  const warnings = tasks.flatMap((task) => Array.isArray(task.warnings) ? task.warnings : []);
  return {
    ...result,
    ...(rejectedTasks.length ? { tasks: [...((result.tasks as Record<string, unknown>[] | undefined) ?? []), ...rejectedTasks] } : {}),
    ...(warnings.length ? { warnings: [...((result.warnings as string[] | undefined) ?? []), `warning: ${warnings.join("; ")}`] } : {}),
  };
}


export async function dispatchConfiguredProvider(
  input: DispatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
): Promise<Record<string, unknown>> {
  try {
    return await dispatchConfiguredProviderUnchecked(input, identity, signal, env);
  } catch (error) {
    if (signal.aborted && !(error instanceof InputError)) throw error;
    return rejected(error);
  }
}

export async function dispatchConfiguredBatch(
  input: BatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
): Promise<Record<string, unknown>> {
  try {
    return await dispatchConfiguredBatchUnchecked(input, identity, signal, env);
  } catch (error) {
    if (signal.aborted && !(error instanceof InputError)) throw error;
    return rejected(error);
  }
}

export async function cancelConfiguredRun(
  id: string,
  identity: Identity,
  reason?: string,
): Promise<Record<string, unknown>> {
  const rows = await statusRows(identity.cwd, [id]);
  if (!rows.runs) return rows;
  const row = rows.runs[0]!;
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
