import { afterEach, describe, expect, it, vi } from "vitest";

import { childEnvironment } from "../../src/daemon/daemon-child.ts";

describe("daemon child environment", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
  });

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

  it("forwards only the bounded test fixtures to a test child", () => {
    vi.stubEnv("NODE_ENV", "test");
    vi.stubEnv("AGENT_FABRIC_TEST_DATABASE_INSPECTION_ATTEMPTS", "1");
    vi.stubEnv("AGENT_FABRIC_TEST_ROSTER_CONVERGENCE_ATTEMPTS", "1");
    vi.stubEnv("AGENT_FABRIC_TEST_SECRET", "must-not-cross-daemon-boundary");

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

    expect(environment).toMatchObject({
      NODE_ENV: "test",
      AGENT_FABRIC_TEST_DATABASE_INSPECTION_ATTEMPTS: "1",
    });
    expect(environment).not.toHaveProperty("AGENT_FABRIC_TEST_DATABASE_INSPECTION_RACE_PATH");
    expect(environment).not.toHaveProperty("AGENT_FABRIC_TEST_ROSTER_CONVERGENCE_ATTEMPTS");
    expect(environment).not.toHaveProperty("AGENT_FABRIC_TEST_SECRET");
  });
});
