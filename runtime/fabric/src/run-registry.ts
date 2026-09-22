/**
 * What survives an MCP restart about a dispatch that is still running.
 *
 * A dispatch owner is spawned detached, so it outlives the host that started
 * it. The only durable handle on it is what the host writes into the run
 * directory before walking away: the owner's pid, the process group it leads,
 * and enough identity to tell that pid apart from a recycled one. Everything
 * here reads and acts on that record, from any process, with no daemon.
 *
 * Known bound: a signal to a process group cannot reach a descendant that
 * called setsid() for itself. skills/_shared/bounded_process.py documents the
 * same limit. The provider record narrows it — dispatch_run.py records the
 * provider's own group after spawning it — but a provider that starts a third
 * session of its own is beyond any group signal, and this module does not
 * pretend otherwise.
 */
import { execFileSync } from "node:child_process";
import {
  lstatSync, readFileSync, realpathSync, readdirSync, rmSync, statSync, unlinkSync, writeFileSync,
} from "node:fs";
import { basename, isAbsolute, join, relative, resolve, sep } from "node:path";

export const OWNER_RECORD_NAME = "dispatch-owner.json";
export const PROVIDER_RECORD_NAME = "dispatch-provider.json";
export const RUN_DIRECTORY_PREFIX = "mcp-";
export const RUN_ROOT_NAME = ".agent-run";
/** A week: long enough to read yesterday's evidence, short enough to bound growth. */
export const DEFAULT_RETENTION_HOURS = 168;
/** Long enough for a cooperative owner to publish evidence, short enough to end. */
const ESCALATION_MS = 3_000;
/** SIGKILL is asynchronous too; leave the durable record behind if it is not observed. */
const STOP_CONFIRMATION_MS = 100;

export interface OwnerRecord {
  schema_version: 1;
  kind: "dispatch" | "batch";
  run_dir: string;
  workspace: string;
  run_token: string;
  owner_pid: number;
  owner_pgid: number;
  owner_started_at: string | null;
  host_pid: number;
  host_started_at: string | null;
  started_at: string;
  owner_stdout: string;
  owner_stderr: string;
  task_id?: string;
  batch_id?: string;
}

export interface ProviderRecord {
  run_token: string;
  provider_pid: number;
  provider_pgid: number;
  provider_started_at: string | null;
}

export interface RecordedRun extends OwnerRecord {
  run_id: string;
  running: boolean;
  orphaned: boolean;
  provider: ProviderRecord | null;
}

export interface TerminationOutcome {
  run_dir: string;
  signalled: boolean;
  escalated: boolean;
  reason?: string;
}

/**
 * A pid alone is not an identity: the kernel recycles it. `ps -o lstart=` gives
 * a stable start timestamp for the same pid, so a record can be matched against
 * the process it was written for. Both macOS and procps support this field.
 */
export function processStartedAt(pid: number): string | null {
  if (!Number.isInteger(pid) || pid <= 1) return null;
  try {
    const output = execFileSync("/bin/ps", ["-o", "lstart=", "-p", String(pid)], {
      encoding: "utf8",
      timeout: 5_000,
      stdio: ["ignore", "pipe", "ignore"],
    });
    const value = output.trim();
    return value.length === 0 ? null : value;
  } catch {
    return null;
  }
}

/**
 * Is this pid still the process the record was written for? Fails closed: an
 * unverifiable pid is never signalled, because signalling a recycled pid is
 * worse than leaving one stray process for the next dispatch to find.
 */
export function processMatches(pid: number, startedAt: string | null): boolean {
  if (!Number.isInteger(pid) || pid <= 1) return false;
  if (startedAt === null) return false;
  try {
    process.kill(pid, 0);
  } catch {
    return false;
  }
  return processStartedAt(pid) === startedAt;
}

function readJson(path: string): Record<string, unknown> | undefined {
  try {
    const metadata = lstatSync(path);
    if (!metadata.isFile() || metadata.isSymbolicLink()) return undefined;
    const value: unknown = JSON.parse(readFileSync(path, "utf8"));
    return typeof value === "object" && value !== null && !Array.isArray(value)
      ? value as Record<string, unknown>
      : undefined;
  } catch {
    return undefined;
  }
}

function positiveInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value > 1;
}

export function readOwnerRecord(runDir: string): OwnerRecord | undefined {
  const record = readJson(join(runDir, OWNER_RECORD_NAME));
  if (record === undefined || record.schema_version !== 1) return undefined;
  if (!positiveInteger(record.owner_pid) || !positiveInteger(record.owner_pgid)) return undefined;
  if (record.kind !== "dispatch" && record.kind !== "batch") return undefined;
  return record as unknown as OwnerRecord;
}

export function readProviderRecord(runDir: string, runToken: string): ProviderRecord | null {
  const record = readJson(join(runDir, PROVIDER_RECORD_NAME));
  if (record === undefined || record.run_token !== runToken) return null;
  if (!positiveInteger(record.provider_pid) || !positiveInteger(record.provider_pgid)) return null;
  return {
    run_token: runToken,
    provider_pid: record.provider_pid,
    provider_pgid: record.provider_pgid,
    provider_started_at: typeof record.provider_started_at === "string" ? record.provider_started_at : null,
  };
}

export function writeOwnerRecord(record: OwnerRecord): void {
  writeFileSync(join(record.run_dir, OWNER_RECORD_NAME), JSON.stringify(record, null, 2) + "\n", {
    mode: 0o600,
  });
}

export function removeOwnerRecord(runDir: string): void {
  try {
    unlinkSync(join(runDir, OWNER_RECORD_NAME));
  } catch { /* An already-reaped run has no record to remove. */ }
}

export function runRoot(workspace: string): string {
  return join(workspace, RUN_ROOT_NAME);
}

function runDirectoryNames(workspace: string): string[] {
  try {
    return readdirSync(runRoot(workspace))
      .filter((name) => name.startsWith(RUN_DIRECTORY_PREFIX))
      .sort();
  } catch {
    return [];
  }
}

/**
 * Every recorded run under this workspace, with the two facts a caller acts on:
 * whether the owner is still alive, and whether the host that started it is
 * gone. An orphan is both.
 */
export function listRecordedRuns(workspace: string): RecordedRun[] {
  const root = runRoot(workspace);
  const runs: RecordedRun[] = [];
  for (const name of runDirectoryNames(workspace)) {
    const runDir = join(root, name);
    const record = readOwnerRecord(runDir);
    if (record === undefined) continue;
    const running = processMatches(record.owner_pid, record.owner_started_at);
    const provider = readProviderRecord(runDir, record.run_token);
    const providerRunning = provider !== null &&
      processMatches(provider.provider_pid, provider.provider_started_at);
    const hostAlive = processMatches(record.host_pid, record.host_started_at);
    runs.push({
      ...record,
      run_id: name,
      running,
      orphaned: !hostAlive && (running || providerRunning),
      provider,
    });
  }
  return runs;
}

export function findRecordedRun(workspace: string, reference: string): RecordedRun | undefined {
  const runs = listRecordedRuns(workspace);
  return runs.find((run) => run.run_id === reference) ??
    runs.find((run) => run.run_dir === reference);
}

/**
 * Signal a whole process group, never a bare recorded pid. The group leader is
 * verified first, so a recycled pid is left alone, and this process's own group
 * is never a target.
 */
export function signalRunGroup(
  pid: number,
  pgid: number,
  startedAt: string | null,
  signal: NodeJS.Signals,
): boolean {
  if (!processMatches(pid, startedAt)) return false;
  if (!positiveInteger(pgid) || pgid === process.pid) return false;
  try {
    process.kill(-pgid, signal);
    return true;
  } catch {
    // A group that has already collapsed to its leader still answers directly.
    try {
      process.kill(pid, signal);
      return true;
    } catch {
      return false;
    }
  }
}

