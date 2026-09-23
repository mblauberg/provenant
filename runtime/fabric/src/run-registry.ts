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
import { runRoot, withoutGitRedirects } from "./identity.js";
import { canonicalSuccessStatus, isSuccessStatus } from "./success-status.js";
export { runRoot } from "./identity.js";
import { execFile } from "node:child_process";
import { psOutput } from "./ps.mjs";
import {
  existsSync,
  renameSync,
  openSync,
  closeSync,
  readSync,
  fstatSync,
  constants,
  lstatSync,
  readFileSync,
  realpathSync,
  readdirSync,
  rmSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { promisify } from "node:util";
const execFileAsync = promisify(execFile);
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";

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

/** `ps` start time distinguishes a live process from a recycled PID. */
function readProcessStartedAt(pid: number, canonical: boolean): string | null {
  if (!Number.isInteger(pid) || pid <= 1) return null;
  try {
    const output = psOutput(["-o", "lstart=", "-p", String(pid)],
      canonical ? { ...process.env, LC_ALL: "C", LANG: "C" } : process.env);
    const value = output.trim();
    return /\d{4}$/u.test(value) ? value : null; // the shim prints ? for an unreadable pid: unknown, not a time
  } catch {
    return null;
  }
}

export function processStartedAt(pid: number): string | null { return readProcessStartedAt(pid, true); }

function startMatches(pid: number, startedAt: string, canonical: string | null): boolean {
  return canonical === startedAt || (canonical !== null && readProcessStartedAt(pid, false) === startedAt);
}

/** Refuse to signal a PID whose recorded start time cannot be verified. */
export function processMatches(pid: number, startedAt: string | null): boolean {
  if (!Number.isInteger(pid) || pid <= 1) return false;
  if (startedAt === null) return false;
  try {
    process.kill(pid, 0);
  } catch {
    return false;
  }
  const canonical = processStartedAt(pid);
  return startMatches(pid, startedAt, canonical);
}

function readJson(path: string): Record<string, unknown> | undefined {
  try {
    const metadata = lstatSync(path);
    if (!metadata.isFile() || metadata.isSymbolicLink()) return undefined;
    const value: unknown = JSON.parse(readFileSync(path, "utf8"));
    return typeof value === "object" && value !== null && !Array.isArray(value)
      ? (value as Record<string, unknown>)
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
  return { ...record, run_dir: runDir } as unknown as OwnerRecord;
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
  } catch {
    /* An already-reaped run has no record to remove. */
  }
}

function runDirectoryNames(workspace: string): string[] {
  const root = runRoot(workspace);
  let local = resolve(workspace, ".agent-run");
  try { local = join(realpathSync(workspace), ".agent-run"); } catch { /* Missing workspace. */ }
  const locations = [{ path: root, modern: true }, ...(local === root ? [] : [{ path: local, modern: false }])];
  const names: string[] = [];
  for (const location of locations) {
    try { if (lstatSync(location.path).isSymbolicLink()) continue; } catch { continue; }
    for (const sub of location.modern ? ["", "runs"] : [""]) {
      const directory = join(location.path, sub);
      try {
        if (lstatSync(directory).isSymbolicLink()) continue;
        for (const entry of readdirSync(directory, { withFileTypes: true })) {
          if (!entry.isDirectory() || entry.isSymbolicLink()) continue;
          if (sub === "" ? entry.name.startsWith("mcp-") : /^\d{8}-\d{4}-(dispatch|batch)-/u.test(entry.name))
            names.push(relative(root, join(directory, entry.name)));
        }
      } catch { /* Root may not exist yet. */ }
    }
  }
  return names.sort();
}

export function shortRunId(runDir: string): string {
  const name = basename(runDir);
  return name.startsWith("mcp-") ? name : `mcp-${name.slice(-6)}`;
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
    const providerRunning = provider !== null && processMatches(provider.provider_pid, provider.provider_started_at);
    const hostAlive = record.host_started_at !== null && observedAlive(record.host_pid, record.host_started_at); // a failed probe is not death
    runs.push({
      ...record,
      run_id: shortRunId(runDir),
      running,
      orphaned: !hostAlive && (running || providerRunning),
      provider,
    });
  }
  return runs;
}

