import { homedir } from "node:os";
import { resolve } from "node:path";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { defaultDaemonStartOptions } from "../../src/cli/default-daemon-options.ts";
import { fabricCliCommand, resolveFabricRoots } from "../../src/domain/fabric-roots.ts";

beforeEach(() => {
  vi.stubEnv("AGENT_FABRIC_PRODUCT_ROOT", undefined);
  vi.stubEnv("AGENT_FABRIC_INSTANCE_ROOT", undefined);
});

afterEach(() => {
  vi.unstubAllEnvs();
  delete process.env.AGENT_FABRIC_PRODUCT_ROOT;
  delete process.env.AGENT_FABRIC_INSTANCE_ROOT;
});

describe("Fabric root resolution", () => {
  it("defaults both roots to ~/.agents when nothing is configured", () => {
    vi.stubEnv("AGENTS_HOME", undefined);

    expect(resolveFabricRoots({})).toEqual({
      productRoot: resolve(homedir(), ".agents"),
      instanceRoot: resolve(homedir(), ".agents"),
    });
  });

  it("uses AGENTS_HOME only for the product root", () => {
    vi.stubEnv("AGENTS_HOME", "/fixture/agents-home");

    expect(resolveFabricRoots({})).toEqual({
      productRoot: resolve("/fixture/agents-home"),
      instanceRoot: resolve(homedir(), ".agents"),
    });
  });

  it("gives explicit --agents-home precedence over split-root environment variables", () => {
    vi.stubEnv("AGENTS_HOME", "/fixture/environment-agents-home");
    vi.stubEnv("AGENT_FABRIC_PRODUCT_ROOT", "/fixture/environment-product");
    vi.stubEnv("AGENT_FABRIC_INSTANCE_ROOT", "/fixture/environment-instance");

    expect(resolveFabricRoots({ agentsHomeFlag: "/fixture/flag-agents-home" })).toEqual({
      productRoot: resolve("/fixture/flag-agents-home"),
      instanceRoot: resolve("/fixture/flag-agents-home"),
    });
  });

  it.each([
    ["AGENT_FABRIC_PRODUCT_ROOT", "relative/product"],
    ["AGENT_FABRIC_INSTANCE_ROOT", "relative/instance"],
  ])("rejects relative paths in %s", (name, value) => {
    vi.stubEnv("AGENTS_HOME", undefined);
    vi.stubEnv(name, value);

    expect(() => resolveFabricRoots({})).toThrow(
      `${name} must be an absolute path, got ${value}`,
    );
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

  it("derives shipped daemon configuration and schemas from the product root", () => {
    const paths = {
      stateDirectory: "/fixture/state",
      runtimeDirectory: "/fixture/state/runtime",
      databasePath: "/fixture/state/fabric.sqlite3",
      socketPath: "/fixture/state/runtime/fabric.sock",
    };

    // ADR 0019 assigns the shipped configuration and the compatibility policy
    // to the product; the instance contributes only a narrowing local layer,
    // and here no such file exists, so none is offered.
    expect(defaultDaemonStartOptions(paths, {
      environment: {
        AGENT_FABRIC_PRODUCT_ROOT: "/fixture/product",
        AGENT_FABRIC_INSTANCE_ROOT: "/fixture/instance",
      },
    })).toEqual({
      ...paths,
      configuration: {
        globalConfigPath: resolve("/fixture/product/config/agent-fabric.yaml"),
        compatibilityPath: resolve("/fixture/product/config/adapter-compatibility.yaml"),
        compatibilitySchemaPath: resolve(
          "/fixture/product/runtime/agent-fabric/schemas/adapter-compatibility.schema.json",
        ),
        agentsHome: resolve("/fixture/product"),
      },
    });
  });
});
