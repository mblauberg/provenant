import { spawn } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it } from "vitest";

import { runRoot, type Identity } from "../src/identity.js";
import { reapOrphanedRuns } from "../src/run-registry.js";
import { findSeat, recordSeatRun } from "../src/seat-index.js";
import { laneIdentity, laneReader, readSeatRuns } from "../src/seat-lanes.js";
import type { RunRead, RunReadResponse } from "../src/run-reader.js";

const roots: string[] = [];
const originalPath = process.env.PATH;
afterEach(() => {
  process.env.PATH = originalPath; for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });

const who: Identity = { project: "/project", cwd: "/project", agentId: "a", provider: "codex" };
const row = (id: string, host?: string): RunRead => ({ id: host ? `${id}@${host}` : id, run_id: `mcp-${id}`, task_id: id,
  run_path: `runs/${id}`, state: "running", status: null, route: null, model: null, started_at: null, ended_at: null,
  last_progress_at: null, pgid: null, pgid_alive: null, result_path: null, receipt_path: null, writer: false,
  worktree: null, attempt: 1, ...(host ? { host, reachability: "reachable" } : {}) });

const federated = (runs: RunRead[]) => ({ schema: "fabric.runs.v1", status: "ok",
  hosts: [{ host: "laptop", local: true }, { host: "shop", local: false }], runs }) as unknown as RunReadResponse;
const bare = (r: RunRead): RunRead => ({ ...r, id: r.id.replace("@laptop", ""), run_id: r.run_id.replace("@laptop", ""),
  task_id: r.task_id?.replace("@laptop", "") ?? null, host: undefined, reachability: undefined } as RunRead);
const qualified = (id: string): RunRead => ({ ...row(id, "laptop"), run_id: `mcp-${id}@laptop`, task_id: `${id}@laptop` });

it("reads the peers once a minute, local reads matching the federated identity", async () => {
  let clock = 0;
  const calls: boolean[] = [];
  let local = [bare(qualified("a"))];
  const read = async (_project: string, localOnly: boolean): Promise<RunReadResponse> => {
    calls.push(localOnly);
    return localOnly ? { schema: "fabric.runs.v1", status: "ok", runs: local } as RunReadResponse
      : federated([qualified("a"), row("p", "shop")]);
  };
  const next = laneReader({ who, now: () => clock, deps: { read, ancestors: new Set() } });
  expect((await next()).runs.map((r) => r.id)).toEqual(["a@laptop", "p@shop"]);
  clock = 30_000;
  const second = await next();
  expect(second.runs.map((r) => [r.id, r.run_id, r.task_id])).toEqual([
    ["a@laptop", "mcp-a@laptop", "a@laptop"], ["p@shop", "mcp-p", "p"]]);
  local = [bare(qualified("a")), bare(qualified("b"))]; // a new local lane is shown without a peer read
  clock = 31_000;
  expect((await next()).runs.map((r) => r.id)).toEqual(["a@laptop", "b@laptop", "p@shop"]);
  expect(calls).toEqual([false, true, true]);
  clock = 100_000;
  await next();
  expect(calls.at(-1)).toBe(false);
});

it("refresh re-reads the peers and clears staleness", async () => {
  let peers = [row("p", "shop")];
  const read = async (_p: string, localOnly: boolean): Promise<RunReadResponse> =>
    localOnly ? { schema: "fabric.runs.v1", status: "ok", runs: [] } as RunReadResponse : federated([...peers]);
  const next = laneReader({ who, now: () => 0, deps: { read, ancestors: new Set() } });
  await next();
  await next();
  expect(next.stale()).toBe(true);
  peers = [];
  expect((await next.refresh()).runs).toEqual([]);
  expect(next.stale()).toBe(false);
});

it("names one lane once however it is qualified", () => {
  expect(laneIdentity(bare(qualified("a")), "laptop")).toBe(laneIdentity(qualified("a")));
  expect(laneIdentity(row("a", "shop"))).not.toBe(laneIdentity(qualified("a")));
});

function seatEnv() {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "fabric-seat-")));
  roots.push(root);
  return { root, env: { ...process.env, AGENT_FABRIC_STATE_DIRECTORY: join(root, "state") } as NodeJS.ProcessEnv };
}

