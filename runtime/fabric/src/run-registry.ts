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
export { runRoot } from "./identity.js";
import { execFile } from "node:child_process";
import { psOutput } from "./ps.mjs";
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
import { promisify } from "node:util";
const execFileAsync = promisify(execFile);
import { basename, dirname, isAbsolute, join, relative, resolve } from "node:path";
import { pythonOwner, pythonOwnerEnvironment } from "./python-owner.js";

export const OWNER_RECORD_NAME = "dispatch-owner.json";
export const PROVIDER_RECORD_NAME = "dispatch-provider.json";
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

/** One Python pass over the selected active runs; the writer owns all receipt writes. */
export async function reapAttemptRuns(workspace: string, runDirs?: string[]): Promise<string[]> {
  const candidates = [...new Set(runDirs ?? (() => { const root = runRoot(workspace); return runDirectoryNames(workspace).map((name) => join(root, name)); })())];
  const active = candidates.filter((dir) => readJson(join(dir, "RUN_RECEIPT.json"))?.status === "active" &&
    !runOwners(readOwnerRecord(dir), readJson(join(dir, "dispatch-status.json")))
      .some((owner) => observedAlive(owner.pid, owner.startedAt)));
  if (!active.length) return [];
  const product = process.env.AGENT_FABRIC_PRODUCT_ROOT ?? resolve(import.meta.dirname, "../../..");
  try {
    if (!isAbsolute(product)) throw new Error("AGENT_FABRIC_PRODUCT_ROOT must be absolute");
    const python = await pythonOwner(product, process.env);
    const output = await new Promise<string>((done, reject) => {
      const child = execFile(python,
        [join(product, "skills/orchestrate/scripts/dispatch_run.py"), "--reap-attempts"],
        { cwd: workspace, env: pythonOwnerEnvironment(process.env), timeout: 10_000, maxBuffer: 64 * 1024 },
        (error, stdout) => error ? reject(error) : done(stdout));
      child.stdin?.on("error", () => { /* execFile reports an early exit. */ });
      child.stdin?.end(JSON.stringify(active));
    });
    const changed: unknown = JSON.parse(output);
    return Array.isArray(changed) ? changed.filter((dir): dir is string => typeof dir === "string" && active.includes(dir)) : [];
  } catch (error) {
    // Probe or custody failures preserve the retained evidence for a later read.
    console.warn(`attempt_reap_failed: ${error instanceof Error ? error.message : String(error)}`);
    return [];
  }
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

/** Signal only the recorded process; its owner handles descendant cleanup. */
export function signalRecordedProcess(pid: number, pgid: number, startedAt: string | null,
  signal: NodeJS.Signals): boolean {
  if (!processMatches(pid, startedAt) || pid === process.pid || !positiveInteger(pgid)) return false;
  try {
    const [actualGroup, ...command] = psOutput(["-o", "pgid=,comm=", "-p", String(pid)]).trim().split(/\s+/u);
    const comm = basename(command.join(" "));
    if (Number(actualGroup) !== pgid || !comm || comm === "?" ||
        ["codex-code-mode-host", "codex-code-mode"].includes(comm)) return false;
    process.kill(pid, signal);
    return true;
  } catch { return false; }
}

/** A process group the graph runner started: its leader is `pid`, which is also `pgid`. */
export interface RecordedGroup { pid: number; pgid: number; started_at: string | null }

/**
 * Whether a recorded group has exited, may be signalled, or cannot be told
 * apart from someone else's. It has exited when no member remains, or when a
 * different process now holds the leader's pid (a pid cannot be reused while
 * its group exists). It is attributable while the leader matches its start
 * time, or once the leader has gone and no process holds its pid. Any probe
 * that cannot answer leaves it fenced.
 */
function groupState(group: RecordedGroup): "exited" | "attributable" | "fenced" {
  if (!positiveInteger(group.pgid) || group.pgid !== group.pid || group.pgid === process.pid) return "fenced";
  const probe = (target: number): "alive" | "gone" | "unknown" => {
    try { process.kill(target, 0); return "alive"; }
    catch (error) { return (error as NodeJS.ErrnoException).code === "ESRCH" ? "gone" : "unknown"; }
  };
  const members = probe(-group.pgid);
  if (members === "gone") return "exited";
  if (members === "unknown") return "fenced";
  const leader = probe(group.pgid);
  if (leader === "gone") return "attributable";
  if (leader === "unknown" || group.started_at === null) return "fenced";
  const observed = processStartedAt(group.pgid);
  if (observed === null) return "fenced";
  return startMatches(group.pgid, group.started_at, observed) ? "attributable" : "exited";
}

/**
 * Terminate a recorded process group: SIGTERM, a grace period, then SIGKILL,
 * signalling `-pgid` only while the group is attributable, and verify it has
 * gone. `fenced` means the group may still hold members nobody can safely
 * signal; the caller must not treat its work as stopped.
 */
export async function signalRecordedGroup(group: RecordedGroup, graceMs = 3_000): Promise<"exited" | "fenced"> {
  const settle = async (timeoutMs: number) => {
    const deadline = Date.now() + timeoutMs;
    let state = groupState(group);
    while (state === "attributable" && Date.now() < deadline) {
      await new Promise((resolveWait) => setTimeout(resolveWait, 50));
      state = groupState(group);
    }
    return state;
  };
  for (const [signal, waitMs] of [["SIGTERM", graceMs], ["SIGKILL", 2_000]] as const) {
    const state = groupState(group);
    if (state !== "attributable") return state;
    try { process.kill(-group.pgid, signal); }
    catch (error) { if ((error as NodeJS.ErrnoException).code !== "ESRCH") return "fenced"; }
    const after = await settle(waitMs);
    if (after !== "attributable") return after;
  }
  return groupState(group) === "exited" ? "exited" : "fenced";
}

/** The owner cleans its descendants; controller signals never widen to a provider's group. */
export function signalRecordedRun(run: RecordedRun, signal: NodeJS.Signals, source = "run_control"): boolean {
  // Publish before signalling so the provider can retain the controller's reason
  // even when its owner is terminated in the same operation.
  const path = join(run.run_dir, "termination-request.json");
  const temporary = `${path}.${process.pid}.tmp`;
  try {
    writeFileSync(temporary, JSON.stringify({ run_token: run.run_token, signal, source,
      sender_pid: process.pid, requested_at: new Date().toISOString(),
      provider_pid: run.provider?.provider_pid ?? null }) + "\n", { mode: 0o600, flag: "wx" });
    renameSync(temporary, path);
  } catch { /* Missing telemetry must not widen signalling authority. */ }
  const owner = signalRecordedProcess(run.owner_pid, run.owner_pgid, run.owner_started_at, signal);
  const provider =
    run.provider === null
      ? false
      : signalRecordedProcess(run.provider.provider_pid, run.provider.provider_pgid, run.provider.provider_started_at, signal);
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
  source = terminalStatus === "cancelled" ? "cancel_requested" : "run_control",
): Promise<TerminationOutcome> {
  if (!runStillAlive(run)) {
    closeStoppedRun(run.run_dir, terminalStatus);
    return { run_dir: run.run_dir, signalled: false, escalated: false, reason: "not running" };
  }
  const signalled = signalRecordedRun(run, "SIGTERM", source);
  await waitForRunStop(run, escalationMs);
  let escalated = false;
  if (runStillAlive(run)) {
    escalated = signalRecordedRun(run, "SIGKILL", source);
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
  const root = runRoot(workspace);
  for (const name of runDirectoryNames(workspace)) {
    const dir = join(root, name),
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
  // A host may hand its run to the owner between the record read and the host
  // probe. A dead host cannot write again, so a record that still names it is
  // a real orphan.
  const orphans = listRecordedRuns(workspace).filter((run) => {
    const current = run.orphaned ? readOwnerRecord(run.run_dir) : undefined;
    return current?.run_token === run.run_token && current.host_pid === run.host_pid &&
      current.host_started_at === run.host_started_at;
  });
  return await Promise.all(
    orphans.map(async (run) => {
      const outcome = await terminateRecordedRun(run, ESCALATION_MS, "interrupted", "orphan_reaper");
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

/** Liveness for exclusion: an unknown process identity counts as alive. Signalling uses processMatches. */
export function observedAlive(pid: number, startedAt: string | null): boolean {
  if (!positiveInteger(pid)) return false;
  try {
    process.kill(pid, 0);
  } catch (error) {
    // A sandbox can deny kill(pid, 0) for a live host. Only ESRCH proves
    // absence; EPERM/EACCES and other unavailable probes exclude reaping.
    return (error as NodeJS.ErrnoException).code !== "ESRCH";
  }
  if (startedAt === null) return true;
  const observed = processStartedAt(pid);
  // An unavailable process identity is not evidence of death. Signalling stays strict.
  return observed === null || startMatches(pid, startedAt, observed);
}

export type ProcessIdentity = { pid: number; startedAt: string | null; token?: string };

/**
 * A run's owners: its owner record and the identity its launcher stamped into
 * run status after the spawn. The stamp survives a record that could not be
 * written; the two differ only when a record could not be replaced, so both count.
 */
export function runOwners(record: OwnerRecord | undefined, status: Record<string, unknown> | undefined): ProcessIdentity[] {
  const owners: ProcessIdentity[] = record
    ? [{ pid: record.owner_pid, startedAt: record.owner_started_at, token: record.run_token }] : [];
  if (positiveInteger(status?.owner_pid)) {
    const stamped = { pid: status.owner_pid, startedAt: typeof status.owner_started_at === "string" ? status.owner_started_at : null,
      ...(typeof status.run_token === "string" ? { token: status.run_token } : {}) };
    if (!owners.some((owner) => owner.pid === stamped.pid && owner.token === stamped.token)) owners.push(stamped);
  }
  return owners;
}

/**
 * Whether any owner of a run, or a provider one of them recorded, may still be
 * acting for it. `timedProviders` ignores a provider record with no start time,
 * whose pid alone cannot tell the provider from a process that reused it.
 */
export function liveness(runDir: string, owners: ProcessIdentity[], timedProviders = false): { owner: boolean; provider: boolean } {
  const owner = owners.some((identity) => observedAlive(identity.pid, identity.startedAt));
  const provider = owners.some((identity) => {
    const record = identity.token === undefined ? null : readProviderRecord(runDir, identity.token);
    return record !== null && !(timedProviders && record.provider_started_at === null) &&
      observedAlive(record.provider_pid, record.provider_started_at);
  });
  return { owner, provider };
}

/**
 * Whether an attempt's own receipt reads terminal, or undefined with no receipt.
 * The canonical `tasks` receipt decides when present; only without it does the
 * owner's legacy `dispatch/tasks` envelope, written once the attempt ends and
 * carrying no state field, count as terminal evidence.
 */
function receiptTerminal(runDir: string, taskId: string, name: string): boolean | undefined {
  const canonical = readJson(join(runDir, "tasks", taskId, name, "attempt.json"));
  if (canonical !== undefined) return canonical.state === "terminal";
  const legacy = readJson(join(runDir, "dispatch", "tasks", taskId, name, "attempt.json"));
  if (legacy === undefined) return undefined;
  return legacy.record_type === "dispatch-attempt" && typeof legacy.finished_at === "string";
}

/**
 * Whether an attempt's own evidence says no provider of its can still run: its
 * receipt reads terminal, or it never launched. An unlaunched attempt has no
 * directory, its run status announced it with no owner stamped, and every
 * earlier attempt's receipt reads terminal. A closure status synthesised for a
 * dead owner is neither, so it is no evidence a provider ended.
 */
function attemptSettled(runDir: string, taskId: string, attempt: number): boolean {
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/u.test(taskId) || !Number.isInteger(attempt) || attempt < 1) return false;
  const name = `attempt-${String(attempt).padStart(3, "0")}`;
  const trees = ["tasks", "dispatch/tasks"].map((tree) => join(runDir, tree, taskId));
  const own = receiptTerminal(runDir, taskId, name);
  if (own !== undefined) return own;
  if (trees.some((tree) => existsSync(join(tree, name)))) return false;
  const status = readJson(join(runDir, "dispatch-status.json"));
  if (Number(status?.next_attempt) !== attempt || status?.owner_pid !== undefined) return false;
  const earlier = new Set<string>();
  for (const tree of trees) {
    try {
      for (const entry of readdirSync(tree)) if (/^attempt-\d+$/u.test(entry)) earlier.add(entry);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") return false;
    }
  }
  return [...earlier].every((entry) => receiptTerminal(runDir, taskId, entry) === true);
}

/**
 * Whether a run whose task reads terminal may still be acting: its owner
 * (between fallback attempts) or a provider. A provider record with no start
 * time, whose pid alone cannot tell the provider from a process that reused
 * it, is ignored only once the task's attempt has settled by its own evidence.
 */
export function runProcessAlive(runDir: string, taskId: string, attempt: number): boolean {
  const live = liveness(runDir, runOwners(readOwnerRecord(runDir), readJson(join(runDir, "dispatch-status.json"))),
    attemptSettled(runDir, taskId, attempt));
  return live.owner || live.provider;
}
