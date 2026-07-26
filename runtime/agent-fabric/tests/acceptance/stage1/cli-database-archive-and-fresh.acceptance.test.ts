import { mkdir, mkdtemp, readFile, rename, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { parseCliJson, runSourceCli } from "../../support/cli-process.ts";

const cleanup: string[] = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function fixture(): Promise<Readonly<{
  root: string;
  databasePath: string;
  archiveDirectory: string;
}>> {
  const root = await mkdtemp(join(tmpdir(), "fabric-cutover-cli-"));
  cleanup.push(root);
  const databasePath = join(root, "fabric.sqlite3");
  const database = new Database(databasePath);
  database.exec("CREATE TABLE legacy_sentinel(value TEXT NOT NULL)");
  database.close();
  return { root, databasePath, archiveDirectory: join(root, "archive") };
}

describe("database archive-and-fresh CLI", () => {
  it("previews and completes through digest-bound machine-readable JSON without a TTY", async () => {
    const value = await fixture();

    const previewProcess = await runSourceCli([
      "database",
      "archive-and-fresh",
      "--database",
      value.databasePath,
      "--archive",
      value.archiveDirectory,
    ]);
    const preview = parseCliJson(previewProcess);
    expect(preview).toMatchObject({
      status: "confirmation-required",
      confirmation: {
        sourceSetSha256: expect.stringMatching(/^sha256:[0-9a-f]{64}$/u),
        confirmed: false,
      },
    });
    if (
      typeof preview !== "object" ||
      preview === null ||
      !("confirmation" in preview) ||
      typeof preview.confirmation !== "object" ||
      preview.confirmation === null ||
      !("sourceSetSha256" in preview.confirmation) ||
      typeof preview.confirmation.sourceSetSha256 !== "string"
    ) throw new TypeError("preview omitted its confirmation digest");

    const confirmedProcess = await runSourceCli([
      "database",
      "archive-and-fresh",
      "--database",
      value.databasePath,
      "--archive",
      value.archiveDirectory,
      "--confirm-source-set",
      preview.confirmation.sourceSetSha256,
    ]);
    expect(confirmedProcess).toMatchObject({ exitCode: 0, stderr: "" });
    expect(JSON.parse(confirmedProcess.stdout)).toMatchObject({
      status: "completed",
      freshBaseline: { status: "current", currentVersion: 1 },
      receiptPath: join(value.archiveDirectory, "source-set", "receipt.json"),
    });
    expect(JSON.parse(await readFile(
      join(value.archiveDirectory, "source-set", "receipt.json"),
      "utf8",
    ))).toMatchObject({ status: "completed" });
  });

  it("returns an archive collision as typed JSON and a non-zero exit", async () => {
    const value = await fixture();
    const preview = parseCliJson(await runSourceCli([
      "database", "archive-and-fresh",
      "--database", value.databasePath,
      "--archive", value.archiveDirectory,
    ])) as { confirmation: { sourceSetSha256: string } };
    await mkdir(value.archiveDirectory);

    const collision = await runSourceCli([
      "database", "archive-and-fresh",
      "--database", value.databasePath,
      "--archive", value.archiveDirectory,
      "--confirm-source-set", preview.confirmation.sourceSetSha256,
    ]);

    expect(collision).toMatchObject({ exitCode: 1, stderr: "" });
    expect(JSON.parse(collision.stdout)).toMatchObject({
      status: "failed",
      code: "ARCHIVE_DESTINATION_EXISTS",
      sourcePreserved: true,
    });
  });

  it("returns malformed arguments as typed JSON", async () => {
    const result = await runSourceCli([
      "database", "archive-and-fresh", "--unknown", "value",
    ]);

    expect(result).toMatchObject({ exitCode: 1, stderr: "" });
    expect(JSON.parse(result.stdout)).toMatchObject({
      status: "failed",
      code: "INVALID_ARGUMENT",
      sourcePreserved: true,
    });
  });

  it("returns typed no-op and stale-confirmation conflict results", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-cutover-cli-empty-"));
    cleanup.push(root);
    const emptyPath = join(root, "empty.sqlite3");
    const archiveDirectory = join(root, "archive");
    await writeFile(emptyPath, "");

    const noOp = await runSourceCli([
      "database", "archive-and-fresh",
      "--database", emptyPath,
      "--archive", archiveDirectory,
    ]);
    expect(noOp).toMatchObject({ exitCode: 0, stderr: "" });
    expect(JSON.parse(noOp.stdout)).toMatchObject({
      status: "no-op",
      reason: "database-empty",
    });

    const value = await fixture();
    const conflict = await runSourceCli([
      "database", "archive-and-fresh",
      "--database", value.databasePath,
      "--archive", value.archiveDirectory,
      "--confirm-source-set", `sha256:${"0".repeat(64)}`,
    ]);
    expect(conflict).toMatchObject({ exitCode: 1, stderr: "" });
    expect(JSON.parse(conflict.stdout)).toMatchObject({
      status: "conflict",
      code: "CUTOVER_CONFIRMATION_MISMATCH",
      sourcePreserved: true,
    });
  });

  it("returns hidden claim residue as named recovery JSON and a non-zero exit", async () => {
    const value = await fixture();
    const claimDirectory = join(value.root, ".fabric.sqlite3.cutover-claim-crash");
    await mkdir(claimDirectory, { mode: 0o700 });
    await rename(value.databasePath, join(claimDirectory, "fabric.sqlite3"));

    const recovery = await runSourceCli([
      "database", "archive-and-fresh",
      "--database", value.databasePath,
      "--archive", value.archiveDirectory,
    ]);

    expect(recovery).toMatchObject({ exitCode: 4, stderr: "" });
    expect(JSON.parse(recovery.stdout)).toMatchObject({
      status: "recovery-required",
      code: "CUTOVER_RESIDUE_DETECTED",
      claimDirectories: [claimDirectory],
      recovery: {
        action: expect.stringContaining("Do not start Fabric or rerun the cutover"),
      },
    });
  });
});