export function findRecordedRun(workspace: string, reference: string): RecordedRun | undefined {
  const runs = listRecordedRuns(workspace);
  const real = (path: string) => { try { return realpathSync(path); } catch { return resolve(path); } }; // macOS /var
  return runs.find((run) => run.run_id === reference) ?? runs.find((run) => real(run.run_dir) === real(reference));
}

/**
 * Signal a whole process group, never a bare recorded pid. The group leader is
 * verified first, so a recycled pid is left alone, and this process's own group
 * is never a target.
 */
export function signalRunGroup(pid: number, pgid: number, startedAt: string | null, signal: NodeJS.Signals): boolean {
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
  const provider =
    run.provider === null
      ? false
      : signalRunGroup(run.provider.provider_pid, run.provider.provider_pgid, run.provider.provider_started_at, signal);
  return owner || provider;
}

function runStillAlive(run: RecordedRun): boolean {
  if (observedAlive(run.owner_pid, run.owner_started_at)) return true;
  return run.provider !== null && observedAlive(run.provider.provider_pid, run.provider.provider_started_at);
}

async function waitForRunStop(run: RecordedRun, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (runStillAlive(run) && Date.now() < deadline) {
    await new Promise((resolveWait) => setTimeout(resolveWait, 100));
  }
  return !runStillAlive(run);
}

/** Preserve closure even when SIGKILL prevented an attempt receipt. */
function closeStoppedRun(runDir: string, terminalStatus: "interrupted" | "cancelled" = "interrupted"): void {
  const path = join(runDir, "dispatch-status.json");
  const status = readJson(path) ?? {};
  const receipt = readJson(join(runDir, "RUN_RECEIPT.json"));
  if ((!status.finished_at || status.status === "running") &&
      (receipt?.status === undefined || receipt.status === "active")) {
    const [temporary, { fix: _staleFix, ...prior }] = [`${path}.${process.pid}.tmp`, status];
    try {
      writeFileSync(temporary, JSON.stringify({ ...prior, status: terminalStatus,
        finished_at: new Date().toISOString(), ...(terminalStatus === "interrupted"
          ? { fix: "Dispatch a new run; the owner exited." } : {}) }) + "\n", { mode: 0o600 });
      renameSync(temporary, path);
    } catch (error) { // A pruned run has nothing to close; else keep the owner record for status.
      if ((error as NodeJS.ErrnoException).code !== "ENOENT")
        return void console.error(`fabric: could not close ${runDir}: ${(error as Error).message}`);
    }
  }
  removeOwnerRecord(runDir);
}

/**
 * Graceful first, then final. The owner publishes its evidence on SIGTERM; a
 * group that ignores it is killed rather than left behind.
 */
