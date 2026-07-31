import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { resolveSplitConfiguration } from "../../src/cli/split-config-paths.ts";
import { resolveStatusPaths } from "../../src/cli/status.ts";

/**
 * Pairing dependencies for a fixture whose paths do not exist on disk. The
 * production readers are `readFileSync` and `realpathSync`, so a fixture path
 * would otherwise be judged unpaired for the uninteresting reason that it is
 * not there. `pointerProduct` is what the instance's machine-local pointer
 * claims; omit it to model an instance carrying no pointer at all.
 */
function pairing(pointerProduct?: string) {
  return {
    exists: () => true,
    realpath: (path: string) => path,
    readFile: (path: string) => {
      if (!pointerProduct || !path.endsWith("/.agent-fabric/product-root.json")) {
        const error: NodeJS.ErrnoException = new Error(`ENOENT: ${path}`);
        error.code = "ENOENT";
        throw error;
      }
      return JSON.stringify({ schema_version: 1, product_root: pointerProduct });
    },
  };
}

describe("split-root contract rig", () => {
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

  /**
   * Split roots are necessary but not sufficient. Before #563 this contract
   * read "split roots offer the layer", and that was the fail-open rule itself:
   * any instance root that happened to be set could narrow an unrelated
   * product. The instance must also be PAIRED, meaning its machine-local
   * pointer names the product in use. Both original properties are kept below,
   * and the unpaired case is added, so this reads as a strictly stronger
   * contract rather than one relaxed to match new code.
   */
  it("offers the instance layer only for paired split roots, never for fused or unpaired ones", () => {
    const split = resolveSplitConfiguration({
      productRootFlag: "/fixture/product",
      instanceRootFlag: "/fixture/instance",
      ...pairing("/fixture/product"),
    });
    const fused = resolveSplitConfiguration({
      productRootFlag: "/fixture/product",
      instanceRootFlag: "/fixture/product",
      ...pairing("/fixture/product"),
    });
    const unpaired = resolveSplitConfiguration({
      productRootFlag: "/fixture/product",
      instanceRootFlag: "/fixture/instance",
      ...pairing("/fixture/other-product"),
    });
    const unpointed = resolveSplitConfiguration({
      productRootFlag: "/fixture/product",
      instanceRootFlag: "/fixture/instance",
      ...pairing(),
    });

    expect(split.localConfigPath).toBe("/fixture/instance/config/agent-fabric.yaml");
    expect(fused.localConfigPath).toBeUndefined();
    expect(unpaired.localConfigPath).toBeUndefined();
    expect(unpointed.localConfigPath).toBeUndefined();
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
