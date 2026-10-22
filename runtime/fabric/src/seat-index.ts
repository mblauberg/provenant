import { createHash } from "node:crypto";
import { lstatSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { databasePath } from "./identity.js";
import { processMatches, processStartedAt } from "./run-registry.js";

/**
 * A seat is the client process an MCP server acts for (its parent: claude,
 * codex, ...), told apart by pid, start time and label. Each dispatch writes
 * one file, once, into its seat's directory, so there is nothing to merge, lock
 * or cap: a seat's runs are the files in its directory.
 */
const PRUNE_AFTER_MS = 14 * 24 * 3600 * 1000;
let mcpServer = false;

/** Called by the MCP entry point only; a CLI dispatch never speaks for a client. */
export function actAsMcpServer(): void { mcpServer = true; }

const seatsDirectory = (env: NodeJS.ProcessEnv) => join(dirname(databasePath(env)), "lane-seats");
const safe = (value: string) => value.replace(/[^A-Za-z0-9._-]/gu, "_").slice(0, 64) + "-" +
  createHash("sha256").update(value).digest("hex").slice(0, 8);

interface RunFile { run_dir: string; pid: number; started_at: string; label: string | null }

function readRunFile(path: string): RunFile | undefined {
  try {
    if (!lstatSync(path).isFile()) return undefined;
    const value = JSON.parse(readFileSync(path, "utf8")) as Partial<RunFile>;
    return typeof value.run_dir === "string" && Number.isInteger(value.pid) && typeof value.started_at === "string"
      ? { run_dir: value.run_dir, pid: value.pid!, started_at: value.started_at,
        label: typeof value.label === "string" ? value.label : null } : undefined;
  } catch { return undefined; }
}

const names = (directory: string): string[] => { try { return readdirSync(directory); } catch { return []; } };

/** Drop seats whose client is gone, and old run files whose run is no longer running. */
function prune(root: string, now = Date.now()): void {
  for (const seat of names(root)) {
    const directory = join(root, seat);
    const files = names(directory).filter((name) => name.endsWith(".json"));
    if (!files.length) continue; // A writer may be between creating the directory and its first file.
    const first = readRunFile(join(directory, files[0]!));
    if (!first || !processMatches(first.pid, first.started_at)) {
      rmSync(directory, { recursive: true, force: true });
      continue;
    }
    for (const name of files) {
      const path = join(directory, name);
      try {
        const file = readRunFile(path);
        if (!file || now - statSync(path).mtimeMs < PRUNE_AFTER_MS) continue;
        const status = JSON.parse(readFileSync(join(file.run_dir, "dispatch-status.json"), "utf8")).status;
        if (status !== "running") rmSync(path, { force: true });
      } catch { rmSync(path, { force: true }); }
    }
  }
}

/** Record a run under the seat this MCP server acts for. Never fails a dispatch. */
export function recordSeatRun(runDir: string, env: NodeJS.ProcessEnv, host: { pid: number } = { pid: process.ppid }): void {
  if (!mcpServer) return;
  try {
    const started = processStartedAt(host.pid);
    if (host.pid <= 1 || started === null) return;
    const label = env.AGENT_FABRIC_LABEL ?? null;
    const root = seatsDirectory(env);
    const directory = join(root, `${host.pid}-${safe(started)}-${safe(label ?? "")}`);
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    const name = `${createHash("sha256").update(runDir).digest("hex").slice(0, 32)}.json`;
    const temporary = join(directory, `.${name}.${process.pid}.tmp`);
    writeFileSync(temporary, JSON.stringify({ run_dir: runDir, pid: host.pid, started_at: started, label }) + "\n", { mode: 0o600 });
    renameSync(temporary, join(directory, name));
    prune(root);
  } catch { /* Discovery falls back to the project; the dispatch must not fail. */ }
}

/**
 * The nearest ancestor that is a recorded, live seat, with the runs of every
 * label it matches. A shell or terminal is never one: only a client an MCP
 * server acted for is recorded.
 */
export function findSeat(ancestors: Iterable<number>, env: NodeJS.ProcessEnv): { pid: number; runs: string[] } | undefined {
  const root = seatsDirectory(env);
  const wanted = env.AGENT_FABRIC_LABEL;
  const seats = names(root).map((name) => ({ name, pid: Number(/^(\d+)-/u.exec(name)?.[1]) })).filter((seat) => seat.pid > 1);
  for (const pid of ancestors) {
    const runs: string[] = [];
    for (const seat of seats) {
      if (seat.pid !== pid) continue;
      for (const name of names(join(root, seat.name))) {
        const file = name.endsWith(".json") ? readRunFile(join(root, seat.name, name)) : undefined;
        if (!file || file.pid !== pid || !processMatches(pid, file.started_at)) continue;
        if (wanted !== undefined && file.label !== null && file.label !== wanted) continue;
        runs.push(file.run_dir);
      }
    }
    if (runs.length) return { pid, runs };
  }
  return undefined;
}

/** Project and run path (relative to its run root) of a recorded run directory. */
export function seatRunLocation(runDir: string): { project: string; runPath: string } | undefined {
  const runs = dirname(runDir);
  if (!runs.endsWith("/runs") || !runDir.includes("/.agent-run/runs/")) return undefined;
  return { project: dirname(dirname(runs)), runPath: `runs/${runDir.slice(runs.length + 1)}` };
}
