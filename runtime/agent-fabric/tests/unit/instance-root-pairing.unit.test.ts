import { mkdir, mkdtemp, realpath, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { hasPairedInstanceRoot } from "../../src/cli/instance-root-pairing.ts";

/**
 * An instance may contribute a local configuration layer only when its
 * machine-local pointer names the product actually in use (#563). The two sides
 * of that comparison are produced by different tools: `install-harness` records
 * `product_root.resolve(strict=True)`, which follows symlinks, while a product
 * root taken from a flag or the environment arrives only lexically resolved. So
 * the symlink cases below are the ones that matter, not an edge case.
 */

type Fixture = { realProduct: string; instanceRoot: string; root: string };

async function makeFixture(label: string, pointerProduct?: string): Promise<Fixture> {
  const root = await realpath(await mkdtemp(join(tmpdir(), `fabric-pairing-${label}-`)));
  const realProduct = join(root, "product");
  const instanceRoot = join(root, "instance");
  await mkdir(realProduct, { recursive: true });
  await mkdir(join(instanceRoot, ".agent-fabric"), { recursive: true });
  await writeFile(
    join(instanceRoot, ".agent-fabric", "product-root.json"),
    `${JSON.stringify({ schema_version: 1, product_root: pointerProduct ?? realProduct })}\n`,
  );
  return { realProduct, instanceRoot, root };
}

describe("pairing an instance root to its product", () => {
  it("pairs when the product root is named directly", async () => {
    const { realProduct, instanceRoot } = await makeFixture("direct");
    expect(hasPairedInstanceRoot({ productRoot: realProduct, instanceRoot })).toBe(true);
  });

  it("pairs when the product is reached through a symlink", async () => {
    const { realProduct, instanceRoot, root } = await makeFixture("symlink");
    const link = join(root, "product-link");
    await symlink(realProduct, link);
    expect(hasPairedInstanceRoot({ productRoot: link, instanceRoot })).toBe(true);
  });

  it("pairs when the pointer itself records a symlinked path", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-pairing-both-")));
    const realProduct = join(root, "product");
    const instanceRoot = join(root, "instance");
    await mkdir(realProduct, { recursive: true });
    await mkdir(join(instanceRoot, ".agent-fabric"), { recursive: true });
    const link = join(root, "product-link");
    await symlink(realProduct, link);
    await writeFile(
      join(instanceRoot, ".agent-fabric", "product-root.json"),
      `${JSON.stringify({ schema_version: 1, product_root: link })}\n`,
    );
    expect(hasPairedInstanceRoot({ productRoot: realProduct, instanceRoot })).toBe(true);
  });

  it("refuses a pointer naming a different product tree", async () => {
    const { instanceRoot, root } = await makeFixture("other");
    const other = join(root, "other-product");
    await mkdir(other, { recursive: true });
    expect(hasPairedInstanceRoot({ productRoot: other, instanceRoot })).toBe(false);
  });

  it("refuses an instance with no pointer at all", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-pairing-none-")));
    const realProduct = join(root, "product");
    const instanceRoot = join(root, "instance");
    await mkdir(realProduct, { recursive: true });
    await mkdir(instanceRoot, { recursive: true });
    expect(hasPairedInstanceRoot({ productRoot: realProduct, instanceRoot })).toBe(false);
  });

  it("refuses a pointer whose product no longer exists", async () => {
    const { instanceRoot, root } = await makeFixture("missing", join("/", "no", "such", "product"));
    expect(hasPairedInstanceRoot({ productRoot: join(root, "product"), instanceRoot })).toBe(false);
  });

  it("refuses a product root that does not exist, so it cannot be canonicalised", async () => {
    const { instanceRoot, root } = await makeFixture("absent-product");
    expect(hasPairedInstanceRoot({ productRoot: join(root, "gone"), instanceRoot })).toBe(false);
  });

  /**
   * A local layer can only narrow: allow-lists intersect, workspace roots must
   * stay contained and limits take the minimum. So dropping it silently WIDENS
   * the effective configuration. An absent pointer is an ordinary unpaired
   * instance and stays quiet, but a pointer that exists and cannot be trusted
   * must not be downgraded into "no local layer".
   */
  it("refuses quietly when the pointer is simply absent", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-pairing-quiet-")));
    const realProduct = join(root, "product");
    const instanceRoot = join(root, "instance");
    await mkdir(realProduct, { recursive: true });
    await mkdir(join(instanceRoot, ".agent-fabric"), { recursive: true });
    expect(hasPairedInstanceRoot({ productRoot: realProduct, instanceRoot })).toBe(false);
  });

  it("raises when the pointer exists but is not readable JSON", async () => {
    const { realProduct, instanceRoot } = await makeFixture("corrupt");
    await writeFile(join(instanceRoot, ".agent-fabric", "product-root.json"), "{not json");
    expect(() => hasPairedInstanceRoot({ productRoot: realProduct, instanceRoot }))
      .toThrow(/product pointer/i);
  });

  it("raises when the pointer is well-formed JSON of the wrong shape", async () => {
    const { realProduct, instanceRoot } = await makeFixture("shape");
    await writeFile(
      join(instanceRoot, ".agent-fabric", "product-root.json"),
      `${JSON.stringify({ schema_version: 2, product_root: realProduct })}\n`,
    );
    expect(() => hasPairedInstanceRoot({ productRoot: realProduct, instanceRoot }))
      .toThrow(/product pointer/i);
  });
});
