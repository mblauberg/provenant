
import { psOutput } from "./ps.mjs";
import type { Identity } from "./identity.js";
import { findSeat, seatRunLocation } from "./seat-index.js";
import { readRuns, type RunRead, type RunReadResponse } from "./run-reader.js";

/** Parent pid of a live process, or null when it cannot be read. */
export function parentPid(pid: number): number | null {
  try {
    const value = Number(String(psOutput(["-o", "ppid=", "-p", String(pid)])).trim());
    return Number.isInteger(value) && value > 0 ? value : null;
  } catch { return null; }
}

/** This process and every ancestor, nearest first, bounded against a cycle. */
export function ancestorPids(pid = process.pid, parent: (pid: number) => number | null = parentPid): Set<number> {
  const chain = new Set<number>();
  for (let current: number | null = pid; current !== null && current > 1 && !chain.has(current) && chain.size < 64;
    current = parent(current)) chain.add(current);
  return chain;
}

/** Projects and run paths this command's seat launched, from the durable seat index. */
export function seatRunDirectories(ancestors: Set<number>, env: NodeJS.ProcessEnv = process.env): Map<string, Set<string>> {
  const found = new Map<string, Set<string>>();
  const seat = findSeat(ancestors, env);
  for (const runDir of seat?.runs ?? []) {
    const location = seatRunLocation(runDir);
    if (!location) continue;
    const set = found.get(location.project) ?? new Set<string>();
    set.add(location.runPath);
    found.set(location.project, set);
  }
  return found;
}

/** One identity for a lane however a read qualified it: host, bare run, bare task, attempt. */
export function laneIdentity(row: RunRead, localHost?: string): string {
  const host = row.host ?? localHost ?? "";
  const bare = (value: unknown) => {
    const text = String(value ?? "");
    return host && text.endsWith(`@${host}`) ? text.slice(0, -host.length - 1) : text;
  };
  return `${host}\0${bare(row.run_id)}\0${bare(row.task_id)}\0${row.attempt}`;
}

/** Give a bare local row the form the federated read gives every row. */
function qualify(row: RunRead, host: string): RunRead {
  if (row.host !== undefined) return row;
  const tag = (value: string) => (value.endsWith(`@${host}`) ? value : `${value}@${host}`);
  return { ...row, id: tag(row.id), run_id: tag(row.run_id), task_id: row.task_id === null ? null : tag(row.task_id),
    host, reachability: "reachable" };
}

export interface SeatLaneDependencies {
  /** Read one project; `localOnly` skips the peer hosts. */
  read?: (project: string, localOnly: boolean) => Promise<RunReadResponse>;
  /** The caller's own read skips the peer hosts. */
  localOnly?: boolean;
  /** This host's name when a previous full read learned it. */
  localHost?: string;
  ancestors?: Set<number>;
  env?: NodeJS.ProcessEnv;
}

type Federated = RunReadResponse & { hosts?: Array<{ host: string; local?: boolean }> };

/**
 * The lanes of the caller's project plus those its own seat's MCP host
 * launched from other projects, each lane once under one identity.
 */
export async function readSeatRuns(who: Identity, ids: string[] | undefined,
  deps: SeatLaneDependencies = {}): Promise<RunReadResponse> {
  const read = deps.read ?? ((project: string, localOnly: boolean) =>
    readRuns(project, ids, 0, undefined, null, undefined, false, localOnly));
  const own = await read(who.project, deps.localOnly === true) as Federated;
  if (own.status !== "ok") return own;
  const localHost = own.hosts?.find((entry) => entry.local)?.host ?? deps.localHost;
  const runs = localHost ? own.runs.map((row) => qualify(row, localHost)) : own.runs;
  const others = seatRunDirectories(deps.ancestors ?? ancestorPids(), deps.env);
  others.delete(who.project);
  const seen = new Set(runs.map((row) => laneIdentity(row, localHost)));
  const extra: RunRead[] = [];
  for (const [project, directories] of others) {
    const other = await read(project, true).catch(() => undefined);
    if (other?.status !== "ok") continue;
    for (const row of other.runs) {
      if (row.source_project !== undefined || row.host !== undefined || !directories.has(row.run_path)) continue;
      const lane = localHost ? qualify(row, localHost) : row;
      const key = laneIdentity(lane, localHost);
      if (seen.has(key)) continue;
      seen.add(key);
      extra.push(lane);
    }
  }
  return { ...own, runs: extra.length || runs !== own.runs ? [...runs, ...extra] : own.runs };
}

export type LaneReader = (() => Promise<RunReadResponse>) & {
  /** True when the last read left the peer hosts at an earlier reading. */
  stale: () => boolean;
  /** Read everything now, peers included. */
  refresh: () => Promise<RunReadResponse>;
};

/**
 * A lane reader for a polling loop. Peer hosts cost an SSH session each, so
 * they are read at most every `peerMs`; between those only this host's files
 * are, and the peers' last rows stand in. `refresh` re-reads them for a final decision.
 */
export function laneReader(options: {
  who: Identity; ids?: string[]; peerMs?: number; now?: () => number; deps?: SeatLaneDependencies;
}): LaneReader {
  const now = options.now ?? Date.now;
  const peerMs = options.peerMs ?? 60_000;
  let last: Federated | undefined;
  let lastAt = 0;
  let stale = false;
  const refresh = async () => {
    const result = await readSeatRuns(options.who, options.ids, { ...options.deps, localOnly: false });
    if (result.status === "ok") { last = result as Federated; lastAt = now(); }
    stale = false;
    return result;
  };
  const read = async () => {
    const localHost = last?.hosts?.find((entry) => entry.local)?.host;
    if (!last || localHost === undefined || now() - lastAt >= peerMs) return refresh();
    const local = await readSeatRuns(options.who, options.ids, { ...options.deps, localOnly: true, localHost });
    if (local.status !== "ok") return local;
    stale = true;
    return { ...last, runs: [...local.runs, ...last.runs.filter((row) => row.host !== undefined && row.host !== localHost)] };
  };
  return Object.assign(read, { stale: () => stale, refresh });
}
