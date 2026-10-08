import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it, vi } from "vitest";

// A process-identity probe can fail transiently; that alone must not orphan a
// live host's run, because the next dispatch's orphan reap would kill it.
const probe = { fail: 0, code: undefined as string | undefined, shim: undefined as string | undefined };
vi.mock("node:child_process", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:child_process")>();
  return {
    ...actual,
    execFileSync: ((file: string, ...rest: unknown[]) => {
      const args = rest[0] as string[] | undefined;
      if (probe.fail && file === "/bin/ps" && args?.includes(String(probe.fail)))
        throw Object.assign(new Error("ps unavailable"), { code: probe.code });
      if (probe.shim !== undefined && file.endsWith("scripts/bin/ps")) return probe.shim;
      return (actual.execFileSync as (...args: unknown[]) => unknown)(file, ...rest);
    }) as typeof actual.execFileSync,
  };
});
const { listRecordedRuns, processStartedAt } = await import("../src/run-registry.js");

const roots: string[] = [];
afterEach(() => {
  probe.fail = 0;
  probe.code = undefined;
  probe.shim = undefined;
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function recordRun(hostStartedAt: string | null, selfHosted = false, hostPid = process.pid,
  ownerPid = process.ppid) {
  const workspace = realpathSync(mkdtempSync(join(tmpdir(), "fabric-host-")));
  roots.push(workspace);
  const runDir = join(workspace, ".agent-run", "mcp-host");
  mkdirSync(runDir, { recursive: true });
  writeFileSync(join(runDir, "dispatch-owner.json"), JSON.stringify({
    schema_version: 1, kind: "dispatch", run_dir: runDir, workspace, run_token: "host",
    owner_pid: ownerPid, owner_pgid: ownerPid,
    owner_started_at: selfHosted ? hostStartedAt : processStartedAt(ownerPid),
    host_pid: selfHosted ? ownerPid : hostPid, host_started_at: hostStartedAt, started_at: new Date().toISOString(),
    owner_stdout: "", owner_stderr: "",
  }));
  if (selfHosted) writeFileSync(join(runDir, "dispatch-provider.json"), JSON.stringify({
    run_token: "host", provider_pid: process.pid, provider_pgid: process.pid,
    provider_started_at: processStartedAt(process.pid),
  }));
  return workspace;
}

it("keeps a live host's run when its identity probe fails", () => {
  const workspace = recordRun(processStartedAt(process.pid));
  probe.fail = process.pid;
  const [row] = listRecordedRuns(workspace);
  expect(row?.orphaned).toBe(false);
});

it("reads the start time through the bundled shim when system ps cannot start", () => {
  probe.fail = process.pid;
  probe.code = "EPERM"; // seatbelt refuses to exec the setuid /bin/ps
  expect(processStartedAt(process.pid)).toMatch(/^\w{3} \w{3} +\d+ \d\d:\d\d:\d\d \d{4}$/);
});

it("treats the shim's unreadable marker as unknown, not a start time", () => {
  probe.fail = process.pid;
  probe.code = "EPERM";
  probe.shim = "?\n";
  expect(processStartedAt(process.pid)).toBeNull();
});

it("does not fall back when system ps started and failed", () => {
  probe.fail = process.pid;
  expect(processStartedAt(process.pid)).toBeNull();
});

it("still orphans a run whose host identity was never recorded", () => {
  const workspace = recordRun(null, false, 2_147_483_647, process.pid);
  const [row] = listRecordedRuns(workspace);
  expect(row?.running).toBe(true);
  expect(row?.orphaned).toBe(true);
});

it("keeps a self-hosted run while its owner lives, even without its start time", () => {
  const workspace = recordRun(null, true);
  const [row] = listRecordedRuns(workspace);
  expect(row?.provider).not.toBeNull();
  expect(row?.orphaned).toBe(false);
});
