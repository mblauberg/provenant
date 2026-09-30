import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it } from "vitest";

import type { Identity } from "../src/identity.js";
import { waitForLanes } from "../src/lane-wait.js";
import type { RunRead } from "../src/run-reader.js";
import { Store } from "../src/store.js";

const roots: string[] = [];
const stores: Store[] = [];
afterEach(() => {
  for (const store of stores.splice(0)) store.close();
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

const who: Identity = { project: "/project", cwd: "/project", agentId: "wait-seat", provider: "codex" };

function store() {
  const root = mkdtempSync(join(tmpdir(), "fabric-lane-wait-"));
  roots.push(root);
  const opened = new Store(join(root, "fabric.sqlite3"));
  stores.push(opened);
  opened.announce(who);
  return opened;
}

function lane(id: string, state = "terminal", status: string | null = "ok", taskId: string | null = id): RunRead {
  return { id, run_id: `mcp-${id}`, task_id: taskId, run_path: `runs/${id}`, state, status, route: null, model: null,
    started_at: null, last_progress_at: null, pgid: null, pgid_alive: null, result_path: null, receipt_path: null,
    writer: false, worktree: null, attempt: 1 };
}

const busy = () => Object.assign(new Error("database is locked"), { code: "SQLITE_BUSY" });

function run(opened: Pick<Store, "unseenLanes" | "markLanesSeen">, runs: RunRead[][], write?: (text: string) => Promise<void>) {
  const output: string[] = [];
  let reads = 0;
  return {
    output,
    code: waitForLanes({
      who, store: opened,
      read: async () => ({ schema: "fabric.runs.v1", status: "ok", runs: runs[Math.min(reads++, runs.length - 1)]! }),
      write: write ?? (async (text) => { output.push(text); }),
      fail: (text) => { throw new Error(text); },
      sleep: async () => undefined,
    }),
  };
}

it("retries contention instead of reporting no running lanes", async () => {
  const opened = store();
  let failures = 1;
  const flaky = {
    unseenLanes: (...args: Parameters<Store["unseenLanes"]>) => {
      if (failures-- > 0) throw busy();
      return opened.unseenLanes(...args);
    },
    markLanesSeen: (...args: Parameters<Store["markLanesSeen"]>) => opened.markLanesSeen(...args),
  };
  const waiter = run(flaky, [[lane("only-done")]]);
  expect(await waiter.code).toBe(0);
  expect(waiter.output.join("")).toContain("only-done");
  expect(waiter.output.join("")).not.toContain("no lanes are running");
});

it("advances the cursor only after the report is written", async () => {
  const opened = store();
  let markFailures = 1;
  const flaky = {
    unseenLanes: (...args: Parameters<Store["unseenLanes"]>) => opened.unseenLanes(...args),
    markLanesSeen: (...args: Parameters<Store["markLanesSeen"]>) => {
      if (markFailures-- > 0) throw busy();
      return opened.markLanesSeen(...args);
    },
  };
  const failed = run(opened, [[lane("kept")]], async () => { throw new Error("EPIPE"); });
  await expect(failed.code).rejects.toThrow("EPIPE");
  const retried = run(flaky, [[lane("kept")]]);
  expect(await retried.code).toBe(0);
  expect(retried.output.join("")).toContain("kept");
  const again = run(opened, [[lane("kept")]]);
  expect(await again.code).toBe(0);
  expect(again.output.join("")).toBe("no lanes are running\n");
});

it("keys a legacy lane without a task id the same way as fabric_status", async () => {
  const opened = store();
  opened.acknowledgeTerminal(who, { run_id: "mcp-legacy", attempt: 1, run_dir: "/project/.agent-run/mcp-legacy" });
  const waiter = run(opened, [[lane("legacy-display-id", "terminal", "ok", null)].map((row) => ({ ...row, run_id: "mcp-legacy" }))]);
  expect(await waiter.code).toBe(0);
  expect(waiter.output.join("")).toBe("no lanes are running\n");
});

function timed(opened: Store, runs: RunRead[][], options: { all?: boolean; timeoutSeconds?: number }) {
  const output: string[] = [];
  let reads = 0, clock = 0;
  return {
    output,
    code: waitForLanes({
      who, store: opened, ...options,
      read: async () => ({ schema: "fabric.runs.v1", status: "ok", runs: runs[Math.min(reads++, runs.length - 1)]! }),
      write: async (text) => { output.push(text); },
      fail: (text) => { throw new Error(text); },
      sleep: async (ms) => { clock += ms; },
      now: () => clock,
    }),
  };
}

it("--all holds every finished lane until the last one is done", async () => {
  const opened = store();
  const waiter = timed(opened, [
    [lane("a"), lane("b", "running", null)],
    [lane("a"), lane("b", "running", null)],
    [lane("a"), lane("b")],
  ], { all: true });
  expect(await waiter.code).toBe(0);
  const text = waiter.output.join("");
  expect(text).toContain("  a  ");
  expect(text).toContain("  b  ");
  expect(waiter.output).toHaveLength(1);
});

it("without --all the first finisher is reported alone", async () => {
  const waiter = timed(store(), [[lane("a"), lane("b", "running", null)]], {});
  expect(await waiter.code).toBe(0);
  expect(waiter.output.join("")).toContain("  a  ");
  expect(waiter.output.join("")).not.toContain("  b  ");
});

it("--timeout ends the wait with exit 124 and names the lanes still running", async () => {
  const waiter = timed(store(), [[lane("a", "running", null), lane("b", "running", null)]], { all: true, timeoutSeconds: 10 });
  expect(await waiter.code).toBe(124);
  expect(waiter.output.join("")).toBe("timeout after 10s; still running: a b\n");
});

it("--all --timeout still reports lanes that finished before the deadline", async () => {
  const opened = store();
  const waiter = timed(opened, [[lane("a"), lane("b", "running", null)]], { all: true, timeoutSeconds: 4 });
  expect(await waiter.code).toBe(124);
  const text = waiter.output.join("");
  expect(text).toContain("  a  ");
  expect(text).toContain("timeout after 4s; still running: b");
  // The finished lane is now seen; a later wait does not repeat it.
  const later = timed(opened, [[lane("a"), lane("b")]], { all: true });
  expect(await later.code).toBe(0);
  expect(later.output.join("")).toContain("  b  ");
  expect(later.output.join("")).not.toContain("  a  ");
});

it("bounds cursor retries under contention by the deadline and leaves the lane for redelivery", async () => {
  const opened = store();
  const stuck = {
    unseenLanes: (...args: Parameters<Store["unseenLanes"]>) => opened.unseenLanes(...args),
    markLanesSeen: () => { throw busy(); },
  };
  const output: string[] = [];
  let clock = 0;
  const code = await waitForLanes({
    who, store: stuck, timeoutSeconds: 5,
    read: async () => ({ schema: "fabric.runs.v1", status: "ok", runs: [lane("kept")] }),
    write: async (text) => { output.push(text); },
    fail: (text) => { throw new Error(text); },
    sleep: async (ms) => { clock += ms; },
    now: () => clock,
  });
  expect(code).toBe(124);
  expect(clock).toBe(5000);
  // Nothing was marked seen, so the lane is reported again.
  const again = run(opened, [[lane("kept")]]);
  expect(await again.code).toBe(0);
  expect(again.output.join("")).toContain("kept");
});

it("--timeout 0 polls once", async () => {
  const waiter = timed(store(), [[lane("a", "running", null)]], { timeoutSeconds: 0 });
  expect(await waiter.code).toBe(124);
  expect(waiter.output.join("")).toBe("timeout after 0s; still running: a\n");
});

it("does not write the cursor at the deadline when contention clears exactly then", async () => {
  const opened = store();
  let clock = 0, marks = 0;
  const flaky = {
    unseenLanes: (...args: Parameters<Store["unseenLanes"]>) => opened.unseenLanes(...args),
    markLanesSeen: (...args: Parameters<Store["markLanesSeen"]>) => {
      if (marks++ === 0) throw busy();
      return opened.markLanesSeen(...args);
    },
  };
  const code = await waitForLanes({
    who, store: flaky, timeoutSeconds: 1,
    read: async () => ({ schema: "fabric.runs.v1", status: "ok", runs: [lane("edge")] }),
    write: async () => undefined, fail: (text) => { throw new Error(text); },
    sleep: async (ms) => { clock += ms; }, now: () => clock,
  });
  expect(code).toBe(124);
  expect(marks).toBe(1);
  const again = run(opened, [[lane("edge")]]);
  expect(await again.code).toBe(0);
  expect(again.output.join("")).toContain("edge");
});