/** Both groups a run can hold: the owner's, and the provider's own session. */
export function signalRecordedRun(run: RecordedRun, signal: NodeJS.Signals): boolean {
  const owner = signalRunGroup(run.owner_pid, run.owner_pgid, run.owner_started_at, signal);
  const provider = run.provider === null
    ? false
    : signalRunGroup(
      run.provider.provider_pid,
      run.provider.provider_pgid,
      run.provider.provider_started_at,
      signal,
    );
  return owner || provider;
}

function runStillAlive(run: RecordedRun): boolean {
  if (processMatches(run.owner_pid, run.owner_started_at)) return true;
  return run.provider !== null &&
    processMatches(run.provider.provider_pid, run.provider.provider_started_at);
}

async function waitForRunStop(run: RecordedRun, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (runStillAlive(run) && Date.now() < deadline) {
    await new Promise((resolveWait) => setTimeout(resolveWait, 100));
  }
  return !runStillAlive(run);
}

/**
 * Graceful first, then final. The owner publishes its evidence on SIGTERM; a
 * group that ignores it is killed rather than left behind.
 */
export async function terminateRecordedRun(
  run: RecordedRun,
  escalationMs = ESCALATION_MS,
): Promise<TerminationOutcome> {
  if (!runStillAlive(run)) {
    removeOwnerRecord(run.run_dir);
    return { run_dir: run.run_dir, signalled: false, escalated: false, reason: "not running" };
  }
  const signalled = signalRecordedRun(run, "SIGTERM");
  await waitForRunStop(run, escalationMs);
  let escalated = false;
  if (runStillAlive(run)) {
    escalated = signalRecordedRun(run, "SIGKILL");
    await waitForRunStop(run, STOP_CONFIRMATION_MS);
  }
  if (runStillAlive(run)) {
    return { run_dir: run.run_dir, signalled, escalated, reason: "still running" };
  }
  removeOwnerRecord(run.run_dir);
  return { run_dir: run.run_dir, signalled, escalated };
}

/**
 * The reaper, on the dispatch path rather than in a daemon: any run whose host
 * is gone but whose processes are not is stopped through the bounded termination
 * owner. Its record remains until that owner observes the processes have stopped.
 */
export async function reapOrphanedRuns(workspace: string): Promise<TerminationOutcome[]> {
  const orphans = listRecordedRuns(workspace).filter((run) => run.orphaned);
  return await Promise.all(orphans.map(async (run) => {
    const outcome = await terminateRecordedRun(run);
    return outcome.reason === "still running" ? outcome : { ...outcome, reason: "host gone" };
  }));
}

/**
 * How long a finished MCP dispatch run is kept, in hours.
 *
 * A malformed setting must not fail a dispatch, and silently keeping
 * everything would restore the unbounded growth this exists to stop, so an
 * unusable value falls back to the default rather than to no pruning at all.
 */
export function retentionHours(env: NodeJS.ProcessEnv): number {
  const configured = env.AGENT_FABRIC_RUN_RETENTION_HOURS;
  if (configured === undefined) return DEFAULT_RETENTION_HOURS;
  const hours = Number(configured);
  return Number.isFinite(hours) && hours >= 0 ? hours : DEFAULT_RETENTION_HOURS;
}

/**
 * The owner logs and staging inputs written as siblings of a run directory,
 * which any prune that only walks run directories would leave behind.
 */
function siblingPaths(root: string, name: string): string[] {
  try {
    return readdirSync(root)
      .filter((entry) => entry !== name && entry.startsWith(`${name}-`))
      .map((entry) => join(root, entry));
  } catch {
    return [];
  }
}

function newestMtimeMs(paths: string[]): number {
  let newest = 0;
  for (const path of paths) {
    try {
      newest = Math.max(newest, statSync(path).mtimeMs);
    } catch { /* A path that has vanished cannot hold a run open. */ }
  }
  return newest;
}

/**
 * Age out MCP dispatch run directories and the owner logs beside them.
 *
 * Only `mcp-` directories are touched: run directories from other
 * orchestration paths share this root and are not this front door's to delete.
 * Liveness decides before age does, so a run still in flight is never pruned
 * however old its directory looks. That liveness comes from the owner record
 * and its provider record, not a second mechanism invented here.
 */
