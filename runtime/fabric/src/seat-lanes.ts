import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { psOutput } from "./ps.mjs";
import type { Identity } from "./identity.js";
import { processMatches } from "./run-registry.js";
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

/**
 * A lane belongs to this agent when its launcher is a live MCP host whose
 * parent, the agent process, is also an ancestor of this command. The MCP host
 * keeps the directory the agent started in, while the agent's shell may be
 * anywhere, so the project alone cannot say whose lane it is.
 */
export function launchedBySeat(runPath: string, ancestors: Set<number>,
  parent: (pid: number) => number | null = parentPid): boolean {
  try {
    const path = join(runPath, "dispatch-status.json");
    if (!lstatSync(path).isFile()) return false;
    const status = JSON.parse(readFileSync(path, "utf8")) as { host_pid?: unknown; host_started_at?: unknown };
    if (typeof status.host_pid !== "number" || typeof status.host_started_at !== "string") return false;
    if (!processMatches(status.host_pid, status.host_started_at)) return false;
    const agent = parent(status.host_pid);
    return agent !== null && ancestors.has(agent);
  } catch { return false; }
}

/**
 * Run directories, by project, whose launcher is this agent's MCP host. Only
 * the file system is read: a registry can name thousands of projects, so none
 * is opened through the run reader unless one of its runs is ours.
 */
export function seatRunDirectories(projects: string[], ancestors: Set<number>,
  parent: (pid: number) => number | null = parentPid): Map<string, Set<string>> {
  const cached = new Map<number, number | null>();
  const cachedParent = (pid: number) => {
    if (!cached.has(pid)) cached.set(pid, parent(pid));
    return cached.get(pid)!;
  };
  const found = new Map<string, Set<string>>();
  for (const project of projects) {
    // Registered projects are canonical roots, whose run root is beside the checkout.
    const runs = join(project, ".agent-run", "runs");
    let names: string[];
    try { names = readdirSync(runs); } catch { continue; }
    for (const name of names) {
      if (!/^\d{8}-\d{4}-(dispatch|batch)-/u.test(name)) continue;
      if (!launchedBySeat(join(runs, name), ancestors, cachedParent)) continue;
      const set = found.get(project) ?? new Set<string>();
      set.add(`runs/${name}`);
      found.set(project, set);
    }
  }
  return found;
}

export interface SeatLaneDependencies {
  /** Read one project; `localOnly` skips the peer hosts. */
  read?: (project: string, localOnly: boolean) => Promise<RunReadResponse>;
  /** The caller's own read skips the peer hosts. */
  localOnly?: boolean;
  ancestors?: Set<number>;
  parent?: (pid: number) => number | null;
}

/**
 * The lanes of the caller's project plus those its own agent's MCP hosts
 * launched from other registered projects.
 */
export async function readSeatRuns(who: Identity, projects: string[], ids: string[] | undefined,
  deps: SeatLaneDependencies = {}): Promise<RunReadResponse> {
  const read = deps.read ?? ((project: string, localOnly: boolean) =>
    readRuns(project, ids, 0, undefined, null, undefined, false, localOnly));
  const own = await read(who.project, deps.localOnly === true);
  if (own.status !== "ok") return own;
  const others = seatRunDirectories(projects.filter((project) => project !== who.project),
    deps.ancestors ?? ancestorPids(), deps.parent);
  if (!others.size) return own;
  const seen = new Set(own.runs.map((row) => `${row.run_id}:${row.task_id}:${row.attempt}`));
  const extra: RunRead[] = [];
  for (const [project, directories] of others) {
    const other = await read(project, true).catch(() => undefined);
    if (other?.status !== "ok") continue;
    for (const row of other.runs) {
      const key = `${row.run_id}:${row.task_id}:${row.attempt}`;
      if (seen.has(key) || row.source_project !== undefined || row.host !== undefined ||
          !directories.has(row.run_path)) continue;
      seen.add(key);
      extra.push(row);
    }
  }
  return extra.length ? { ...own, runs: [...own.runs, ...extra] } : own;
}

const rowKey = (row: RunRead) => `${row.run_id}:${row.task_id}:${row.attempt}`;

/**
 * A lane reader for a polling loop. Peer hosts cost an SSH session each, so
 * they are read at most every `peerMs`; between those reads only this host's
 * files are, and the peers' last rows stand in. A lane that is new locally
 * forces a full read so it is never shown without its host.
 */
export function laneReader(options: {
  who: Identity; projects: () => string[]; ids?: string[]; peerMs?: number;
  now?: () => number; deps?: SeatLaneDependencies;
}): () => Promise<RunReadResponse> {
  const now = options.now ?? Date.now;
  const peerMs = options.peerMs ?? 60_000;
  let last: RunReadResponse | undefined;
  let lastAt = 0;
  const full = async () => {
    const result = await readSeatRuns(options.who, options.projects(), options.ids, options.deps);
    if (result.status === "ok") { last = result; lastAt = now(); }
    return result;
  };
  return async () => {
    const hosts = (last as { hosts?: Array<{ host: string; local?: boolean }> } | undefined)?.hosts;
    const localHost = hosts?.find((entry) => entry.local)?.host;
    if (!last || localHost === undefined || now() - lastAt >= peerMs) return full();
    const local = await readSeatRuns(options.who, options.projects(), options.ids, { ...options.deps, localOnly: true });
    if (local.status !== "ok") return local;
    const known = new Map(last.runs.filter((row) => row.host === localHost).map((row) => [rowKey(row), row]));
    const runs: RunRead[] = [];
    for (const row of local.runs) {
      const prior = known.get(rowKey(row));
      if (!prior) return full();
      runs.push({ ...row, id: prior.id, host: prior.host, reachability: prior.reachability });
    }
    return { ...last, runs: [...runs, ...last.runs.filter((row) => row.host !== undefined && row.host !== localHost)] };
  };
}
