import { batchDefaults, mergeBatchTask, dispatchConfiguredBatchUnchecked } from "./batch-execution.js";
// Kept for the Rust supervisor inventory tests, whose imports stay frozen.
export { dispatchConfiguredBatch, cancelConfiguredRun } from "./batch-execution.js";
import { canonical, productRoot, executableOwner, assertFixtureRunRoot } from "./run-root.js";
import {
  normaliseRoute,
  namesExplicitRoute,
  nativeAdapter,
  type RouteInput,
  preflight,
  routeArguments,
  applyDispatchDefaults,
  dispatchDefaults,
  validatePrompt,
  timeoutSeconds,
  ownerPromptPath,
  readRoots,
  workingIdentity,
  rejected,
  InputError,
  type DispatchInput,
  type BatchInput,
  type BatchTaskInput,
} from "./execution-input.js";
export { ACCESS_MODES, DISPATCH_ADAPTERS } from "./execution-input.js";
export type { AccessMode, RouteInput, DispatchInput, BatchInput, BatchTaskInput } from "./execution-input.js";
import { recordSeatRun } from "./seat-index.js";
import { compactRoute, basePaths, compactDispatch, compactBatch, type OwnerCompletion } from "./owner-output.js";
import { execFileSync, execFile, spawn, type ChildProcess } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import {
  appendFileSync,
  closeSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  existsSync,
  readFileSync,
  renameSync,
  statSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { promisify } from "node:util";

import { pythonOwner, pythonOwnerEnvironment } from "./python-owner.js";
import { catalogueSnapshot } from "./catalogue.js";
import { expandPools, nativeFirst, usesPool, withoutEmptySelectors, POOL_FIELDS } from "./pools.js";
import { runRoot, databasePath, projectRoot, withoutGitRedirects, type Identity } from "./identity.js";
import {
  shortRunId,
  readOwnerRecord,
  pruneDispatchRuns,
  readProviderRecord,
  writeOwnerRecord,
  type OwnerRecord,
  type RecordedRun,
} from "./run-registry.js";
import { processStartedAt, processMatches, reapOrphanedRuns, terminateRecordedRun } from "./process-identity.js";
import { statusRows } from "./run-status.js";

import { laneTaskId, Store } from "./store.js";
import { digest } from "./surface.js";

const execFileAsync = promisify(execFile);
import { federatedLane } from "./hosts.js";
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

export const activeOwners = new Set<StartedOwner>();

/**
 * Whether new owners are recorded as their own host. The reaper stops a run
 * whose host is gone, so a caller that exits before its runs finish, such as
 * the CLI, must not be the host their liveness depends on.
 */
let ownersHostThemselves = false;

export function hostOwnersInThemselves(): void {
  ownersHostThemselves = true;
}

let launcherStarted: string | null | undefined;

/** This process's start time, probed once: the launcher identity a run's status records. */
function launcherStartedAt(): string | null {
  return (launcherStarted ??= processStartedAt(process.pid));
}

/** A self-hosted run is orphaned only once its owner is gone. */
function selfHosted(record: OwnerRecord): OwnerRecord {
  return { ...record, host_pid: record.owner_pid, host_started_at: record.owner_started_at };
}

/** Await bounded owner cancellation before releasing the host. */
export async function cancelActiveExecutions(): Promise<void> {
  await Promise.allSettled(
    [...activeOwners].map(async (started) => {
      await requestOwnerCancellation(started);
    }),
  );
}

/**
 * Hand every run this host started to its own owner, so the runs outlive a
 * host restart. The owner is a detached session leader that publishes its own
 * evidence; status, events and cancel already read it from the run records.
 * Synchronous, because a signalled host may not survive an await.
 */
export function releaseActiveExecutions(): void {
  for (const started of activeOwners) {
    const record = started.record;
    // A run already closed has no record to rewrite; writing one would revive it.
    if (record !== undefined && readOwnerRecord(record.run_dir)?.run_token === record.run_token) {
      try {
        writeOwnerRecord(selfHosted(record));
      } catch {
        /* An unwritable record leaves the run to the reaper, as before. */
      }
    }
    started.child.unref();
  }
  activeOwners.clear();
}

function createRunDirectory(identity: Identity, kind: "dispatch" | "batch", routes: readonly { model?: unknown; cli_version?: unknown }[]): string {
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

export async function initialiseRun(
  identity: Identity,
  env: NodeJS.ProcessEnv,
  root: string,
  signal: AbortSignal,
  kind: "dispatch" | "batch" = "dispatch",
  routes: readonly { model?: unknown; cli_version?: unknown }[] = [],
): Promise<string> {
  assertFixtureRunRoot(runRoot(identity.cwd), routes);
  const owner = executableOwner(root, "skills/orchestrate/scripts/run_dir_init.sh");
  await maintainRunRoot(identity, env);
  signal.throwIfAborted();
  const runDir = createRunDirectory(identity, kind, routes);
  try {
    await execFileAsync(owner, [runDir, "--owner-logs"], {
      cwd: identity.cwd,
      env: pythonOwnerEnvironment(env),
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

/**
 * The run a dispatch is about to launch, reported before the owner starts. A
 * throwing observer stops the launch: nothing runs that its caller could not record.
 */
export interface LaunchObserver {
  onLaunch?: (launch: { runId: string; taskId: string; attempt: number; runDir?: string }) => void;
  /** Set by a handoff: its route is inherited, so the own-seat guard for new explicit dispatches does not apply. */
  continuation?: boolean;
  /** Set by a single dispatch's council: its members come from the caller's own `models`/selectors. */
  guardedCouncil?: boolean;
}

export interface OwnerIdentification {
  kind: "dispatch" | "batch";
  identifier: string;
  routes?: unknown;
  taskIds?: string[];
  taskAlias?: string;
  timeout?: number;
  resume?: boolean;
  batchId?: string;
  nextAttempt?: number;
  workspace?: string;
  cwdProjects?: Record<string, string>;
}

/** One index entry per source run in each explicitly targeted project. */
function writeLanePointers(runDir: string, projects: Record<string, string> | undefined,
  sourceProject: string, env: NodeJS.ProcessEnv): void {
  const targets = new Map<string, string[]>();
  for (const [task, project] of Object.entries(projects ?? {})) {
    if (runRoot(project, env) === runRoot(sourceProject, env)) continue;
    const tasks = targets.get(project) ?? [];
    tasks.push(task);
    targets.set(project, tasks);
  }
  // Run ids are short display ids; the full path prevents index collisions.
  const name = createHash("sha256").update(runDir).digest("hex");
  for (const [project, tasks] of targets) {
    const targetRoot = runRoot(project, env);
    const directory = join(targetRoot, "runs/pointers");
    for (const path of [targetRoot, join(targetRoot, "runs"), directory]) {
      mkdirSync(path, { recursive: true, mode: 0o700 });
      if (lstatSync(path).isSymbolicLink()) throw new Error("pointer directory must not be a symlink");
    }
    const path = join(directory, `${name}.json`);
    const temporary = `${path}.${process.pid}.tmp`;
    writeFileSync(temporary, JSON.stringify({ source_project: sourceProject, run_dir: runDir, task_ids: tasks }) + "\n",
      { mode: 0o600 });
    renameSync(temporary, path);
  }
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
    ...pythonOwnerEnvironment(env),
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
  const statusPath = join(runDir, "dispatch-status.json");
  const cwdProjects = identification.cwdProjects;
  const metadata = {
    id: shortRunId(runDir),
    ...(identification.kind === "dispatch"
      ? { task_id: identification.identifier, ...(identification.batchId ? { batch_id: identification.batchId } : {}) }
      : { batch_id: identification.identifier }),
    next_attempt: identification.nextAttempt,
    kind: identification.kind,
    started_at: new Date().toISOString(),
    routes: identification.routes,
    task_ids: identification.taskIds,
    task_alias: identification.taskAlias,
    cwd_projects: cwdProjects,
    timeout_seconds: identification.timeout,
    status: "running",
    owner_stdout: stdoutPath,
    owner_stderr: stderrPath,
    // The launcher: if it dies before an owner is stamped below, readers close the announced attempt.
    host_pid: process.pid,
    host_started_at: launcherStartedAt(),
  };
  writeFileSync(statusPath, JSON.stringify(metadata) + "\n", { mode: 0o600 });
  // The seat keeps the association once this host and launcher are gone.
  recordSeatRun(runDir, env);
  // The pointer is only a discovery index: an unwritable target run root must
  // not fail the dispatch. The lane then stays visible from its source project.
  try {
    const sourceRoot = basename(dirname(runDir)) === "runs" ? dirname(dirname(runDir)) : dirname(runDir);
    writeLanePointers(runDir, cwdProjects, projectRoot(dirname(sourceRoot), env), env);
  } catch (error) {
    Object.assign(metadata, { warnings: [`lane_pointer_failed: ${error instanceof Error ? error.message : String(error)}; ` +
      "the lane is listed only from its source project"] });
    // Atomic: a target reader may already be reading this status.
    const temporary = `${statusPath}.${process.pid}.tmp`;
    try {
      writeFileSync(temporary, JSON.stringify(metadata) + "\n", { mode: 0o600 });
      renameSync(temporary, statusPath);
    } catch { /* The owner stamp below rewrites status with the warning. */ }
  }
  let child: ChildProcess;
  try {
    const stdout = openSync(stdoutPath, "wx", 0o600);
    let stderr: number | undefined;
    try {
      stderr = openSync(stderrPath, "wx", 0o600);
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
      if (stderr !== undefined) closeSync(stderr);
    }
  } catch (error) {
    // No owner started: close the attempt this status announced, so no reader waits on it.
    const message = error instanceof Error ? error.message : String(error);
    try {
      writeFileSync(statusPath, JSON.stringify({ ...metadata, status: "rejected", error: "owner_start_failed",
        fix: `The run owner could not start (${message}); retry.`, finished_at: new Date().toISOString() }) + "\n", { mode: 0o600 });
    } catch {
      /* Readers still close it: no owner and no attempt past queued. */
    }
    throw error;
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
                  outputPath: `${row.run_id}:${laneTaskId(row)}:${row.attempt ?? row.attempt_count}`,
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
    const ownerStartedAt = processStartedAt(pid);
    try {
      // The owner's identity and run token in run status too, so readers still see it and its provider if the record cannot be written.
      writeFileSync(statusPath, JSON.stringify({ ...metadata, owner_pid: pid, owner_started_at: ownerStartedAt, run_token: runToken }) + "\n", { mode: 0o600 });
    } catch {
      /* The owner record below still identifies it. */
    }
    const hosted: OwnerRecord = {
      schema_version: 1,
      kind: identification.kind,
      run_dir: runDir,
      workspace: identity.cwd,
      run_token: runToken,
      owner_pid: pid,
      // A detached child leads the group it was placed in, so the leader is
      // the child itself.
      owner_pgid: pid,
      owner_started_at: ownerStartedAt,
      host_pid: process.pid,
      host_started_at: launcherStartedAt(),
      started_at: new Date().toISOString(),
      owner_stdout: stdoutPath,
      owner_stderr: stderrPath,
      ...(identification.kind === "dispatch"
        ? { task_id: identification.identifier, ...(identification.batchId ? { batch_id: identification.batchId } : {}) }
        : { batch_id: identification.identifier }),
    };
    const record = ownersHostThemselves ? selfHosted(hosted) : hosted;
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

export async function requestOwnerCancellation(started: StartedOwner): Promise<void> {
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

export function running(
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
  env: NodeJS.ProcessEnv,
  launch: LaunchObserver,
): Promise<Record<string, unknown>> {
  const workspaceIdentity = identity;
  const root = productRoot(env);
  const ownSeatGuard = !launch.continuation && namesExplicitRoute(withoutEmptySelectors(input));
  // Routing may replace an alias with a pool selector or a concrete model.
  const callerAlias = input.task_id === undefined ? withoutEmptySelectors(input).alias : undefined;
  const namedTaskId = input.task_id ?? (callerAlias === undefined ? undefined
    : callerAlias.replace(/[^A-Za-z0-9._-]+/gu, "-").replace(/^[^A-Za-z0-9]+/u, "").slice(0, 64) || "task");
  const preferred = nativeFirst(input, identity);
  input = preferred.task;
  let poolWarnings: string[] = preferred.warnings;
  if (usesPool(input)) {
    // A route pick keeps this a single dispatch; a council runs as a batch of its members.
    const taskId = namedTaskId ?? (input.council === undefined && input.models === undefined ? undefined : "council");
    const expanded = await expandPools([{ ...input, ...(taskId === undefined ? {} : { id: taskId }) }],
      await pythonOwner(root, env), root, identity, env, signal);
    if (expanded.errors.length) {
      const error = expanded.errors[0]!;
      return { status: "rejected", error: error.error, fix: error.fix };
    }
    if (expanded.council) {
      const { task_id: _task, wait_seconds, ...controls } = input;
      const result = await dispatchConfiguredBatchUnchecked({
        ...Object.fromEntries(Object.entries(controls).filter(([key]) =>
          !["prompt", "prompt_file", "resume", "handoff", "adapter", "alias", "model", "effort", "confidential", ...POOL_FIELDS].includes(key))),
        tasks: expanded.tasks,
        concurrency: Math.min(8, expanded.tasks.length),
        wait_seconds: wait_seconds ?? DEFAULT_WAIT_SECONDS,
      }, identity, signal, env, { ...launch, guardedCouncil: ownSeatGuard || input.models !== undefined });
      return withWarnings(result, [...preferred.warnings, ...expanded.warnings]);
    }
    // A single pick, or a selector that won precedence, replaces the request's own selectors.
    const { id: _id, ...picked } = expanded.tasks[0]!;
    input = picked as DispatchInput;
    poolWarnings = [...preferred.warnings, ...expanded.warnings];
  }
  const snapshotStarted = performance.now(), catalogue = catalogueSnapshot(root, env);
  const initialRoute = normaliseRoute(input, identity, catalogue, ownSeatGuard);
  input = { ...input, mode: initialRoute.access_mode };
  const providerIdentity = workingIdentity(input, identity);
  input = { ...input, ...(input.cwd === undefined ? {} : { cwd: providerIdentity.cwd }),
    ...(input.prompt_file === undefined ? {} : { prompt_file: ownerPromptPath(identity, input.prompt_file) }) };
  const policy = dispatchDefaults(identity.cwd);
  const route = applyDispatchDefaults(normaliseRoute(input, identity, catalogue, ownSeatGuard), policy.defaults);
  assertFixtureRunRoot(runRoot(workspaceIdentity.cwd), [route]);
  route.warnings = [...new Set([...(initialRoute.warnings ?? []), ...(route.warnings ?? []), ...poolWarnings, ...policy.warnings])];
  validatePrompt(input.prompt, input.prompt_file);
  const roots = [...new Set([...readRoots(identity, input.cwd, input.prompt_file), ...(input.read_roots ?? [])])];
  if (roots.length) route.read_roots = roots;
  if (
    !Number.isInteger(input.wait_seconds ?? DEFAULT_WAIT_SECONDS) ||
    (input.wait_seconds ?? 0) < 0 ||
    (input.wait_seconds ?? 0) > 55
  ) {
    throw new InputError("wait_invalid", "Pass wait_seconds from 0 to 55.");
  }
  const callStarted = Date.now(), validateStarted = performance.now();
  const snapshotMs = performance.now() - snapshotStarted;
  const timeout = timeoutSeconds(input.timeout_seconds ?? policy.defaults.timeout_seconds, input.mode);
  // A writer works in its worktree, a reader in its cwd; either names the lane's project.
  const lanesProject = input.cwd === undefined ? route.worktree : providerIdentity.cwd;
  const taskId = namedTaskId ?? `task-${randomUUID().slice(0, 8)}`;
  const owner = executableOwner(root, "skills/orchestrate/scripts/dispatch_run.py");
  const controls = executableOwner(root, "skills/orchestrate/scripts/run_controls.py");
  const python = await pythonOwner(root, env);
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
  const initStarted = performance.now(), runDir = await initialiseRun(workspaceIdentity, env, root, signal, "dispatch", [route]);
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
    input.intent ?? "ordinary",
    "--timeout",
    String(timeout),
    ...routeArguments(route),
  ];
  try {
    launch.onLaunch?.({ runId: shortRunId(runDir), taskId, attempt: 1, runDir });
  } catch (error) {
    rmSync(runDir, { recursive: true, force: true });
    throw error;
  }
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
      taskAlias: callerAlias,
      cwdProjects: lanesProject === undefined ? undefined : { [taskId]: projectRoot(lanesProject, env) },
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
  return withWarnings(result, route.warnings ?? []);
}

/** A native-model warning gets its own line ahead of the ordinary notes, which share one. */
export function withWarnings(result: Record<string, unknown>, warnings: string[]): Record<string, unknown> {
  if (!warnings.length) return result;
  const native = [...new Set(warnings.filter((warning) => warning.startsWith("NATIVE: ")))];
  const rest = [...new Set(warnings.filter((warning) => !warning.startsWith("NATIVE: ")))];
  return { ...result, warnings: [...native, ...((result.warnings as string[] | undefined) ?? []),
    ...(rest.length ? [`warning: ${rest.join("; ")}`] : [])] };
}

/**
 * The own-seat guard runs on the initiating side before any placement, so a request for the seat's own Claude model
 * is refused the same way whether it would run here or on a peer. A continuation that inherits its route is exempt.
 */
export function rejectOwnSeatClaude(input: Record<string, any>, identity: Identity, env: NodeJS.ProcessEnv, launch: LaunchObserver = {}): void {
  if (launch.continuation || nativeAdapter(identity) !== "claude") return;
  const batch = Array.isArray(input.tasks);
  const defaults = batch ? batchDefaults(input as BatchInput) : {};
  const tasks: Record<string, any>[] = batch
    ? input.tasks.flatMap((task: BatchTaskInput & { _fabric_error?: unknown }, index: number) =>
      task._fabric_error ? [] : [mergeBatchTask(defaults, task, index)])
    : [input];
  const candidates = tasks.flatMap((task) => {
    const effective = nativeFirst(task, identity).task;
    // Malformed councils belong to per-task validation, not a batch-wide guard refusal.
    if (effective.models !== undefined && (!Array.isArray(effective.models) ||
      effective.models.some((model: unknown) => typeof model !== "string"))) return [];
    const routes = effective.models !== undefined ? effective.models.map((model: string) => ({ model })) : [effective];
    return routes.filter(namesExplicitRoute).map((route: RouteInput) => ({ route, taskId: batch ? task.id : undefined }));
  });
  if (!candidates.length) return;
  const catalogue = catalogueSnapshot(productRoot(env), env);
  for (const { route, taskId } of candidates) {
    try {
      normaliseRoute({ ...route }, identity, catalogue, true);
    } catch (error) {
      if (error instanceof InputError && error.code === "native_seat_model") {
        if (batch) throw new InputError(error.code, `Task ${taskId}: ${error.fix} Entire batch refused before placement.`);
        throw error;
      }
    }
  }
}

export function rejectNestedSandboxDispatch(env: NodeJS.ProcessEnv): void {
  if (env.CODEX_SANDBOX && env.PROVENANT_PARENT_ATTEMPT) {
    throw new InputError("nested_dispatch_unavailable",
      "Nested Fabric dispatch is unavailable inside a sandboxed Codex lane; dispatch reviewers from the chair.");
  }
}

export async function dispatchConfiguredProvider(
  input: DispatchInput,
  identity: Identity,
  signal: AbortSignal,
  env: NodeJS.ProcessEnv = process.env,
  launch: LaunchObserver = {},
): Promise<Record<string, unknown>> {
  try {
    rejectNestedSandboxDispatch(env);
    rejectOwnSeatClaude(input, identity, env, launch);
    const remote = env.PROVENANT_HOST_LOCAL_ONLY === "1" ? undefined : await federatedLane("dispatch", identity.cwd, input as Record<string, unknown>, signal);
    if (remote) return remote;
    const localEnv = { ...env, PROVENANT_HOST_LOCAL_ONLY: "1", ...(input.placement_fallback ? { PROVENANT_PLACEMENT_FALLBACK: JSON.stringify(input.placement_fallback) } : {}) };
    const result = await dispatchConfiguredProviderUnchecked(input, identity, signal, localEnv, launch);
    if (input.placement_fallback) {
      const fallback = input.placement_fallback;
      const note = `local fallback from ${fallback.from_host}: ${fallback.reason}${fallback.detail ? `: ${fallback.detail}` : ""}`;
      const runDir = result.run_dir;
      return { ...result, placement_fallback: fallback, warnings: [...((result.warnings as string[]) ?? []), note],
        digest: `${String(result.digest ?? result.status)}\n${note}` };
    }
    return result;
  } catch (error) {
    if (signal.aborted && !(error instanceof InputError)) throw error;
    return rejected(error);
  }
}
