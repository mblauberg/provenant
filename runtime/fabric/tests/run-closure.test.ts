import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, it } from "vitest";
import { terminateRecordedRun, type RecordedRun } from "../src/run-registry.js";

it("closing a run whose directory was already removed resolves instead of rejecting", async () => {
  const root = mkdtempSync(join(tmpdir(), "fabric-closure-"));
  const runDir = join(root, "removed-run");
  rmSync(root, { recursive: true, force: true });
  const run = {
    schema_version: 1, kind: "dispatch", run_dir: runDir, workspace: root, run_token: "t",
    owner_pid: 2_147_483_000, owner_pgid: 2_147_483_000, owner_started_at: null,
    host_pid: 2_147_483_001, host_started_at: null, started_at: new Date().toISOString(),
    owner_stdout: `${runDir}-owner.stdout.jsonl`, owner_stderr: `${runDir}-owner.stderr.log`,
    run_id: "mcp-remove", running: false, orphaned: false, provider: null,
  } as unknown as RecordedRun;
  await expect(terminateRecordedRun(run, 10, "cancelled")).resolves.toMatchObject({ reason: "not running" });
});