export function pruneDispatchRuns(workspace: string, env: NodeJS.ProcessEnv): string[] {
  const cutoff = Date.now() - retentionHours(env) * 3_600_000;
  const root = runRoot(workspace);
  const pruned: string[] = [];
  for (const name of runDirectoryNames(workspace)) {
    const runDir = join(root, name);
    try {
      const metadata = lstatSync(runDir);
      if (!metadata.isDirectory() || metadata.isSymbolicLink()) continue;
      const record = readOwnerRecord(runDir);
      if (record !== undefined) {
        const provider = readProviderRecord(runDir, record.run_token);
        if (
          processMatches(record.owner_pid, record.owner_started_at) ||
          processMatches(record.host_pid, record.host_started_at) ||
          (provider !== null && processMatches(provider.provider_pid, provider.provider_started_at))
        ) continue;
      }
      const siblings = siblingPaths(root, name);
      if (newestMtimeMs([runDir, ...siblings]) > cutoff) continue;
      rmSync(runDir, { recursive: true, force: true });
      for (const sibling of siblings) rmSync(sibling, { recursive: true, force: true });
      pruned.push(runDir);
    } catch { /* A run another process is already removing is already pruned. */ }
  }
  return pruned;
}

function observedAlive(pid: number, startedAt: string | null): boolean {
  if (startedAt !== null) return processMatches(pid, startedAt);
  // A read-only observation can report a live PID when ps is unavailable;
  // signalling still requires the verified identity in processMatches.
  if (!positiveInteger(pid)) return false;
  try { process.kill(pid, 0); return true; } catch { return false; }
}

