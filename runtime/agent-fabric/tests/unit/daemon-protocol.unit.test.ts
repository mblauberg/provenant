import { afterEach, describe, expect, it, vi } from "vitest";

import type { FabricClient } from "../../src/core/fabric.ts";
import { bootstrapMcpSeatInput, dispatchClientMethod } from "../../src/daemon/protocol.ts";

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("daemon provider action protocol", () => {
  it("names an exact shell-safe peer-provision remedy for optional bootstrap seats", () => {
    vi.stubEnv("AGENT_FABRIC_PRODUCT_ROOT", "/fixture/wrong-product");

    expect(() => bootstrapMcpSeatInput({
      canonicalRoot: "/tmp/project root's",
      trustRecordDigest: `sha256:${"a".repeat(64)}`,
      seat: "agy",
      expiresAt: "2026-07-19T00:00:00.000Z",
    }, "/fixture/product")).toThrow(
      `'/fixture/product/scripts/agent-fabric' mcp peer-provision ` +
      `--project '/tmp/project root'"'"'s' --seat agy`,
    );
  });

  it.each([
    ["adapter", { adapterId: "fake\0lifecycle", actionId: "provider-action:spawn", operation: "spawn", taskId: "task-1", authorityId: "authority-1" }],
    ["action", { adapterId: "fake-lifecycle", actionId: "provider-action\0steer", operation: "steer" }],
  ])("rejects a NUL-containing %s identity before client dispatch", async (_case, identity) => {
    const dispatchProviderAction = vi.fn();
    const client = { dispatchProviderAction } as unknown as FabricClient;

    await expect(dispatchClientMethod(client, "dispatchProviderAction", {
      ...identity,
      certifyingReview: null,
      payload: {},
      commandId: "provider-action:command",
    })).rejects.toMatchObject({
      code: "PROTOCOL_INVALID",
      message: "provider adapter ID and action ID must not contain NUL",
    });
    expect(dispatchProviderAction).not.toHaveBeenCalled();
  });
});
