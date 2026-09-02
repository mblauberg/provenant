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
import { lstatSync, readFileSync, readdirSync, unlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export const OWNER_RECORD_NAME = "dispatch-owner.json";
export const PROVIDER_RECORD_NAME = "dispatch-provider.json";
export const RUN_DIRECTORY_PREFIX = "mcp-";
export const RUN_ROOT_NAME = ".agent-run";
/** Long enough for a cooperative owner to publish evidence, short enough to end. */
const ESCALATION_MS = 3_000;

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
  const deadline = Date.now() + escalationMs;
  while (runStillAlive(run) && Date.now() < deadline) {
    await new Promise((resolveWait) => setTimeout(resolveWait, 100));
  }
  let escalated = false;
  if (runStillAlive(run)) {
    escalated = signalRecordedRun(run, "SIGKILL");
  }
  removeOwnerRecord(run.run_dir);
  return { run_dir: run.run_dir, signalled, escalated };
}

/**
 * The reaper, on the dispatch path rather than in a daemon: any run whose host
 * is gone but whose processes are not is signalled now, and its record cleared.
 */
export function reapOrphanedRuns(workspace: string): TerminationOutcome[] {
  const reaped: TerminationOutcome[] = [];
  for (const run of listRecordedRuns(workspace)) {
    if (!run.orphaned) continue;
    const signalled = signalRecordedRun(run, "SIGTERM");
    removeOwnerRecord(run.run_dir);
    reaped.push({ run_dir: run.run_dir, signalled, escalated: false, reason: "host gone" });
  }
  return reaped;
}
