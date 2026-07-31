import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { resolveSplitConfiguration } from "../../src/cli/split-config-paths.ts";
import { resolveStatusPaths } from "../../src/cli/status.ts";

describe("split-root contract rig", () => {
  // EXPECTED FAILURE: #549 owns the fail-open defaulted-instance-layer fix.
  it("does not offer a defaulted instance layer as an explicit narrowing layer", () => {
    const configuration = resolveSplitConfiguration({
      environment: {
        AGENT_FABRIC_PRODUCT_ROOT: "/fixture/product",
        AGENTS_HOME: "/fixture/defaulted-instance",
      },
      exists: (path) => path === "/fixture/defaulted-instance/config/agent-fabric.yaml",
    });

    expect(configuration.globalConfigPath).toBe("/fixture/product/config/agent-fabric.yaml");
    expect(configuration.localConfigPath).toBeUndefined();
    expect(configuration.compatibilityPath).toBe(join("/fixture/product", "config", "adapter-compatibility.yaml"));
  });

  it("keeps resolved files on the root that owns their ADR 0019 class", () => {
    const productRoot = "/fixture/product";
    const instanceRoot = "/fixture/instance";
    const selected = resolveStatusPaths([
      "--product-root", productRoot,
      "--instance-root", instanceRoot,
    ]);

    expect(selected.config).toBe(join(productRoot, "config", "agent-fabric.yaml"));
    expect(selected.compatibility).toBe(join(productRoot, "config", "adapter-compatibility.yaml"));
    expect(selected.compatibilitySchema).toBe(join(
      productRoot,
      "runtime",
      "agent-fabric",
      "schemas",
      "adapter-compatibility.schema.json",
    ));
    expect(selected.reviewProfile.startsWith(`${productRoot}/`)).toBe(true);
    expect(selected.modelRouting.startsWith(`${instanceRoot}/`)).toBe(true);
    expect(selected.config.startsWith(`${instanceRoot}/`)).toBe(false);
    expect(selected.compatibility.startsWith(`${instanceRoot}/`)).toBe(false);
    expect(selected.compatibilitySchema.startsWith(`${instanceRoot}/`)).toBe(false);
    expect(selected.reviewProfile.startsWith(`${instanceRoot}/`)).toBe(false);
    expect(selected.modelRouting.startsWith(`${productRoot}/`)).toBe(false);
  });

  it("offers the instance layer only for split roots and never for fused roots", () => {
    const split = resolveSplitConfiguration({
      productRootFlag: "/fixture/product",
      instanceRootFlag: "/fixture/instance",
      exists: () => true,
    });
    const fused = resolveSplitConfiguration({
      productRootFlag: "/fixture/product",
      instanceRootFlag: "/fixture/product",
      exists: () => true,
    });

    expect(split.localConfigPath).toBe("/fixture/instance/config/agent-fabric.yaml");
    expect(fused.localConfigPath).toBeUndefined();
  });

  it("does not let an irrelevant instance-root move change product-owned answers", () => {
    const first = resolveSplitConfiguration({
      productRootFlag: "/fixture/product",
      instanceRootFlag: "/fixture/instance-a",
      exists: () => false,
    });
    const second = resolveSplitConfiguration({
      productRootFlag: "/fixture/product",
      instanceRootFlag: "/fixture/instance-b",
      exists: () => false,
    });

    expect(second.globalConfigPath).toBe(first.globalConfigPath);
    expect(second.compatibilityPath).toBe(first.compatibilityPath);
    expect(second.compatibilitySchemaPath).toBe(first.compatibilitySchemaPath);
    expect(second.agentsHome).toBe(first.agentsHome);
    expect(second.localConfigPath).toBeUndefined();
  });

  it("keeps status product paths stable when only the instance root moves", () => {
    const first = resolveStatusPaths([
      "--product-root", "/fixture/product",
      "--instance-root", "/fixture/instance-a",
    ]);
    const second = resolveStatusPaths([
      "--product-root", "/fixture/product",
      "--instance-root", "/fixture/instance-b",
    ]);

    expect(second.config).toBe(first.config);
    expect(second.compatibility).toBe(first.compatibility);
    expect(second.compatibilitySchema).toBe(first.compatibilitySchema);
    expect(second.reviewProfile).toBe(first.reviewProfile);
    expect(second.modelRouting).not.toBe(first.modelRouting);
  });
});
