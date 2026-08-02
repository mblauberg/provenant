import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { AUTHORITY_ACTION_VOCABULARY, openFabric, verifyFabricReceiptLink } from "../../src/index.ts";
import { describe, expect, it } from "vitest";
import { TEST_AUTHORITY_V2_FIELDS } from "../support/authority-v2-testkit.ts";
import { writeDeliveryRunFixture } from "../support/delivery-run-fixture.ts";
import { createCurrentSessionRun } from "../support/current-session-testkit.ts";

async function currentRun(
  root: string,
  input: Omit<Parameters<typeof createCurrentSessionRun>[0], "databasePath" | "workspaceRoot">,
) {
  return await createCurrentSessionRun({
    databasePath: join(root, "fabric.sqlite3"),
    workspaceRoot: root,
    ...input,
  });
}

describe("Stage 1 chair receipt link", () => {
  it("continues to verify a hash-bound historical schema-v1 fabric receipt", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-v1-receipt-link-"));
    const runDirectory = join(root, ".agent-run", "run-v1-link");
    await mkdir(runDirectory, { recursive: true });
    try {
      const receipt = {
        schemaVersion: 1,
        runId: "run-v1-link",
        chair: { agentId: "chair", adapterId: null },
        observedAt: "2026-07-11T00:00:00.000Z",
        stageOwners: [], agents: [], executionProfile: "headless", directInputProvenance: "unavailable",
        modelRoutingReceipts: [], taskAndWriteLeases: [],
        messagesSentReceivedAbandoned: { sent: 0, delivered: 0, acknowledged: 0, abandoned: 0, expired: 0 },
        objectiveChecks: [], crossFamilyReviews: [], providerFailuresAndSubstitutions: [], operatorInterventions: [],
        compactionsAndRotations: [], counts: { agents: 0, tasks: 0, messages: 0, deliveries: 0, leases: 0, events: 0 },
      };
      const bytes = Buffer.from(`${JSON.stringify(receipt)}\n`);
      const sha256 = createHash("sha256").update(bytes).digest("hex");
      const relativePath = ".agent-run/run-v1-link/historical-v1.json";
      await writeFile(join(runDirectory, "historical-v1.json"), bytes);
      const runReceiptPath = await writeDeliveryRunFixture({
        runDirectory, runId: "run-v1-link", artifactPath: relativePath, artifactSha256: sha256,
      });

      await expect(verifyFabricReceiptLink({ runReceiptPath })).resolves.toEqual({
        valid: true, relativePath, schemaVersion: 1, sha256,
      });
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  it("verifies the fabric receipt declared by the canonical delivery-run artifact", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-receipt-link-"));
    const runDirectory = join(root, ".agent-run", "run-link");
    await mkdir(runDirectory, { recursive: true });
    const fabric = await openFabric({ databasePath: join(root, "fabric.sqlite3"), workspaceRoots: [root] });
    try {
      const run = await currentRun(root, {
        runId: "run-link",
        projectRunDirectory: runDirectory,
        chair: {
          agentId: "chair",
          authority: {
            ...TEST_AUTHORITY_V2_FIELDS,
            workspaceRoots: ["."],
            sourcePaths: ["."],
            artifactPaths: [".agent-run/run-link"],
            actions: [...AUTHORITY_ACTION_VOCABULARY],
            disclosure: { level: "scoped", scopes: ["local"] } as const,
            expiresAt: "2099-01-01T00:00:00.000Z",
            budget: { turns: 4, "cost:USD": 4 },
          },
        },
      });
      const chair = fabric.connect(run.chairCapability);
      const exported = await chair.exportReceipt({ commandId: "receipt-link:export" });
      const artifactPath = `.agent-run/run-link/${exported.relativePath}`;
      const runReceiptPath = await writeDeliveryRunFixture({
        runDirectory,
        runId: "run-link",
        artifactPath,
        artifactSha256: exported.sha256,
      });

      await expect(verifyFabricReceiptLink({ runReceiptPath })).resolves.toMatchObject({
        valid: true,
        relativePath: artifactPath,
        schemaVersion: 2,
        sha256: exported.sha256,
      });

      const fabricReceiptPath = join(runDirectory, exported.relativePath);
      const bytes = await readFile(fabricReceiptPath);
      const invalid = JSON.parse(bytes.toString("utf8")) as Record<string, unknown>;
      delete invalid.taskOwners;
      const invalidBytes = Buffer.from(`${JSON.stringify(invalid)}\n`);
      await writeFile(fabricReceiptPath, invalidBytes);
      const invalidHash = createHash("sha256").update(invalidBytes).digest("hex");
      const linked = JSON.parse(await readFile(runReceiptPath, "utf8")) as { artifacts: Array<{ id: string; digest: string }> };
      const declaredArtifact = linked.artifacts.find((artifact) => artifact.id === "fabric-coordination-receipt");
      if (declaredArtifact === undefined) throw new TypeError("delivery run is missing its fabric receipt artifact");
      declaredArtifact.digest = `sha256:${invalidHash}`;
      await writeFile(runReceiptPath, `${JSON.stringify(linked, null, 2)}\n`);
      await expect(verifyFabricReceiptLink({ runReceiptPath })).rejects.toMatchObject({ code: "RECEIPT_SCHEMA_MISMATCH" });

      declaredArtifact.digest = `sha256:${exported.sha256}`;
      await writeFile(runReceiptPath, `${JSON.stringify(linked, null, 2)}\n`);
      await writeFile(fabricReceiptPath, bytes);
      await writeFile(fabricReceiptPath, Buffer.concat([bytes, Buffer.from("tampered\n")]));
      await expect(verifyFabricReceiptLink({ runReceiptPath })).rejects.toMatchObject({
        code: "RECEIPT_HASH_MISMATCH",
      });
      expect(createHash("sha256").update(bytes).digest("hex")).toBe(exported.sha256);
    } finally {
      await fabric.close();
      await rm(root, { recursive: true, force: true });
    }
  });

  it("verifies accepted stochastic assurance and rejects a tampered nested evaluation artifact", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-stochastic-receipt-link-"));
    const runDirectory = join(root, ".agent-run", "run-stochastic-link");
    await mkdir(runDirectory, { recursive: true });
    const fabric = await openFabric({ databasePath: join(root, "fabric.sqlite3"), workspaceRoots: [root] });
    try {
      const run = await currentRun(root, {
        runId: "run-stochastic-link",
        projectRunDirectory: runDirectory,
        chair: {
          agentId: "chair",
          authority: {
            ...TEST_AUTHORITY_V2_FIELDS,
            workspaceRoots: ["."],
            sourcePaths: ["."],
            artifactPaths: [".agent-run/run-stochastic-link"],
            actions: [...AUTHORITY_ACTION_VOCABULARY],
            disclosure: { level: "scoped", scopes: ["local"] } as const,
            expiresAt: "2099-01-01T00:00:00.000Z",
            budget: { turns: 4, "cost:USD": 4 },
          },
        },
      });
      const exported = await fabric.connect(run.chairCapability).exportReceipt({
        commandId: "receipt-link:stochastic-export",
      });
      const artifactPath = `.agent-run/run-stochastic-link/${exported.relativePath}`;
      const runReceiptPath = await writeDeliveryRunFixture({
        runDirectory,
        runId: "run-stochastic-link",
        artifactPath,
        artifactSha256: exported.sha256,
        profile: "agent-product",
        accepted: true,
      });

      await expect(verifyFabricReceiptLink({ runReceiptPath })).resolves.toMatchObject({
        valid: true,
        relativePath: artifactPath,
        schemaVersion: 2,
        sha256: exported.sha256,
      });

      const nestedOutput = join(root, "evaluation", "evidence", "output.json");
      await writeFile(nestedOutput, `${await readFile(nestedOutput, "utf8")}tampered\n`);
      await expect(verifyFabricReceiptLink({ runReceiptPath })).rejects.toMatchObject({
        code: "RECEIPT_LINK_INVALID",
        message: expect.stringContaining("artifact output digest mismatch"),
      });
    } finally {
      await fabric.close();
      await rm(root, { recursive: true, force: true });
    }
  });

  it("rejects the removed RUN_RECEIPT fabric_receipt link", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-legacy-receipt-link-"));
    const runDirectory = join(root, ".agent-run", "run-legacy-link");
    await mkdir(runDirectory, { recursive: true });
    const fabric = await openFabric({ databasePath: join(root, "fabric.sqlite3"), workspaceRoots: [root] });
    try {
      const run = await currentRun(root, {
        runId: "run-legacy-link",
        projectRunDirectory: runDirectory,
        chair: {
          agentId: "chair",
          authority: {
            ...TEST_AUTHORITY_V2_FIELDS,
            workspaceRoots: ["."],
            sourcePaths: ["."],
            artifactPaths: [".agent-run/run-legacy-link"],
            actions: [...AUTHORITY_ACTION_VOCABULARY],
            disclosure: { level: "scoped", scopes: ["local"] } as const,
            expiresAt: "2099-01-01T00:00:00.000Z",
            budget: { turns: 1 },
          },
        },
      });
      const exported = await fabric.connect(run.chairCapability).exportReceipt({ commandId: "receipt-link:legacy-export" });
      const legacyReceiptPath = join(runDirectory, "RUN_RECEIPT.json");
      await writeFile(legacyReceiptPath, `${JSON.stringify({
        schema_version: 1,
        run_id: "run-legacy-link",
        fabric_receipt: {
          relative_path: exported.relativePath,
          schema_version: exported.schemaVersion,
          sha256: exported.sha256,
        },
      }, null, 2)}\n`);

      await expect(verifyFabricReceiptLink({ runReceiptPath: legacyReceiptPath })).rejects.toMatchObject({
        code: "RECEIPT_LINK_INVALID",
      });
    } finally {
      await fabric.close();
      await rm(root, { recursive: true, force: true });
    }
  });

  it("rejects a partial RUN.json that is not a valid delivery-run receipt", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-partial-delivery-run-"));
    const runDirectory = join(root, ".agent-run", "run-partial");
    await mkdir(runDirectory, { recursive: true });
    const fabric = await openFabric({ databasePath: join(root, "fabric.sqlite3"), workspaceRoots: [root] });
    try {
      const run = await currentRun(root, {
        runId: "run-partial",
        projectRunDirectory: runDirectory,
        chair: {
          agentId: "chair",
          authority: {
            ...TEST_AUTHORITY_V2_FIELDS,
            workspaceRoots: ["."],
            sourcePaths: ["."],
            artifactPaths: [".agent-run/run-partial"],
            actions: [...AUTHORITY_ACTION_VOCABULARY],
            disclosure: { level: "scoped", scopes: ["local"] } as const,
            expiresAt: "2099-01-01T00:00:00.000Z",
            budget: { turns: 1 },
          },
        },
      });
      const exported = await fabric.connect(run.chairCapability).exportReceipt({ commandId: "receipt-link:partial-export" });
      const runReceiptPath = join(runDirectory, "RUN.json");
      await writeFile(runReceiptPath, `${JSON.stringify({
        schema_version: 1,
        contract: "delivery-run",
        run_id: "run-partial",
        artifacts: [{
          id: "fabric-coordination-receipt",
          path: `.agent-run/run-partial/${exported.relativePath}`,
          digest: `sha256:${exported.sha256}`,
        }],
      }, null, 2)}\n`);

      await expect(verifyFabricReceiptLink({ runReceiptPath })).rejects.toMatchObject({ code: "RECEIPT_LINK_INVALID" });
    } finally {
      await fabric.close();
      await rm(root, { recursive: true, force: true });
    }
  });

  it("rejects a delivery run whose directory name differs from run_id", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-run-directory-mismatch-"));
    const runDirectory = join(root, ".agent-run", "run-directory");
    await mkdir(runDirectory, { recursive: true });
    const fabric = await openFabric({ databasePath: join(root, "fabric.sqlite3"), workspaceRoots: [root] });
    try {
      const run = await currentRun(root, {
        runId: "run-declared",
        projectRunDirectory: runDirectory,
        chair: {
          agentId: "chair",
          authority: {
            ...TEST_AUTHORITY_V2_FIELDS,
            workspaceRoots: ["."],
            sourcePaths: ["."],
            artifactPaths: [".agent-run/run-directory"],
            actions: [...AUTHORITY_ACTION_VOCABULARY],
            disclosure: { level: "scoped", scopes: ["local"] } as const,
            expiresAt: "2099-01-01T00:00:00.000Z",
            budget: { turns: 1 },
          },
        },
      });
      const exported = await fabric.connect(run.chairCapability).exportReceipt({ commandId: "receipt-link:mismatch-export" });
      await expect(writeDeliveryRunFixture({
        runDirectory,
        runId: "run-declared",
        artifactPath: `.agent-run/run-directory/${exported.relativePath}`,
        artifactSha256: exported.sha256,
      })).rejects.toMatchObject({
        stderr: expect.stringContaining("run-dir name must match run-id"),
      });
    } finally {
      await fabric.close();
      await rm(root, { recursive: true, force: true });
    }
  });
});
