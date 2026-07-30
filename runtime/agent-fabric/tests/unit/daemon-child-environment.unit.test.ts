import { describe, expect, it } from "vitest";

import { childEnvironment } from "../../src/daemon/daemon-child.ts";

describe("daemon child environment", () => {
  it("carries the resolved product root into the strict child allowlist", () => {
    const environment = childEnvironment({
      databasePath: "/fixture/state/fabric.sqlite3",
      productRoot: "/fixture/product",
      stateDirectory: "/fixture/state",
      runtimeDirectory: "/fixture/runtime",
      socketPath: "/fixture/runtime/fabric.sock",
      adapters: {},
      executionProfile: "headless",
      maximumConcurrentProviderTurns: 1,
      workspaceRoots: ["/fixture/project"],
    }, "afb_fixture", [], "fixture-key", {
      mode: "production-election",
      actionId: "action-1",
      electionGeneration: 1,
      daemonInstanceGeneration: 1,
    });

    expect(environment.AGENT_FABRIC_PRODUCT_ROOT).toBe("/fixture/product");
  });
});