export async function terminateRecordedRun(
  run: RecordedRun,
  escalationMs = ESCALATION_MS,
  terminalStatus: "interrupted" | "cancelled" = "interrupted",
): Promise<TerminationOutcome> {
  if (!runStillAlive(run)) {
    closeStoppedRun(run.run_dir, terminalStatus);
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
  closeStoppedRun(run.run_dir, terminalStatus);
  return { run_dir: run.run_dir, signalled, escalated };
}

/**
 * The reaper, on the dispatch path rather than in a daemon: any run whose host
 * is gone but whose processes are not is stopped through the bounded termination
 * owner. Its record remains until that owner observes the processes have stopped.
 */
export async function reapOrphanedRuns(workspace: string): Promise<TerminationOutcome[]> {
  for (const name of runDirectoryNames(workspace)) {
    const dir = join(runRoot(workspace), name),
      path = join(dir, "RUN_RECEIPT.json");
    const receipt = readJson(path);
    if (receipt?.status !== "active" || statSync(path).mtimeMs > Date.now() - 48 * 3600000) continue;
    const owner = readOwnerRecord(dir);
    const provider = owner && readProviderRecord(dir, owner.run_token);
    if (
      (owner && observedAlive(owner.owner_pid, owner.owner_started_at)) ||
      (provider && observedAlive(provider.provider_pid, provider.provider_started_at)) ||
      (typeof receipt.pid === "number" && observedAlive(receipt.pid, null))
    )
      continue;
    // Write atomically and only after observing the receipt unchanged.
    const before = readFileSync(path, "utf8");
    if (JSON.parse(before).status !== "active") continue;
    const tmp = `${path}.${process.pid}.tmp`;
    writeFileSync(
      tmp,
      JSON.stringify({
        ...receipt,
        status: "interrupted",
        ended_at: new Date().toISOString(),
        fix: "Dispatch a new run; the owner exited.",
      }) + "\n",
      { mode: 0o600 },
    );
    if (readFileSync(path, "utf8") === before) renameSync(tmp, path);
    else unlinkSync(tmp);
  }
  const orphans = listRecordedRuns(workspace).filter((run) => run.orphaned);
  return await Promise.all(
    orphans.map(async (run) => {
      const outcome = await terminateRecordedRun(run);
      return outcome.reason === "still running" ? outcome : { ...outcome, reason: "host gone" };
    }),
  );
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
    } catch {
      /* A path that has vanished cannot hold a run open. */
    }
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
      if (existsSync(join(runDir, "KEEP")) || existsSync(join(runDir, "RUN.json"))) continue;
      const receipt = readJson(join(runDir, "RUN_RECEIPT.json"));
      if (receipt?.status === "active") continue;
      if (receipt?.status === "input_required" || receipt?.resumable === true) continue;
      const successful = isSuccessStatus(receipt?.status) || receipt?.status === "cancelled";
      const failed = [
        "failed",
        "partial",
        "usage_limited",
        "rate_limited",
        "auth_required",
        "model_unavailable",
        "permission_blocked",
        "stalled",
        "timed_out",
        "interrupted",
        "rejected",
        "tool_missing",
      ].includes(String(receipt?.status));
      if (name.startsWith("runs/") && !successful && !failed) continue;
      const runCutoff =
        failed && env.AGENT_FABRIC_RUN_RETENTION_HOURS === undefined ? Date.now() - 14 * 86400000 : cutoff;
      const record = readOwnerRecord(runDir);
      if (record !== undefined) {
        const provider = readProviderRecord(runDir, record.run_token);
        if (
          observedAlive(record.owner_pid, record.owner_started_at) ||
          observedAlive(record.host_pid, record.host_started_at) ||
          (provider !== null && observedAlive(provider.provider_pid, provider.provider_started_at))
        )
          continue;
      }
      const siblings = name.startsWith("runs/") ? [] : siblingPaths(dirname(runDir), basename(runDir));
      if (newestMtimeMs([runDir, join(runDir, "RUN_RECEIPT.json"), ...siblings]) > runCutoff) continue;
      rmSync(runDir, { recursive: true, force: true });
      for (const sibling of siblings) rmSync(sibling, { recursive: true, force: true });
      pruned.push(runDir);
    } catch {
      /* A run another process is already removing is already pruned. */
    }
  }
  return pruned;
}

function observedAlive(pid: number, startedAt: string | null): boolean {
  if (!positiveInteger(pid)) return false;
  try {
    process.kill(pid, 0);
  } catch {
    return false;
  }
  if (startedAt === null) return true;
  const observed = processStartedAt(pid);
  // An unavailable process identity is not evidence of death. Signalling stays strict.
  return observed === null || startMatches(pid, startedAt, observed);
}

