import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it, vi } from "vitest";

// A process-identity probe can fail transiently; that alone must not orphan a
// live host's run, because the next dispatch's orphan reap would kill it.
const probe = { fail: 0 };
vi.mock("node:child_process", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:child_process")>();
  return {
    ...actual,
    execFileSync: ((file: string, ...rest: unknown[]) => {
      const args = rest[0] as string[] | undefined;
      if (probe.fail && file === "/bin/ps" && args?.includes(String(probe.fail))) throw new Error("ps unavailable");
      return (actual.execFileSync as (...args: unknown[]) => unknown)(file, ...rest);
    }) as typeof actual.execFileSync,
  };
});
const { listRecordedRuns, processStartedAt } = await import("../src/run-registry.js");

const roots: string[] = [];
afterEach(() => {
  probe.fail = 0;
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function recordRun(hostStartedAt: string | null) {
  const workspace = realpathSync(mkdtempSync(join(tmpdir(), "fabric-host-")));
  roots.push(workspace);
  const runDir = join(workspace, ".agent-run", "mcp-host");
  mkdirSync(runDir, { recursive: true });
  writeFileSync(join(runDir, "dispatch-owner.json"), JSON.stringify({
    schema_version: 1, kind: "dispatch", run_dir: runDir, workspace, run_token: "host",
    owner_pid: process.ppid, owner_pgid: process.ppid, owner_started_at: processStartedAt(process.ppid),
    host_pid: process.pid, host_started_at: hostStartedAt, started_at: new Date().toISOString(),
    owner_stdout: "", owner_stderr: "",
  }));
  return workspace;
}

it("keeps a live host's run when its identity probe fails", () => {
  const workspace = recordRun(processStartedAt(process.pid));
  probe.fail = process.pid;
  const [row] = listRecordedRuns(workspace);
  expect(row?.orphaned).toBe(false);
});

it("still orphans a run whose host identity was never recorded", () => {
  const workspace = recordRun(null);
  const [row] = listRecordedRuns(workspace);
  expect(row?.running).toBe(true);
  expect(row?.orphaned).toBe(true);
});
