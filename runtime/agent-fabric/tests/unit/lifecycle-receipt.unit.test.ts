import { describe, expect, it } from "vitest";

import {
  lifecycleFailureReceipt,
  validateLifecycleActionReceipt,
} from "../../src/lifecycle/lifecycle-receipt.ts";

describe("lifecycle stale-build evidence", () => {
  it("preserves typed incumbent context in the existing failure receipt", () => {
    const staleBuildEvidence = {
      kind: "daemon-stale-build" as const,
      expectedRuntimeBuildIdentity: "sha256:" + "1".repeat(64),
      currentRuntimeBuildIdentity: "sha256:" + "2".repeat(64),
      pid: 4172,
      socketPath: "/tmp/agent-fabric.sock",
      electionGeneration: 9,
      daemonInstanceGeneration: 4,
      gate: "reconciliation-required" as const,
    };
    const cause = Object.assign(new Error("operator reconciliation is required"), {
      code: "DAEMON_STALE_BUILD",
      staleBuildEvidence,
    });

    const receipt = lifecycleFailureReceipt({
      canonicalRoot: "/workspace",
      seat: "codex",
      generation: "seat-generation-1",
      actions: [],
      phase: "daemon-start",
      cause,
    });

    expect(receipt.failure).toEqual({
      phase: "daemon-start",
      message: "operator reconciliation is required",
      code: "DAEMON_STALE_BUILD",
      evidence: staleBuildEvidence,
    });
    expect(validateLifecycleActionReceipt(receipt)?.failure).toEqual(receipt.failure);
  });

  it("rejects untyped or secret-bearing stale-build evidence", () => {
    const receipt = {
      schemaVersion: 1,
      kind: "agent-fabric-lifecycle-action",
      canonicalRoot: "/workspace",
      seat: "codex",
      generation: "seat-generation-1",
      mutated: false,
      healthy: false,
      actions: [],
      failure: {
        phase: "daemon-start",
        message: "operator reconciliation is required",
        code: "DAEMON_STALE_BUILD",
        evidence: {
          kind: "daemon-stale-build",
          expectedRuntimeBuildIdentity: "sha256:" + "1".repeat(64),
          currentRuntimeBuildIdentity: "sha256:" + "2".repeat(64),
          pid: 4172,
          socketPath: "/tmp/agent-fabric.sock",
          electionGeneration: 9,
          daemonInstanceGeneration: 4,
          gate: "reconciliation-required",
          bootstrapCapability: "secret-must-not-cross-the-evidence-boundary",
        },
      },
    };

    expect(validateLifecycleActionReceipt(receipt)).toBeNull();
  });
});
