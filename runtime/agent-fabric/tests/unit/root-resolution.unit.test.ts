import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import { defaultDaemonStartOptions } from "../../src/cli/default-daemon-options.ts";
import { fabricCliCommand, resolveFabricRoots } from "../../src/cli/root-resolution.ts";

afterEach(() => {
  vi.unstubAllEnvs();
});

describe("Fabric root resolution", () => {
  it("defaults both roots to AGENTS_HOME for the fused layout", () => {
    vi.stubEnv("AGENTS_HOME", "/fixture/agents-home");
    vi.stubEnv("AGENT_FABRIC_PRODUCT_ROOT", "");
    vi.stubEnv("AGENT_FABRIC_INSTANCE_ROOT", "");

    expect(resolveFabricRoots({})).toEqual({
      productRoot: resolve("/fixture/agents-home"),
      instanceRoot: resolve("/fixture/agents-home"),
    });
  });

  it("resolves independently configured product and instance roots", () => {
    vi.stubEnv("AGENTS_HOME", "/fixture/agents-home");
    vi.stubEnv("AGENT_FABRIC_PRODUCT_ROOT", "/fixture/product");
    vi.stubEnv("AGENT_FABRIC_INSTANCE_ROOT", "/fixture/instance");

    expect(resolveFabricRoots({})).toEqual({
      productRoot: resolve("/fixture/product"),
      instanceRoot: resolve("/fixture/instance"),
    });
  });

  it("gives explicit root flags precedence over the environment", () => {
    vi.stubEnv("AGENTS_HOME", "/fixture/agents-home");
    vi.stubEnv("AGENT_FABRIC_PRODUCT_ROOT", "/fixture/environment-product");
    vi.stubEnv("AGENT_FABRIC_INSTANCE_ROOT", "/fixture/environment-instance");

    expect(resolveFabricRoots({
      productRootFlag: "/fixture/flag-product",
      instanceRootFlag: "/fixture/flag-instance",
    })).toEqual({
      productRoot: resolve("/fixture/flag-product"),
      instanceRoot: resolve("/fixture/flag-instance"),
    });
  });

  it("renders the bundled CLI from the resolved product root", () => {
    expect(fabricCliCommand({ productRootFlag: "/fixture/product root's" }))
      .toBe(`'/fixture/product root'"'"'s/scripts/agent-fabric'`);
  });

  it("routes all twelve production remedy emitters through the product-root formatter", async () => {
    const expectedCalls = new Map([
      ["core/bootstrap-mcp-custody.ts", 1],
      ["cli/status.ts", 2],
      ["mcp/credentials.ts", 1],
      ["cli/mcp-bootstrap.ts", 5],
      ["cli/mcp-roster-renewal.ts", 2],
      ["daemon/protocol.ts", 1],
    ]);
    let total = 0;
    for (const [relativePath, expected] of expectedCalls) {
      const source = await readFile(resolve(import.meta.dirname, "../../src", relativePath), "utf8");
      expect(source).not.toContain("$HOME/.agents/scripts/agent-fabric");
      const calls = source.match(/\$\{fabricCliCommand\(/gu)?.length ?? 0;
      expect(calls, relativePath).toBe(expected);
      total += calls;
    }
    expect(total).toBe(12);
  });

  it("derives daemon configuration from the instance root and schemas from the product root", () => {
    const paths = {
      stateDirectory: "/fixture/state",
      runtimeDirectory: "/fixture/state/runtime",
      databasePath: "/fixture/state/fabric.sqlite3",
      socketPath: "/fixture/state/runtime/fabric.sock",
    };

    expect(defaultDaemonStartOptions(paths, {
      environment: {
        AGENT_FABRIC_PRODUCT_ROOT: "/fixture/product",
        AGENT_FABRIC_INSTANCE_ROOT: "/fixture/instance",
      },
    })).toEqual({
      ...paths,
      configuration: {
        globalConfigPath: resolve("/fixture/instance/config/agent-fabric.yaml"),
        compatibilityPath: resolve("/fixture/instance/config/adapter-compatibility.yaml"),
        compatibilitySchemaPath: resolve(
          "/fixture/product/runtime/agent-fabric/schemas/adapter-compatibility.schema.json",
        ),
        agentsHome: resolve("/fixture/product"),
      },
    });
  });
});
