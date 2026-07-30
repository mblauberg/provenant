import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import {
  bootstrapMcpSeat,
  inspectBootstrapMcpSeat,
  schemaCutoverGate,
} from "../../src/cli/mcp-bootstrap.ts";
import { mcpBootstrapRenewalCommand } from "../../src/cli/mcp-roster-renewal.ts";
import { Fabric } from "../../src/core/fabric.ts";

const cleanup: string[] = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe("product-root remedy rendering", () => {
  it("renders the core bootstrap custody peer-provision remedy from its product root", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-remedy-root-"));
    cleanup.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    const productRoot = join(root, "product");
    const fabric = new Fabric({
      databasePath: join(root, "fabric.sqlite3"),
      workspaceRoots: [root],
      productRoot,
    });

    try {
      expect(() => fabric.bootstrapCurrentMcpSeat({
        canonicalRoot: root,
        trustRecordDigest: `sha256:${"a".repeat(64)}`,
        seat: "agy" as "codex",
        expiresAt: "2026-07-19T00:00:00.000Z",
      })).toThrow(`${join(productRoot, "scripts", "agent-fabric")}' mcp peer-provision`);
    } finally {
      await fabric.close();
    }
  });

  it("renders the schema cutover command from the configured product root", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-schema-remedy-root-")));
    cleanup.push(root);
    const productRoot = join(root, "product");

    expect(schemaCutoverGate(
      join(root, "missing.sqlite3"),
      new Error("schema mismatch"),
      { AGENT_FABRIC_PRODUCT_ROOT: productRoot },
    ).command).toContain(`'${join(productRoot, "scripts", "agent-fabric")}'`);
  });

  it("renders the bootstrap inspection seat refusal from the configured product root", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-inspect-remedy-root-")));
    cleanup.push(root);
    const productRoot = join(root, "product");

    await expect(inspectBootstrapMcpSeat({
      environment: {
        AGENT_FABRIC_PRODUCT_ROOT: productRoot,
        AGENT_FABRIC_SEAT: "agy",
      },
      cwd: root,
      paths: {
        stateDirectory: join(root, "state"),
        runtimeDirectory: join(root, "runtime"),
        databasePath: join(root, "fabric.sqlite3"),
        socketPath: join(root, "fabric.sock"),
      },
    })).rejects.toThrow(`'${join(productRoot, "scripts", "agent-fabric")}' mcp peer-provision`);
  });

  it("renders the bootstrap seat refusal from the configured product root", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-bootstrap-remedy-root-")));
    cleanup.push(root);
    const productRoot = join(root, "product");

    await expect(bootstrapMcpSeat({
      environment: {
        AGENT_FABRIC_PRODUCT_ROOT: productRoot,
        AGENT_FABRIC_SEAT: "agy",
      },
      cwd: root,
      paths: {
        stateDirectory: join(root, "state"),
        runtimeDirectory: join(root, "runtime"),
        databasePath: join(root, "fabric.sqlite3"),
        socketPath: join(root, "fabric.sock"),
      },
    })).rejects.toThrow(`'${join(productRoot, "scripts", "agent-fabric")}' mcp peer-provision`);
  });

  it("renders the bootstrap renewal command from its required product root", () => {
    const productRoot = "/fixture/product";

    expect(mcpBootstrapRenewalCommand("/fixture/project", "codex", productRoot))
      .toContain(`'${join(productRoot, "scripts", "agent-fabric")}' bootstrap`);
  });
});
