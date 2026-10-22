import { chmodSync, lstatSync, mkdirSync, readdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { databasePath } from "./identity.js";
import { processMatches, processStartedAt } from "./run-registry.js";

/**
 * A seat is the client process an MCP host serves (its parent: claude, codex,
 * ...), identified by pid and start time so a reused pid is not mistaken for it.
 * Each seat keeps a durable list of the runs its host launched, so the
 * association outlives the host and the launcher that wrote it.
 */
export interface SeatRecord {
  pid: number;
  started_at: string;
  label: string | null;
  runs: string[];
}

const MAX_RUNS = 500;
const seatDirectory = (env: NodeJS.ProcessEnv) => join(dirname(databasePath(env)), "lane-seats");

function readSeat(path: string): SeatRecord | undefined {
  try {
    if (!lstatSync(path).isFile()) return undefined;
    const value = JSON.parse(readFileSync(path, "utf8")) as Partial<SeatRecord>;
    return Number.isInteger(value.pid) && typeof value.started_at === "string" && Array.isArray(value.runs)
      ? { pid: value.pid!, started_at: value.started_at, label: typeof value.label === "string" ? value.label : null,
        runs: value.runs.filter((run): run is string => typeof run === "string") }
      : undefined;
  } catch { return undefined; }
}

/** Record a run under the seat this MCP host serves. Never fails a dispatch. */
export function recordSeatRun(runDir: string, env: NodeJS.ProcessEnv, host = { pid: process.ppid }): void {
  try {
    const started = processStartedAt(host.pid);
    if (host.pid <= 1 || started === null) return;
    const directory = seatDirectory(env);
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    chmodSync(directory, 0o700);
    const path = join(directory, `${host.pid}.json`);
    const prior = readSeat(path);
    const runs = prior?.started_at === started ? prior.runs.filter((run) => run !== runDir) : [];
    runs.push(runDir);
    const record: SeatRecord = { pid: host.pid, started_at: started, label: env.AGENT_FABRIC_LABEL ?? null,
      runs: runs.slice(-MAX_RUNS) };
    const temporary = `${path}.${process.pid}.tmp`;
    writeFileSync(temporary, JSON.stringify(record) + "\n", { mode: 0o600 });
    renameSync(temporary, path);
    // Seats whose client has gone are dropped, which bounds the directory.
    for (const name of readdirSync(directory)) {
      const other = /^(\d+)\.json$/u.exec(name);
      const stale = other && Number(other[1]) !== host.pid ? readSeat(join(directory, name)) : undefined;
      if (stale && !processMatches(stale.pid, stale.started_at)) {
        try { unlinkSync(join(directory, name)); } catch { /* Another reader pruned it. */ }
      }
    }
  } catch { /* Discovery falls back to the project; the dispatch must not fail. */ }
}

/**
 * The nearest ancestor that is a recorded, still-live seat. A shell, terminal
 * or launchd is never one, because only an MCP host's client is recorded.
 */
export function findSeat(ancestors: Iterable<number>, env: NodeJS.ProcessEnv): SeatRecord | undefined {
  const directory = seatDirectory(env);
  for (const pid of ancestors) {
    const record = readSeat(join(directory, `${pid}.json`));
    if (!record || record.pid !== pid || !processMatches(pid, record.started_at)) continue;
    // Seats of one client told apart by label never claim each other's lanes.
    if (record.label !== null && env.AGENT_FABRIC_LABEL !== undefined && record.label !== env.AGENT_FABRIC_LABEL) continue;
    return record;
  }
  return undefined;
}

/** Project and run path (relative to its run root) of a recorded run directory. */
export function seatRunLocation(runDir: string): { project: string; runPath: string } | undefined {
  const runs = dirname(runDir);
  if (!runs.endsWith("/runs") || !runDir.includes("/.agent-run/runs/")) return undefined;
  return { project: dirname(dirname(runs)), runPath: `runs/${runDir.slice(runs.length + 1)}` };
}
