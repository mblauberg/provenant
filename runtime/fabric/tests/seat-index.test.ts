import { existsSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, it } from "vitest";

import { recordSeatRun } from "../src/seat-index.js";

it("a CLI dispatch never creates a seat", () => {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "fabric-cli-seat-")));
  try {
    recordSeatRun(join(root, "p/.agent-run/runs/20260101-0000-batch-user-abc"),
      { ...process.env, AGENT_FABRIC_STATE_DIRECTORY: join(root, "state") }, { pid: process.ppid });
    expect(existsSync(join(root, "state/lane-seats"))).toBe(false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
