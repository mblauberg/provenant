import { chmodSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it } from "vitest";

import { runRoot, type Identity } from "../src/identity.js";
import { laneReader } from "../src/seat-lanes.js";
import type { RunRead, RunReadResponse } from "../src/run-reader.js";

const roots: string[] = [];
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });

const who: Identity = { project: "/project", cwd: "/project", agentId: "a", provider: "codex" };
const row = (id: string, host?: string): RunRead => ({ id: host ? `${id}@${host}` : id, run_id: `mcp-${id}`, task_id: id,
  run_path: `runs/${id}`, state: "running", status: null, route: null, model: null, started_at: null, ended_at: null,
  last_progress_at: null, pgid: null, pgid_alive: null, result_path: null, receipt_path: null, writer: false,
  worktree: null, attempt: 1, ...(host ? { host, reachability: "reachable" } : {}) });

it("reads the peers once a minute and the local files between", async () => {
  let clock = 0;
  const calls: boolean[] = [];
  let local = [row("a")];
  const read = async (_project: string, localOnly: boolean): Promise<RunReadResponse> => {
    calls.push(localOnly);
    return localOnly ? { schema: "fabric.runs.v1", status: "ok", runs: local } as RunReadResponse
      : { schema: "fabric.runs.v1", status: "ok", hosts: [{ host: "laptop", local: true }, { host: "shop", local: false }],
        runs: [row("a", "laptop"), row("p", "shop")] } as unknown as RunReadResponse;
  };
  const next = laneReader({ who, projects: () => [], now: () => clock, deps: { read } });
  expect((await next()).runs.map((r) => r.id)).toEqual(["a@laptop", "p@shop"]);
  clock = 30_000;
  expect((await next()).runs.map((r) => r.id)).toEqual(["a@laptop", "p@shop"]);
  expect(calls).toEqual([false, true]);
  local = [row("a"), row("b")]; // a new local lane forces a full read
  clock = 31_000;
  await next();
  expect(calls).toEqual([false, true, true, false]);
  clock = 100_000;
  await next();
  expect(calls.at(-1)).toBe(false);
});

it("asks git once per directory however many run directories are listed", () => {
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
  runRoot(repo, env);
  const first = calls();
  expect(first).toBeGreaterThan(0);
  for (let i = 0; i < 200; i++) runRoot(repo, env);
  expect(calls()).toBe(first);
});