/** Status observes retained files and process identities; it never repairs or reaps runs. */
async function legacyStatus(
  workspace: string,
  id?: string,
  waitSeconds = 0,
  signal?: AbortSignal,
): Promise<Record<string, unknown>> {
  if (!Number.isInteger(waitSeconds) || waitSeconds < 0 || waitSeconds > 55) {
    return { status: "rejected", error: "wait_invalid", fix: "Pass wait_seconds from 0 to 55." };
  }
  try {
    workspace = realpathSync(workspace);
  } catch {
    workspace = resolve(workspace);
  }
  const deadline = Date.now() + waitSeconds * 1000;
  while (true) {
    signal?.throwIfAborted();
    const candidates = runDirectoryNames(workspace)
      .flatMap((name) => {
        const runDir = join(runRoot(workspace), name);
        try {
          const metadata = lstatSync(runDir);
          if (!metadata.isDirectory() || metadata.isSymbolicLink()) return [];
          const status = readJson(join(runDir, "dispatch-status.json"));
          const owner = readOwnerRecord(runDir);
          const started = Date.parse(String(status?.started_at ?? owner?.started_at ?? "")) || metadata.birthtimeMs;
          return [{ runDir, status, owner, started, created: metadata.birthtimeMs }];
        } catch {
          return [];
        }
      })
      .sort((a, b) => b.started - a.started || b.created - a.created);
    const exact = candidates.find(
      (run) => run.runDir === id || basename(run.runDir) === id || shortRunId(run.runDir) === id,
    );
    const matches =
      id === undefined
        ? candidates
            .filter((run) => run.status?.status === "running" || run.started >= Date.now() - 86_400_000)
            .slice(0, 20)
        : exact !== undefined
          ? [exact]
          : candidates.filter(
              (run) =>
                [
                  run.status?.id,
                  run.status?.task_id,
                  run.status?.batch_id,
                  run.owner?.task_id,
                  run.owner?.batch_id,
                ].includes(id) ||
                (Array.isArray(run.status?.task_ids) && run.status.task_ids.includes(id)) ||
                (/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/u.test(id) &&
                  readJson(join(run.runDir, "dispatch", "tasks", id, "attempt-001", "attempt.json"))?.task_id === id),
            );
    if (id !== undefined && matches.length === 0) {
      return { status: "rejected", error: "run_not_found", fix: "Pass the id from the dispatch response." };
    }
    const note =
      id !== undefined && matches.length > 1
        ? `Multiple runs match; showing the newest (${basename(matches[0]!.runDir)}).`
        : undefined;
    const rows = (id === undefined ? matches : matches.slice(0, 1)).map(
      ({ runDir, status: initialStatus, owner, started }) => {
        const provider = owner === undefined ? null : readProviderRecord(runDir, owner.run_token);
        const alive =
          (owner !== undefined && observedAlive(owner.owner_pid, owner.owner_started_at)) ||
          (provider !== null && observedAlive(provider.provider_pid, provider.provider_started_at));
        // Once the owner is dead its attempt files are final. Read them after the probe.
        const status = readJson(join(runDir, "dispatch-status.json")) ?? initialStatus;
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
            for (const attempt of readdirSync(taskDir)
              .filter((name) => /^attempt-\d+$/u.test(name))
              .sort()
              .slice(-1)) {
              const attemptDir = join(taskDir, attempt);
              if (lstatSync(attemptDir).isSymbolicLink()) continue;
              const record = readJson(join(attemptDir, "attempt.json"));
              if (record !== undefined) attempts.push(record);
              for (const name of ["result.md", "stderr.log", "adapter-receipt.json"])
                outputPaths.push(join(attemptDir, name));
              const scratch = readJson(join(attemptDir, "provider-output.json"))?.directory;
              if (
                typeof scratch === "string" &&
                isAbsolute(scratch) &&
                basename(scratch).startsWith("fabric-provider-")
              ) {
                try {
                  if (lstatSync(scratch).isSymbolicLink()) continue;
                  for (const directory of readdirSync(scratch)
                    .filter((name) => name.startsWith("cf-dispatch-run."))
                    .slice(0, 32)) {
                    const path = join(scratch, directory);
                    if (lstatSync(path).isSymbolicLink()) continue;
                    for (const name of ["raw", "diag", "combined", "clean"]) outputPaths.push(join(path, name));
                  }
                } catch {
                  /* Owners remove temporary output after retaining the result. */
                }
              }
            }
          }
        } catch {
          /* A new run may not have its first attempt yet. */
        }
        const selectedIndex = id === undefined || !Array.isArray(status?.task_ids) ? -1 : status.task_ids.indexOf(id);
        const selectedTask =
          id !== undefined &&
          id !== status?.id &&
          id !== status?.task_id &&
          id !== status?.batch_id &&
          id !== owner?.task_id &&
          id !== owner?.batch_id
            ? attempts.find((attempt) => attempt.task_id === id)
            : undefined;
        const batchId = owner?.batch_id ?? status?.batch_id ?? (status?.kind === "batch" ? status.id : undefined);
        const summaryPath =
          typeof batchId === "string" && /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/u.test(batchId)
            ? join(runDir, "dispatch", "batches", batchId, "summary.json")
            : undefined;
        const summary = summaryPath === undefined ? undefined : readJson(summaryPath);
        const route = (selectedTask?.route ??
          (selectedIndex >= 0 ? (status?.routes as unknown[] | undefined)?.[selectedIndex] : undefined) ??
          status?.route ??
          attempts[0]?.route ??
          (status?.routes as unknown[] | undefined)?.[0] ??
          {}) as Record<string, unknown>;
        const terminal =
          attempts.length > 0 &&
          attempts.every((attempt) =>
            ["ok", "failed", "blocked", "timed_out", "cancelled"].includes(String(canonicalSuccessStatus(attempt.status))),
          ) &&
          (batchId === undefined ||
            (Array.isArray(status?.task_ids) &&
              status.task_ids.every((taskId) => attempts.some((attempt) => attempt.task_id === taskId))));
        let state = String(status?.status ?? "running");
        if (state === "running" && !alive) {
          state = ["completed", "failed", "cancelled"].includes(String(summary?.status))
            ? String(summary!.status)
            : terminal
              ? batchId === undefined
                ? String(attempts[0]!.status)
                : "completed"
              : "interrupted";
        }
        if (alive) state = "running";
        if (selectedTask !== undefined) state = String(selectedTask.status);
        state = String(canonicalSuccessStatus(state));
        // Owner logs are siblings created by the front door, never arbitrary receipt paths.
        outputPaths.push(`${runDir}-owner.stdout.jsonl`, `${runDir}-owner.stderr.log`);
        const newest = Math.max(started, newestMtimeMs(outputPaths));
        const silence = Math.max(0, (Date.now() - newest) / 1000);
        const paths = status?.paths as Record<string, unknown> | undefined;
        const result = (selectedTask ?? attempts[0])?.result as Record<string, unknown> | undefined;
        const finished = Date.parse(
          String(
            selectedTask?.finished_at ??
              status?.finished_at ??
              summary?.finished_at ??
              attempts.at(-1)?.finished_at ??
              "",
          ),
        );
        return {
          id:
            selectedTask?.task_id ??
            (selectedIndex >= 0 ? id : undefined) ??
            status?.id ??
            owner?.task_id ??
            owner?.batch_id ??
            basename(runDir),
          run_dir: runDir,
          adapter: route.adapter ?? null,
          model: route.resolved_model ?? route.model ?? null,
          status: state,
          elapsed_seconds: Math.round(
            Math.max(0, ((state === "running" || !finished ? Date.now() : finished) - started) / 1000),
          ),
          output_age_seconds: Math.round(silence),
          stalled:
            state === "running" && alive && silence > Math.max(600, Number(status?.timeout_seconds ?? 3600) * 0.2),
          result_path:
            safePath(
              selectedTask === undefined
                ? (paths?.result ?? paths?.summary ?? (summary === undefined ? result?.path : summaryPath))
                : result?.path,
            ) ?? null,
        };
      },
    );
    if (id === undefined) return { runs: rows };
    const row = { ...rows[0]!, ...(note === undefined ? {} : { note }) };
    if (row.status !== "running" || Date.now() >= deadline) return row;
    await new Promise((done) => setTimeout(done, Math.min(250, deadline - Date.now())));
  }
}
export interface StatusResult extends Record<string, unknown> {
  runs?: Record<string, any>[];
}
async function ledger(worktree: unknown): Promise<Record<string, unknown>> {
  const empty = { worktree: worktree ?? null, branch_tip: null, dirty: null, ahead: null };
  if (typeof worktree !== "string") return empty;
  try {
    const git = async (...args: string[]) =>
      (await execFileAsync("git", ["-C", worktree, ...args], {
        env: withoutGitRedirects(process.env),
        encoding: "utf8",
        timeout: 1000,
      })).stdout.trim();
    const branch_tip = await git("rev-parse", "HEAD");
    const status = await git("status", "--porcelain=v2", "--branch", "--untracked-files=normal");
    const ahead = status.match(/^# branch\.ab \+(\d+)/mu)?.[1];
    return {
      worktree,
      branch_tip,
      dirty: status.split("\n").some((line) => line && !line.startsWith("#")),
      ahead: ahead === undefined ? null : Number(ahead),
    };
  } catch {
    return empty;
  }
}
function v1Rows(runDir: string): Record<string, any>[] {
  const grouped = new Map<string, Record<string, any>[]>();
  for (const tree of ["tasks", "dispatch/tasks"]) {
    const tasks = join(runDir, tree);
    try {
      if (lstatSync(tasks).isSymbolicLink()) continue;
      for (const task of readdirSync(tasks, { withFileTypes: true })
        .filter((d) => d.isDirectory())
        .slice(0, 64)) {
        const dir = join(tasks, task.name);
        for (const attempt of readdirSync(dir, { withFileTypes: true }).filter(
          (d) => d.isDirectory() && /^attempt-\d+$/u.test(d.name),
        )) {
          const row = readJson(join(dir, attempt.name, "attempt.json"));
          if (row?.schema !== "fabric.attempt.v1") continue;
          const rows = grouped.get(task.name) ?? [];
          rows.push(row);
          grouped.set(task.name, rows);
        }
      }
    } catch {
      /* An owner may still be creating its task directory. */
    }
  }
  if (grouped.size === 0) {
    const receipt = readJson(join(runDir, "RUN_RECEIPT.json"));
    const rows = Array.isArray(receipt?.attempts) ? receipt.attempts : [];
    for (const row of rows)
      if (row?.schema === "fabric.attempt.v1") {
        const values = grouped.get(String(row.task_id)) ?? [];
        values.push(row);
        grouped.set(String(row.task_id), values);
      }
  }
  if (grouped.size === 0) return [];
  const metadata = readJson(join(runDir, "dispatch-status.json"));
  const receipt = readJson(join(runDir, "RUN_RECEIPT.json"));
  const owner = readOwnerRecord(runDir);
  const provider = owner && readProviderRecord(runDir, owner.run_token);
  const dead =
    owner !== undefined &&
    !observedAlive(owner.owner_pid, owner.owner_started_at) &&
    !(provider && observedAlive(provider.provider_pid, provider.provider_started_at));
  const closed = metadata?.finished_at !== undefined && metadata?.status !== "running";
  const ownerAlive = owner !== undefined && observedAlive(owner.owner_pid, owner.owner_started_at);
  const interrupted = !ownerAlive && (receipt?.status === "interrupted" || dead || closed);
  const closureStatus = receipt?.status === "cancelled" || metadata?.status === "cancelled" ? "cancelled" : "interrupted";
  const summary =
    typeof metadata?.batch_id === "string" && /^[A-Za-z0-9._-]+$/u.test(metadata.batch_id)
      ? readJson(join(runDir, "dispatch/batches", metadata.batch_id, "summary.json"))
      : undefined;
  const rows: Record<string, any>[] = [...grouped.values()].map((attempts) => {
    const row = attempts.sort((a, b) => Number(a.attempt) - Number(b.attempt)).at(-1)!;
    const routeIndex = Array.isArray(metadata?.task_ids) ? metadata.task_ids.indexOf(row.task_id) : 0;
    const route = Array.isArray(metadata?.routes) ? metadata.routes[routeIndex] : undefined;
    const notes = Array.isArray(route?.notes) ? route.notes.filter((note: unknown) => typeof note === "string") : [];
    const pending = metadata?.task_id === row.task_id && Number(metadata?.next_attempt ?? 0) > Number(row.attempt);
    const effective =
      interrupted && row.state !== "terminal"
        ? {
            ...row,
            state: "terminal",
            status: closureStatus,
            digest: `${closureStatus} ${row.run_id}${closureStatus === "interrupted" ? " · fix: dispatch a new run" : ""}`,
          }
        : row;
    if (pending) {
      const status = closed && metadata?.status === "rejected" ? "rejected" : interrupted ? closureStatus : null;
      const fix = metadata?.fix ?? metadata?.message ?? "Dispatch a new run; the owner exited.";
      return {
        schema: "fabric.status.v1",
        id: row.run_id,
        run_id: row.run_id,
        run_dir: runDir,
        task_id: row.task_id,
        attempt: metadata!.next_attempt,
        attempts: attempts.map((attempt) => ({ ...attempt, status: canonicalSuccessStatus(attempt.status) })),
        attempt_count: attempts.length,
        state: interrupted ? "terminal" : "queued",
        status,
        ...(interrupted ? { fix, message: metadata?.message } : {}),
        started_at: metadata!.started_at,
        digest: interrupted ? `${status} ${row.run_id} · fix: ${fix}` : `running ${row.run_id} attempt ${metadata!.next_attempt} · fabric_status{ids:["${row.run_id}"],wait_seconds:55}`,
        paths: {},
      };
    }
    return {
      ...effective,
      status: canonicalSuccessStatus(effective.status),
      ...(notes.length ? { notes } : {}),
      schema: "fabric.status.v1",
      id: row.run_id,
      run_dir: runDir,
      attempts: attempts.map((attempt) => ({ ...attempt, status: canonicalSuccessStatus(attempt.status) })),
      attempt_count: attempts.length,
      ...(metadata?.batch_id ? { batch_id: metadata.batch_id } : {}),
      result_path: typeof row.paths?.result === "string" ? resolve(runDir, row.paths.result) : null,
    };
  });
  // A batch cannot finish before every manifest task has a row.
  if (rows.length && Array.isArray(metadata?.task_ids))
    for (const id of metadata.task_ids) {
      if (rows.some((row) => row.task_id === id)) continue;
      const terminalTask = Array.isArray(summary?.tasks)
        ? summary.tasks.find((task) => task.task_id === id)
        : undefined;
      const terminal = interrupted || (summary?.status !== undefined && summary.status !== "running");
      rows.push({
        schema: "fabric.status.v1",
        id: metadata.id,
        run_id: metadata.id ?? shortRunId(runDir),
        run_dir: runDir,
        task_id: id,
        batch_id: metadata.batch_id,
        attempt: 0,
        attempts: [],
        attempt_count: 0,
        state: terminal ? "terminal" : "queued",
        status: terminal ? canonicalSuccessStatus(terminalTask?.status ?? "interrupted") : null,
        started_at: metadata.started_at,
        paths: {},
        digest: `${terminal ? canonicalSuccessStatus(terminalTask?.status ?? "interrupted") : "queued"} ${id}`,
      });
    }
  const order = Array.isArray(metadata?.task_ids) ? metadata.task_ids.map(String) : []; // manifest order, else task-2 before task-10
  const rank = (row: Record<string, any>) => { const at = order.indexOf(String(row.task_id)); return at < 0 ? order.length : at; };
  return rows.sort((a, b) => rank(a) - rank(b) || String(a.task_id).localeCompare(String(b.task_id), "en", { numeric: true }));
}

/** v1 is authoritative; the old reader remains isolated for wave-1 receipts. */
export async function statusRows(
  workspace: string,
  ids?: string[],
  waitSeconds = 0,
  until: "any" | "all" = "all",
  signal?: AbortSignal,
  detail: "brief" | "full" = "brief",
): Promise<StatusResult> {
  if (!Number.isInteger(waitSeconds) || waitSeconds < 0 || waitSeconds > 55)
    return { status: "rejected", error: "wait_invalid", fix: "Pass wait_seconds from 0 to 55." };
  const deadline = Date.now() + waitSeconds * 1000;
  for (;;) {
    signal?.throwIfAborted();
    const root = runRoot(workspace);
    const candidates = runDirectoryNames(workspace).map((name) => join(root, name));
    const v1 = candidates.flatMap(v1Rows);
    const v1Dirs = new Set(v1.map((row) => row.run_dir));
    const wrap = (row: Record<string, any>): Record<string, any> => {
      const metadata = readJson(join(row.run_dir, "dispatch-status.json"));
      const owner = readOwnerRecord(row.run_dir);
      return {
        ...row,
        schema: "fabric.status.v1",
        run_id: shortRunId(row.run_dir),
        task_id: metadata?.task_id ?? owner?.task_id,
        batch_id: metadata?.batch_id ?? owner?.batch_id,
        state: row.status === "running" ? "running" : "terminal",
        attempt: 1,
        attempts: [row],
        attempt_count: 1,
        started_at: metadata?.started_at ?? owner?.started_at ?? new Date(statSync(row.run_dir).mtimeMs).toISOString(),
        paths: metadata?.paths ?? { result: row.result_path },
        worktree: metadata?.worktree ?? null,
      };
    };
    let rows: Record<string, any>[] = [];
    if (ids?.length) {
      for (const id of ids) {
        const exact = v1.filter((row) => [row.run_id, row.run_dir, basename(row.run_dir)].includes(id));
        const matches = exact.length ? exact : v1.filter((row) => [row.batch_id, row.task_id, row.id].includes(id));
        if (matches.length) {
          const newest = matches.reduce((best, row) => (Date.parse(row.started_at) > Date.parse(best.started_at) ? row : best)); // tasks keep batch order
          rows.push(...matches.filter((row) => row.run_dir === newest.run_dir));
        } else {
          const legacy = await legacyStatus(workspace, id);
          if (legacy.error) return legacy;
          rows.push(wrap(legacy));
        }
      }
      rows = rows.filter(
        (row, index) =>
          rows.findIndex((other) => other.run_dir === row.run_dir && other.task_id === row.task_id) === index,
      );
    } else {
      const legacy = await legacyStatus(workspace);
      rows = [
        ...v1,
        ...((legacy.runs ?? []) as Record<string, any>[]).filter((row) => !v1Dirs.has(row.run_dir)).map(wrap),
      ]
        .filter((row) => row.state !== "terminal" || Date.parse(row.ended_at ?? row.started_at) > Date.now() - 86400000)
        .sort((a, b) => Date.parse(b.started_at) - Date.parse(a.started_at))
        .slice(0, 20);
    }
    const done =
      until === "any" ? rows.some((row) => row.state === "terminal") : rows.every((row) => row.state === "terminal");
    if (done || !rows.length || Date.now() >= deadline) {
      const cache = new Map<unknown, Promise<Record<string, unknown>>>();
      const enriched = await Promise.all(rows.map(async (row) => {
        if (row.state === "terminal" && detail !== "full") return row;
        if (!cache.has(row.worktree)) cache.set(row.worktree, ledger(row.worktree));
        return { ...row, ...await cache.get(row.worktree) };
      }));
      return { schema: "fabric.status.v1", runs: enriched };
    }
    await new Promise((done) => setTimeout(done, Math.min(100, deadline - Date.now())));
  }
}

export async function fabricStatus(
  workspace: string,
  id?: string,
  waitSeconds = 0,
  signal?: AbortSignal,
): Promise<Record<string, any>> {
  const result = await statusRows(workspace, id ? [id] : undefined, waitSeconds, "all", signal);
  if (id && result.runs?.length === 1) return result.runs[0]!;
  return result;
}

export async function fabricOutput(
  workspace: string,
  input: { id: string; part?: string; offset?: number; max_bytes?: number; tail?: boolean },
) {
  const result = await statusRows(workspace, [input.id]);
  if (!result.runs) return result;
  if (result.runs.length !== 1)
    return { status: "rejected", error: "output_ambiguous", fix: "Pass a task id for batch output." };
  const row = result.runs[0]!;
  const part = input.part ?? "result";
  const raw = row.paths?.[part] ?? (part === "result" ? row.result_path : undefined);
  if (typeof raw !== "string")
    return { status: "rejected", error: "output_unavailable", fix: `Wait for the owner to publish ${part}.` };
  let offset = input.offset ?? 0;
  const max = input.max_bytes ?? 4000;
  if (!Number.isSafeInteger(offset) || offset < 0 || !Number.isInteger(max) || max < 1 || max > 20000)
    return { status: "rejected", error: "output_bounds", fix: "Use offset >= 0 and max_bytes 1–20000." };
  let fd: number | undefined;
  try {
    const root = realpathSync(row.run_dir),
      path = realpathSync(resolve(root, raw));
    const rel = relative(root, path);
    if (isAbsolute(rel) || rel === ".." || rel.startsWith(`..${sep}`)) throw new Error("output escapes run directory");
    fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    if (!fstatSync(fd).isFile()) throw new Error("output is not a regular file");
    if (input.tail && input.offset !== undefined)
      return { status: "rejected", error: "output_bounds", fix: "Use tail or offset, not both." };
    const size = fstatSync(fd).size;
    if (input.tail) offset = Math.max(0, size - max);
    const bytes = Buffer.alloc(max + 3);
    const available = readSync(fd, bytes, 0, bytes.length, offset);
    const continuation = (byte: number) => (byte & 0xc0) === 0x80;
    let start = 0, count = Math.min(max, available);
    if (input.tail) while (start < count && continuation(bytes[start]!)) start++;
    else if (available && continuation(bytes[0]!))
      return { status: "rejected", error: "output_bounds", fix: "Use a UTF-8 boundary from next_offset." };
    if (count < available) while (count > start && continuation(bytes[count]!)) count--;
    if (count === start && available > start)
      return { status: "rejected", error: "output_bounds", fix: "Use max_bytes of at least 4 for UTF-8 text." };
    const text = bytes.subarray(start, count).toString("utf8");
    return {
      id: input.id,
      part,
      offset: offset + start,
      next_offset: offset + count,
      eof: offset + count >= fstatSync(fd).size,
      digest: text,
    };
  } catch (error) {
    return { status: "rejected", error: "output_unavailable", fix: `Read a retained output file: ${String(error)}` };
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}