it("keeps a seat's lane after its launcher is gone and ignores shells and other seats", async () => {
  const { root, env } = seatEnv();
  const seat = spawn("sleep", ["30"]);
  const other = spawn("sleep", ["30"]);
  try {
    const runDir = join(root, "elsewhere/.agent-run/runs/20260101-0000-batch-user-abc");
    mkdirSync(runDir, { recursive: true });
    recordSeatRun(runDir, env, { pid: seat.pid! }); // the launcher itself is not recorded and may die
    expect(findSeat([process.pid, seat.pid!], env)?.runs).toEqual([runDir]);
    expect(findSeat([process.pid, other.pid!], env)).toBeUndefined();
    const read = async (project: string): Promise<RunReadResponse> => ({ schema: "fabric.runs.v1", status: "ok",
      runs: project === join(root, "elsewhere") ? [{ ...row("a"), run_path: "runs/20260101-0000-batch-user-abc" }] : [] } as RunReadResponse);
    const result = await readSeatRuns(who, undefined, { read, env, ancestors: new Set([process.pid, seat.pid!]) });
    expect(result.runs.map((r) => r.id)).toEqual(["a"]);
    const none = await readSeatRuns(who, undefined, { read, env, ancestors: new Set([process.pid, other.pid!]) });
    expect(none.runs).toEqual([]);
  } finally { seat.kill(); other.kill(); }
});

it("drops a seat whose client has exited", async () => {
  const { root, env } = seatEnv();
  const seat = spawn("sleep", ["30"]);
  const runDir = join(root, "p/.agent-run/runs/20260101-0000-batch-user-abc");
  recordSeatRun(runDir, env, { pid: seat.pid! });
  const exited = new Promise((done) => seat.once("exit", done));
  seat.kill();
  await exited;
  expect(findSeat([seat.pid!], env)).toBeUndefined();
});

it("lists a lane once when a seat and a pointer both reach it", async () => {
  const { root, env } = seatEnv();
  const seat = spawn("sleep", ["30"]);
  try {
    const runDir = join(root, "elsewhere/.agent-run/runs/20260101-0000-batch-user-abc");
    recordSeatRun(runDir, env, { pid: seat.pid! });
    const read = async (project: string, localOnly: boolean): Promise<RunReadResponse> =>
      project === who.project && !localOnly ? federated([qualified("a")]) : { schema: "fabric.runs.v1", status: "ok",
        runs: [{ ...bare(qualified("a")), run_path: "runs/20260101-0000-batch-user-abc" }] } as RunReadResponse;
    const result = await readSeatRuns(who, undefined, { read, env, ancestors: new Set([seat.pid!]) });
    expect(result.runs.map((r) => r.id)).toEqual(["a@laptop"]);
  } finally { seat.kill(); }
});

it("asks git the same number of times however many run directories are listed", async () => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "fabric-runroot-")));
  roots.push(root);
  const repo = join(root, "repo");
  execFileSync("git", ["init", "-q", repo]);
  const bin = join(root, "bin");
  const log = join(root, "git.log");
  mkdirSync(bin);
  const real = execFileSync("which", ["git"], { encoding: "utf8" }).trim();
  writeFileSync(join(bin, "git"), `#!/bin/sh\necho x >> '${log}'\nexec '${real}' "$@"\n`);
  chmodSync(join(bin, "git"), 0o755);
  const env = { ...process.env, PATH: `${bin}:${process.env.PATH}` };
  const calls = () => { try { return readFileSync(log, "utf8").split("\n").filter(Boolean).length; } catch { return 0; } };
  Object.assign(process.env, { PATH: env.PATH });
  const runDirectories = (count: number) => {
    for (let i = 0; i < count; i++)
      mkdirSync(join(repo, ".agent-run/runs", `20260101-00${String(i).padStart(2, "0")}-batch-user-r${i}`), { recursive: true });
  };
  runDirectories(2);
  const before = calls();
  await reapOrphanedRuns(repo);
  const small = calls() - before;
  runDirectories(40);
  const mid = calls();
  await reapOrphanedRuns(repo);
  expect(calls() - mid).toBe(small);
});

it("follows a nested repository created or removed between reads", () => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "fabric-nested-")));
  roots.push(root);
  execFileSync("git", ["init", "-q", root]);
  const nested = join(root, "sub");
  mkdirSync(nested);
  expect(runRoot(nested)).toBe(join(root, ".agent-run"));
  execFileSync("git", ["init", "-q", nested]);
  expect(runRoot(nested)).toBe(join(nested, ".agent-run"));
  rmSync(join(nested, ".git"), { recursive: true });
  expect(runRoot(nested)).toBe(join(root, ".agent-run"));
});
