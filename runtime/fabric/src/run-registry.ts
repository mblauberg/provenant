/**
 * What survives an MCP restart about a dispatch that is still running.
 *
 * A dispatch owner is spawned detached, so it outlives the host that started
 * it. The only durable handle on it is what the host writes into the run
 * directory before walking away: the owner's pid, the process group it leads,
 * and enough identity to tell that pid apart from a recycled one. Everything
 * here reads and acts on that record, from any process, with no daemon.
 *
 * Controllers signal only identity-verified owner and provider PIDs. A live
 * owner handles its descendants. When an owner cannot finish cleanup, these
 * records do not authorise a broader group kill that could hit a shared host.
 */
import { runRoot } from "./identity.js";
import { isSuccessStatus } from "./success-status.js";
import { observedAlive, processMatches } from "./process-identity.js";
export { runRoot } from "./identity.js";
import {
  existsSync,
  renameSync,
  lstatSync,
  readFileSync,
  realpathSync,
  readdirSync,
  rmSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join, relative, resolve } from "node:path";

export const OWNER_RECORD_NAME = "dispatch-owner.json";
export const PROVIDER_RECORD_NAME = "dispatch-provider.json";
/** A week: long enough to read yesterday's evidence, short enough to bound growth. */
export const DEFAULT_RETENTION_HOURS = 168;

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

export function readJson(path: string): Record<string, unknown> | undefined {
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

export function positiveInteger(value: unknown): value is number {
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

/** Atomic, so a reader or a failed rewrite never leaves a live run without its record. */
export function writeOwnerRecord(record: OwnerRecord): void {
  const path = join(record.run_dir, OWNER_RECORD_NAME);
  const temporary = `${path}.${process.pid}.tmp`;
  writeFileSync(temporary, JSON.stringify(record, null, 2) + "\n", { mode: 0o600 });
  renameSync(temporary, path);
}

export function removeOwnerRecord(runDir: string): void {
  try {
    unlinkSync(join(runDir, OWNER_RECORD_NAME));
  } catch {
    /* An already-reaped run has no record to remove. */
  }
}

export function runDirectoryNames(workspace: string): string[] {
  const root = runRoot(workspace);
  const names: string[] = [];
  try { if (lstatSync(root).isSymbolicLink()) return names; } catch { return names; }
  const directory = join(root, "runs");
  try {
    if (lstatSync(directory).isSymbolicLink()) return names;
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      if (entry.isDirectory() && !entry.isSymbolicLink() && /^\d{8}-\d{4}-(dispatch|batch)-/u.test(entry.name))
        names.push(relative(root, join(directory, entry.name)));
    }
  } catch { /* Root may not exist yet. */ }
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
    // A failed probe is not death. A self-hosted run lives with its owner,
    // whose unknown start time is not evidence of death either.
    const hostAlive = record.host_pid === record.owner_pid
      ? observedAlive(record.owner_pid, record.owner_started_at)
      : observedAlive(record.host_pid, record.host_started_at);
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
  const real = (path: string) => { try { return realpathSync.native(path); } catch { return resolve(path); } }; // macOS /var
  return runs.find((run) => run.run_id === reference) ?? runs.find((run) => real(run.run_dir) === real(reference));
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

export function newestMtimeMs(paths: string[]): number {
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
 * Age out current dispatch run directories and the owner logs beside them.
 *
 * Only timestamped dispatch directories are touched: other run kinds share
 * this root and are not this front door's to delete.
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
        "quarantined",
        "partial",
        "usage_limited",
        "rate_limited",
        "auth_required",
        "model_unavailable",
        "permission_blocked",
        "stalled",
        "startup_timeout",
        "writer_changed",
        "timed_out",
        "interrupted",
        "lost",
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