/** Status observes retained files and process identities; it never repairs or reaps runs. */
export async function fabricStatus(workspace: string, id?: string, waitSeconds = 0,
  signal?: AbortSignal): Promise<Record<string, unknown>> {
  if (!Number.isInteger(waitSeconds) || waitSeconds < 0 || waitSeconds > 55) {
    return { status: "rejected", error: "wait_invalid", fix: "Pass wait_seconds from 0 to 55." };
  }
  try { workspace = realpathSync(workspace); } catch { workspace = resolve(workspace); }
  const deadline = Date.now() + waitSeconds * 1000;
  while (true) {
    signal?.throwIfAborted();
    const candidates = runDirectoryNames(workspace).flatMap((name) => {
      const runDir = join(runRoot(workspace), name);
      try {
        const metadata = lstatSync(runDir);
        if (!metadata.isDirectory() || metadata.isSymbolicLink()) return [];
        const status = readJson(join(runDir, "dispatch-status.json"));
        const owner = readOwnerRecord(runDir);
        const started = Date.parse(String(status?.started_at ?? owner?.started_at ?? "")) || metadata.birthtimeMs;
        return [{ runDir, status, owner, started }];
      } catch { return []; }
    }).sort((a, b) => b.started - a.started);
    const matches = id === undefined
      ? candidates.filter((run) => run.started >= Date.now() - 86_400_000).slice(0, 20)
      : candidates.filter((run) => [run.runDir, basename(run.runDir), run.status?.id, run.owner?.task_id, run.owner?.batch_id].includes(id)
        || (Array.isArray(run.status?.task_ids) && run.status.task_ids.includes(id))
        || (/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/u.test(id) && readJson(join(run.runDir, "dispatch", "tasks", id, "attempt-001", "attempt.json"))?.task_id === id));
    if (id !== undefined && matches.length !== 1) {
      return { status: "rejected", error: matches.length === 0 ? "run_not_found" : "run_id_ambiguous",
        fix: "Pass the run_dir returned by fabric_dispatch or fabric_batch." };
    }
    const rows = matches.map(({ runDir, status, owner, started }) => {
      const safePath = (value: unknown): string | undefined => {
        if (typeof value !== "string") return undefined;
        const path = resolve(runDir, value);
        const rel = relative(runDir, path);
        return !isAbsolute(rel) && rel !== ".." && !rel.startsWith(`..${sep}`) ? path : undefined;
      };
      const attempts: Record<string, unknown>[] = [];
      const outputPaths: string[] = [];
      try {
        const tasksRoot = join(runDir, "dispatch", "tasks");
        if (lstatSync(tasksRoot).isSymbolicLink()) throw new Error("linked tasks");
        for (const task of readdirSync(tasksRoot).slice(0, 64)) {
          const taskDir = join(tasksRoot, task);
          if (lstatSync(taskDir).isSymbolicLink()) continue;
          for (const attempt of readdirSync(taskDir).filter((name) => /^attempt-\d+$/u.test(name)).sort().slice(-1)) {
            const attemptDir = join(taskDir, attempt);
            if (lstatSync(attemptDir).isSymbolicLink()) continue;
            const record = readJson(join(attemptDir, "attempt.json"));
            if (record !== undefined) attempts.push(record);
            for (const name of ["result.md", "stderr.log", "adapter-receipt.json"]) outputPaths.push(join(attemptDir, name));
          }
        }
      } catch { /* A new run may not have its first attempt yet. */ }
      const provider = owner === undefined ? null : readProviderRecord(runDir, owner.run_token);
      const alive = (owner !== undefined && observedAlive(owner.owner_pid, owner.owner_started_at)) ||
        (provider !== null && observedAlive(provider.provider_pid, provider.provider_started_at));
      const selectedIndex = id === undefined || !Array.isArray(status?.task_ids) ? -1 : status.task_ids.indexOf(id);
      const selectedTask = id !== undefined && id !== status?.id && id !== owner?.task_id && id !== owner?.batch_id
        ? attempts.find((attempt) => attempt.task_id === id) : undefined;
      const route = (selectedTask?.route ?? (selectedIndex >= 0 ? (status?.routes as unknown[] | undefined)?.[selectedIndex] : undefined) ?? status?.route ?? attempts[0]?.route ?? (status?.routes as unknown[] | undefined)?.[0] ?? {}) as Record<string, unknown>;
      const terminal = attempts.length > 0 && attempts.every((attempt) =>
        ["succeeded", "failed", "blocked", "timed_out", "cancelled"].includes(String(attempt.status)));
      let state = String(status?.status ?? "running");
      if (state === "running" && !alive) {
        state = terminal ? (attempts.length === 1 ? String(attempts[0]!.status) : "completed") : "interrupted";
      }
      if (alive) state = "running";
      if (selectedTask !== undefined) state = String(selectedTask.status);
      // Owner logs are siblings created by the front door, never arbitrary receipt paths.
      outputPaths.push(`${runDir}-owner.stdout.jsonl`, `${runDir}-owner.stderr.log`);
      const newest = Math.max(started, newestMtimeMs(outputPaths));
      const silence = Math.max(0, (Date.now() - newest) / 1000);
      const paths = status?.paths as Record<string, unknown> | undefined;
      const result = (selectedTask ?? attempts[0])?.result as Record<string, unknown> | undefined;
      const finished = Date.parse(String(selectedTask?.finished_at ?? status?.finished_at ?? attempts.at(-1)?.finished_at ?? ""));
      return { id: selectedTask?.task_id ?? (selectedIndex >= 0 ? id : undefined) ?? status?.id ?? owner?.task_id ?? owner?.batch_id ?? basename(runDir), run_dir: runDir,
        adapter: route.adapter ?? null, model: route.resolved_model ?? route.model ?? null,
        status: state, elapsed_seconds: Math.round(Math.max(0, ((state === "running" || !finished ? Date.now() : finished) - started) / 1000)),
        output_age_seconds: Math.round(silence), stalled: state === "running" && alive && silence > Math.max(600, Number(status?.timeout_seconds ?? 3600) * 0.2),
        result_path: safePath(selectedTask === undefined ? paths?.result ?? paths?.summary ?? result?.path : result?.path) ?? null };
    });
    if (id === undefined) return { runs: rows };
    const row = rows[0]!;
    if (row.status !== "running" || Date.now() >= deadline) return row;
    await new Promise((done) => setTimeout(done, Math.min(250, deadline - Date.now())));
  }
}
