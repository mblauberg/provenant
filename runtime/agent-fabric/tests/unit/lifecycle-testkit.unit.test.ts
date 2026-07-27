import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it, vi } from "vitest";

import {
  type LifecycleFixture,
  writeLifecycleCheckpoint,
} from "../support/lifecycle-testkit.ts";

describe("lifecycle testkit", () => {
  it("keeps same-agent checkpoints unique when the clock does not advance", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-lifecycle-testkit-"));
    const runDirectory = join(directory, ".agent-run", "run-stage3");
    const fixture = {
      runDirectory,
      providerSessionMarker: "fake-session:leader:g1",
    } as LifecycleFixture;
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);

    try {
      const first = await writeLifecycleCheckpoint(fixture, {
        agentId: "leader",
        nextAction: "first checkpoint",
      });
      const firstBytes = await readFile(join(runDirectory, first.relativePath));
      const second = await writeLifecycleCheckpoint(fixture, {
        agentId: "leader",
        nextAction: "second checkpoint",
      });

      expect(
        second.relativePath,
        "same-agent checkpoint writes in one clock tick must not collide",
      ).not.toBe(first.relativePath);
      expect(await readFile(join(runDirectory, first.relativePath))).toEqual(firstBytes);
      expect(createHash("sha256").update(firstBytes).digest("hex")).toBe(first.sha256);
    } finally {
      vi.restoreAllMocks();
      await rm(directory, { recursive: true, force: true });
    }
  });
});
