import { createHash } from "node:crypto";
import {
  chmodSync,
  mkdirSync,
  readFileSync,
  renameSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import {
  copyFile,
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  readlink,
  rm,
  unlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import {
  archiveAndFreshDatabase,
  inspectDatabaseArchiveAndFresh,
  runDatabaseArchiveAndFreshCli,
} from "../../src/cli/database-archive-and-fresh.ts";
import {
  inspectFabricDatabase,
  SQLITE_SOURCE_SUFFIXES,
} from "../../src/core/migrations.ts";
import { openFabricDatabase } from "../../src/persistence/sqlite.ts";

const cleanup: string[] = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function legacyFixture(): Promise<Readonly<{
  root: string;
  databasePath: string;
  archiveDirectory: string;
}>> {
  const root = await mkdtemp(join(tmpdir(), "fabric-database-cutover-"));
  cleanup.push(root);
  const databasePath = join(root, "fabric.sqlite3");
  const database = new Database(databasePath);
  try {
    database.exec(`
      CREATE TABLE legacy_sentinel(value TEXT NOT NULL);
      INSERT INTO legacy_sentinel(value) VALUES ('preserve-me');
    `);
  } finally {
    database.close();
  }
  return {
    root,
    databasePath,
    archiveDirectory: join(root, "archive"),
  };
}

async function walFixture(): Promise<Readonly<{
  root: string;
  databasePath: string;
  archiveDirectory: string;
}>> {
  const root = await mkdtemp(join(tmpdir(), "fabric-database-cutover-wal-"));
  cleanup.push(root);
  const databasePath = join(root, "fabric.sqlite3");
  const database = new Database(databasePath);
  database.pragma("journal_mode = WAL");
  database.pragma("wal_autocheckpoint = 0");
  database.exec(`
    CREATE TABLE legacy_sentinel(value TEXT NOT NULL);
    INSERT INTO legacy_sentinel(value) VALUES ('preserve-wal-and-shm');
  `);
  const captured = new Map(SQLITE_SOURCE_SUFFIXES.flatMap((suffix) => {
    try {
      return [[suffix, readFileSync(`${databasePath}${suffix}`)] as const];
    } catch (error: unknown) {
      if (error instanceof Error && "code" in error && error.code === "ENOENT") return [];
      throw error;
    }
  }));
  database.close();
  for (const [suffix, bytes] of captured) {
    writeFileSync(`${databasePath}${suffix}`, bytes, { mode: suffix === "" ? 0o640 : 0o620 });
    chmodSync(`${databasePath}${suffix}`, suffix === "" ? 0o640 : 0o620);
  }
  return {
    root,
    databasePath,
    archiveDirectory: join(root, "archive"),
  };
}

async function walWithoutShmFixture(): Promise<Readonly<{
  root: string;
  databasePath: string;
  archiveDirectory: string;
}>> {
  const fixture = await walFixture();
  await unlink(`${fixture.databasePath}-shm`);
  return fixture;
}

async function sourceHashes(databasePath: string): Promise<Readonly<Record<string, string>>> {
  const entries = await readdir(join(databasePath, ".."));
  const hashes: Record<string, string> = {};
  for (const suffix of SQLITE_SOURCE_SUFFIXES) {
    if (!entries.includes(`fabric.sqlite3${suffix}`)) continue;
    hashes[suffix] = createHash("sha256")
      .update(await readFile(`${databasePath}${suffix}`))
      .digest("hex");
  }
  return hashes;
}

async function sourcePathSnapshot(databasePath: string): Promise<unknown> {
  return await Promise.all(SQLITE_SOURCE_SUFFIXES.map(async (suffix) => {
    const path = `${databasePath}${suffix}`;
    try {
      const metadata = await lstat(path);
      return {
        suffix,
        mode: metadata.mode,
        kind: metadata.isSymbolicLink() ? "symlink" : metadata.isFile() ? "file" : "other",
        link: metadata.isSymbolicLink() ? await readlink(path) : null,
        sha256: metadata.isFile() || metadata.isSymbolicLink()
          ? createHash("sha256").update(await readFile(path)).digest("hex")
          : null,
      };
    } catch (error: unknown) {
      if (error instanceof Error && "code" in error && error.code === "ENOENT") {
        return { suffix, absent: true };
      }
      throw error;
    }
  }));
}

describe("database archive-and-fresh cutover", () => {
  it("reports an exact mismatch and requires digest-bound confirmation without mutation", async () => {
    const fixture = await legacyFixture();
    const before = await sourceHashes(fixture.databasePath);

    const inspection = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    const result = await archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
    });

    expect(inspection).toMatchObject({
      status: "confirmation-required",
      mismatch: {
        code: "SCHEMA_CUTOVER_REQUIRED",
        fields: expect.arrayContaining([
          expect.objectContaining({ field: "epoch" }),
          expect.objectContaining({ field: "catalogSha256" }),
        ]),
      },
      confirmation: {
        sourceSetSha256: expect.stringMatching(/^sha256:[0-9a-f]{64}$/u),
      },
    });
    expect(result).toEqual(inspection);
    expect(await sourceHashes(fixture.databasePath)).toEqual(before);
    await expect(readdir(fixture.archiveDirectory)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("publishes a mode-preserving receipt archive before initialising a fresh current baseline", async () => {
    const fixture = await legacyFixture();
    const beforeBytes = await readFile(fixture.databasePath);
    const beforeMode = (await lstat(fixture.databasePath)).mode & 0o777;
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    expect(preview.status).toBe("confirmation-required");
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");

    const result = await archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    });

    expect(result).toMatchObject({
      status: "completed",
      confirmation: {
        sourceSetSha256: preview.confirmation.sourceSetSha256,
        confirmed: true,
      },
      freshBaseline: { status: "current" },
    });
    expect(inspectFabricDatabase(fixture.databasePath)).toEqual({ state: "current" });
    const archivedPath = join(fixture.archiveDirectory, "source-set", "fabric.sqlite3");
    expect(await readFile(archivedPath)).toEqual(beforeBytes);
    expect((await lstat(archivedPath)).mode & 0o777).toBe(beforeMode);
    const receiptText = await readFile(
      join(fixture.archiveDirectory, "source-set", "receipt.json"),
      "utf8",
    );
    const receipt = JSON.parse(receiptText) as Record<string, unknown>;
    expect(receipt).toMatchObject({
      schemaVersion: 1,
      kind: "agent-fabric-database-archive-and-fresh-receipt",
      status: "completed",
      source: {
        members: [{
          suffix: "",
          archiveName: "fabric.sqlite3",
          identity: {
            mode: expect.any(String),
            ino: expect.any(String),
            sha256: createHash("sha256").update(beforeBytes).digest("hex"),
          },
        }],
      },
    });
    expect(receiptText).not.toMatch(/capability|credential|bearer|token/iu);
  });

  it("archives an existing SHM as custody evidence with the WAL and preserves both modes", async () => {
    const fixture = await walFixture();
    const before = await sourceHashes(fixture.databasePath);
    expect(Object.keys(before)).toEqual(["", "-wal", "-shm"]);
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");

    const result = await archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    });

    expect(result.status).toBe("completed");
    for (const suffix of ["-wal", "-shm"] as const) {
      const archivedPath = join(fixture.archiveDirectory, "source-set", `fabric.sqlite3${suffix}`);
      expect(createHash("sha256").update(await readFile(archivedPath)).digest("hex")).toBe(before[suffix]);
      expect((await lstat(archivedPath)).mode & 0o777).toBe(0o620);
    }
  });

  it("cuts over a readable WAL without SHM and preserves its rows in the archive", async () => {
    const fixture = await walWithoutShmFixture();
    const before = await sourceHashes(fixture.databasePath);
    expect(Object.keys(before)).toEqual(["", "-wal"]);
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    expect(preview).toMatchObject({
      status: "confirmation-required",
      source: { members: [{ suffix: "" }, { suffix: "-wal" }] },
    });
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");

    const result = archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    });

    expect(result.status).toBe("completed");
    const verificationDirectory = join(fixture.root, "archive-verification");
    await mkdir(verificationDirectory);
    const verificationPath = join(verificationDirectory, "fabric.sqlite3");
    await copyFile(
      join(fixture.archiveDirectory, "source-set", "fabric.sqlite3"),
      verificationPath,
    );
    await copyFile(
      join(fixture.archiveDirectory, "source-set", "fabric.sqlite3-wal"),
      `${verificationPath}-wal`,
    );
    const archivedDatabase = new Database(verificationPath);
    try {
      expect(archivedDatabase.prepare("SELECT value FROM legacy_sentinel").all()).toEqual([
        { value: "preserve-wal-and-shm" },
      ]);
    } finally {
      archivedDatabase.close();
    }
    expect(await sourceHashes(join(fixture.archiveDirectory, "source-set", "fabric.sqlite3")))
      .toEqual(before);
  });

  it("reports the computed catalogue digest when current metadata masks catalogue drift", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-cutover-catalogue-"));
    cleanup.push(root);
    const databasePath = join(root, "fabric.sqlite3");
    openFabricDatabase(databasePath).close();
    const database = new Database(databasePath);
    database.exec("DROP INDEX tasks_by_state");
    database.close();
    const before = await sourceHashes(databasePath);

    const result = inspectDatabaseArchiveAndFresh(databasePath);

    expect(result).toMatchObject({
      status: "confirmation-required",
      mismatch: {
        fields: expect.arrayContaining([{
          field: "catalogSha256",
          expected: expect.stringMatching(/^[0-9a-f]{64}$/u),
          actual: expect.stringMatching(/^[0-9a-f]{64}$/u),
        }]),
      },
    });
    if (result.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const catalogue = result.mismatch.fields.find(({ field }) => field === "catalogSha256");
    expect(catalogue?.actual).not.toBe(catalogue?.expected);
    expect(await sourceHashes(databasePath)).toEqual(before);
  });

  it("reports malformed schema fields without erasing other observed values", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-cutover-malformed-metadata-"));
    cleanup.push(root);
    const databasePath = join(root, "fabric.sqlite3");
    const database = new Database(databasePath);
    database.exec(`
      CREATE TABLE fabric_schema(
        epoch,
        baseline_sha256 TEXT,
        catalog_sha256 TEXT
      );
      INSERT INTO fabric_schema(epoch, baseline_sha256, catalog_sha256)
      VALUES (x'07', 'observed-baseline', 'observed-catalog')
    `);
    database.close();

    const result = inspectDatabaseArchiveAndFresh(databasePath);

    expect(result).toMatchObject({
      status: "confirmation-required",
      mismatch: {
        fields: expect.arrayContaining([
          { field: "epoch", expected: expect.any(String), actual: "blob:07" },
          { field: "baselineSha256", expected: expect.any(String), actual: "observed-baseline" },
          { field: "recordedCatalogSha256", expected: expect.any(String), actual: "observed-catalog" },
        ]),
      },
    });
  });

  it("never persists arbitrary schema-row values in the archive receipt", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-cutover-secret-row-"));
    cleanup.push(root);
    const databasePath = join(root, "fabric.sqlite3");
    const archiveDirectory = join(root, "archive");
    const database = new Database(databasePath);
    database.exec(`
      CREATE TABLE fabric_schema(
        epoch TEXT,
        baseline_sha256 TEXT,
        catalog_sha256 TEXT
      );
      INSERT INTO fabric_schema(epoch, baseline_sha256, catalog_sha256)
      VALUES ('Bearer review-secret-token', 'credential-like-value', 'observed-catalog')
    `);
    database.close();
    const preview = inspectDatabaseArchiveAndFresh(databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");

    const result = archiveAndFreshDatabase({
      databasePath,
      archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    });

    expect(result.status).toBe("completed");
    const receiptText = await readFile(join(archiveDirectory, "source-set", "receipt.json"), "utf8");
    expect(receiptText).not.toContain("review-secret-token");
    expect(receiptText).not.toContain("credential-like-value");
    expect(JSON.parse(receiptText)).toMatchObject({
      mismatch: {
        code: "SCHEMA_CUTOVER_REQUIRED",
        fields: expect.arrayContaining([{ field: "epoch" }, { field: "baselineSha256" }]),
      },
    });
  });

  it("accepts a rollback-journal-only source shape without synthesising other sidecars", async () => {
    const fixture = await legacyFixture();
    await writeFile(`${fixture.databasePath}-journal`, "", { mode: 0o620 });
    const before = await sourceHashes(fixture.databasePath);

    const result = inspectDatabaseArchiveAndFresh(fixture.databasePath);

    expect(result).toMatchObject({
      status: "confirmation-required",
      source: {
        members: [
          { suffix: "" },
          { suffix: "-journal" },
        ],
      },
    });
    expect(await sourceHashes(fixture.databasePath)).toEqual(before);
  });

  it("rejects an existing archive destination without changing any source hash", async () => {
    const fixture = await legacyFixture();
    await writeFile(join(fixture.root, "archive-sentinel"), "unrelated\n");
    await mkdir(fixture.archiveDirectory);
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const before = await sourceHashes(fixture.databasePath);

    expect(() => archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    })).toThrowError(expect.objectContaining({ code: "EEXIST" }));

    expect(await sourceHashes(fixture.databasePath)).toEqual(before);
    expect(await readdir(fixture.archiveDirectory)).toEqual([]);
  });

  it.each(["-wal", "-shm", "-journal"] as const)(
    "rejects an archive beneath absent source member %s without changing its path set",
    async (suffix) => {
      const fixture = await legacyFixture();
      const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
      if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
      const before = await sourcePathSnapshot(fixture.databasePath);

      expect(() => archiveAndFreshDatabase({
        databasePath: fixture.databasePath,
        archiveDirectory: join(`${fixture.databasePath}${suffix}`, "archive"),
        confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
      })).toThrow("archive destination must not equal or descend from database source member");

      expect(await sourcePathSnapshot(fixture.databasePath)).toEqual(before);
    },
  );

  it("resolves a symlinked archive ancestor before checking source-path disjointness", async () => {
    const fixture = await legacyFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const alias = join(fixture.root, "alias");
    symlinkSync(fixture.root, alias);
    const before = await sourcePathSnapshot(fixture.databasePath);

    expect(() => archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: join(alias, "fabric.sqlite3-wal", "archive"),
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    })).toThrow("archive destination must not equal or descend from database source member");

    expect(await sourcePathSnapshot(fixture.databasePath)).toEqual(before);
  });

  it("detects a sidecar race and preserves the race-winner source hashes", async () => {
    const fixture = await legacyFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const mainHash = (await sourceHashes(fixture.databasePath))[""];
    let racedHashes: Readonly<Record<string, string>> | undefined;

    expect(() => archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    }, {
      afterInspection: () => {
        writeFileSync(`${fixture.databasePath}-wal`, "racing sidecar\n", { mode: 0o640 });
        racedHashes = {
          "": mainHash ?? createHash("sha256").update(readFileSync(fixture.databasePath)).digest("hex"),
          "-wal": createHash("sha256").update("racing sidecar\n").digest("hex"),
        };
      },
    })).toThrowError(expect.objectContaining({
      code: "SCHEMA_CUTOVER_REQUIRED",
      preserved: true,
    }));

    expect(await sourceHashes(fixture.databasePath)).toEqual(racedHashes);
    await expect(readdir(fixture.archiveDirectory)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("preserves and warns against overwriting a race winner after archive publication", async () => {
    const fixture = await legacyFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const beforeMain = (await sourceHashes(fixture.databasePath))[""];
    let racedHashes: Readonly<Record<string, string>> = {};

    const result = archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    }, {
      beforeSourceClaim: () => {
        writeFileSync(`${fixture.databasePath}-wal`, "late race winner\n", { mode: 0o640 });
        racedHashes = {
          "": beforeMain ?? createHash("sha256").update(readFileSync(fixture.databasePath)).digest("hex"),
          "-wal": createHash("sha256").update("late race winner\n").digest("hex"),
        };
      },
    });

    expect(result).toMatchObject({
      status: "archive-complete-cutover-failed",
      code: "SOURCE_DISPLACEMENT_FAILED",
      recovery: {
        boundary: "archive-complete",
        action: expect.stringContaining("Do not start Fabric or overwrite any live source path"),
      },
    });
    expect(await sourceHashes(fixture.databasePath)).toEqual(racedHashes);
    expect(createHash("sha256").update(await readFile(
      join(fixture.archiveDirectory, "source-set", "fabric.sqlite3"),
    )).digest("hex")).toBe(beforeMain);
  });

  it("reports claim and archive staging crash residue instead of a clean no-op", async () => {
    const fixture = await legacyFixture();
    const claimDirectory = join(fixture.root, ".fabric.sqlite3.cutover-claim-crash");
    const claimedDatabasePath = join(claimDirectory, "fabric.sqlite3");
    await mkdir(claimDirectory, { mode: 0o700 });
    renameSync(fixture.databasePath, claimedDatabasePath);
    const stagingDirectory = join(fixture.root, ".archive.staging-crash");
    await mkdir(stagingDirectory, { mode: 0o700 });
    await writeFile(join(stagingDirectory, "fabric.sqlite3"), "partial archive\n");
    const beforeClaim = createHash("sha256").update(await readFile(claimedDatabasePath)).digest("hex");

    const result = archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
    });

    expect(result).toEqual({
      schemaVersion: 1,
      kind: "agent-fabric-database-archive-and-fresh",
      status: "recovery-required",
      code: "CUTOVER_RESIDUE_DETECTED",
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      claimDirectories: [claimDirectory],
      stagingDirectories: [stagingDirectory],
      sourcePreserved: true,
      recovery: {
        action: expect.stringContaining("Do not start Fabric or rerun the cutover"),
      },
    });
    await expect(readFile(fixture.databasePath)).rejects.toMatchObject({ code: "ENOENT" });
    expect(createHash("sha256").update(await readFile(claimedDatabasePath)).digest("hex"))
      .toBe(beforeClaim);
    expect(await readFile(join(stagingDirectory, "fabric.sqlite3"), "utf8"))
      .toBe("partial archive\n");
  });

  it("previews a live legacy database while another cutover holds a claim directory", async () => {
    const fixture = await legacyFixture();
    // A cutover in flight owns a claim directory that is indistinguishable on
    // disk from crash residue. It must not turn this invocation's ordinary
    // preview into "do not start Fabric": the live source set is still here, so
    // the claim race -- not residue recovery -- is what governs this path.
    const claimDirectory = join(fixture.root, ".fabric.sqlite3.cutover-claim-inflight");
    await mkdir(claimDirectory, { mode: 0o700 });
    const before = await sourceHashes(fixture.databasePath);

    const result = archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
    });

    expect(result).toMatchObject({ status: "confirmation-required" });
    expect(await sourceHashes(fixture.databasePath)).toEqual(before);
  });

  it.each(SQLITE_SOURCE_SUFFIXES)(
    "rejects a symbolic link at source suffix %j without changing the source paths",
    async (suffix) => {
      const fixture = await legacyFixture();
      if (suffix === "") {
        const target = join(fixture.root, "legacy-target.sqlite3");
        renameSync(fixture.databasePath, target);
        symlinkSync(target, fixture.databasePath);
      } else {
        const target = join(fixture.root, `sidecar-target${suffix}`);
        writeFileSync(target, `target ${suffix}\n`, { mode: 0o640 });
        symlinkSync(target, `${fixture.databasePath}${suffix}`);
      }
      const before = await sourcePathSnapshot(fixture.databasePath);

      expect(() => inspectDatabaseArchiveAndFresh(fixture.databasePath)).toThrowError(
        expect.objectContaining({ code: "SCHEMA_CUTOVER_REQUIRED", preserved: true }),
      );

      expect(await sourcePathSnapshot(fixture.databasePath)).toEqual(before);
    },
  );

  it.each([
    { suffixes: ["-shm"] },
    { suffixes: ["-wal", "-journal"] },
    { suffixes: ["-shm", "-journal"] },
    { suffixes: ["-wal", "-shm", "-journal"] },
  ] as const)(
    "rejects partial or mixed sidecars $suffixes without changing any source hash",
    async ({ suffixes }) => {
      const fixture = await legacyFixture();
      for (const suffix of suffixes) {
        await writeFile(`${fixture.databasePath}${suffix}`, `sidecar ${suffix}\n`, { mode: 0o640 });
      }
      const before = await sourceHashes(fixture.databasePath);

      const result = inspectDatabaseArchiveAndFresh(fixture.databasePath);

      expect(result).toMatchObject({
        status: "conflict",
        code: "PARTIAL_SQLITE_SOURCE_SET",
        sourcePreserved: true,
      });
      expect(await sourceHashes(fixture.databasePath)).toEqual(before);
    },
  );

  it("cleans an empty claim directory and accurately reports an ordinary claim race loss", async () => {
    const fixture = await legacyFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const competingClaimDirectory = join(
      fixture.root,
      ".fabric.sqlite3.cutover-claim-competing-winner",
    );

    const result = archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    }, {
      claimSourceFile: (sourcePath) => {
        mkdirSync(competingClaimDirectory, { mode: 0o700 });
        renameSync(sourcePath, join(competingClaimDirectory, "fabric.sqlite3"));
        const error = new Error("source disappeared before claim");
        Object.assign(error, { code: "ENOENT" });
        throw error;
      },
    });

    expect(result).toMatchObject({
      status: "archive-complete-cutover-failed",
      code: "SOURCE_DISPLACEMENT_FAILED",
      claimDirectory: expect.stringMatching(/\.fabric\.sqlite3\.cutover-claim-/u),
      claimDirectoryDisposition: "removed-empty",
      message: expect.stringContaining(
        "before any source files were claimed; another cutover may have claimed the source set first",
      ),
    });
    if (
      result.status !== "archive-complete-cutover-failed" ||
      !("claimDirectory" in result) ||
      result.claimDirectory === undefined
    ) throw new TypeError("empty claim race omitted its claim directory");
    await expect(readdir(result.claimDirectory)).rejects.toMatchObject({ code: "ENOENT" });
    expect(await readFile(join(competingClaimDirectory, "fabric.sqlite3")))
      .toEqual(await readFile(join(fixture.archiveDirectory, "source-set", "fabric.sqlite3")));
  });

  it("reports and removes its empty claim directory when private setup fails", async () => {
    const fixture = await legacyFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");

    const result = archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    }, {
      prepareClaimDirectory: () => {
        throw new Error("injected private claim setup failure");
      },
    });

    expect(result).toMatchObject({
      status: "archive-complete-cutover-failed",
      code: "SOURCE_DISPLACEMENT_FAILED",
      claimDirectory: expect.stringMatching(/\.fabric\.sqlite3\.cutover-claim-/u),
      claimDirectoryDisposition: "removed-empty",
      message: expect.stringContaining("injected private claim setup failure"),
    });
    if (
      result.status !== "archive-complete-cutover-failed" ||
      !("claimDirectory" in result) ||
      result.claimDirectory === undefined
    ) throw new TypeError("private claim setup failure omitted its claim directory");
    await expect(readdir(result.claimDirectory)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("preserves and reports the claim directory when claim-state classification fails", async () => {
    const fixture = await legacyFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const before = await readFile(fixture.databasePath);

    const result = archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    }, {
      claimSourceFile: (sourcePath, claimedPath) => {
        renameSync(sourcePath, claimedPath);
        throw new Error("injected source claim failure");
      },
      claimedSourceExists: () => {
        throw new Error("injected claim-state classification failure");
      },
    });

    expect(result).toMatchObject({
      status: "archive-complete-cutover-failed",
      code: "SOURCE_DISPLACEMENT_FAILED",
      claimDirectory: expect.stringMatching(/\.fabric\.sqlite3\.cutover-claim-/u),
      claimDirectoryDisposition: "preserved",
      message: expect.stringContaining("claim-state classification also failed"),
    });
    if (
      result.status !== "archive-complete-cutover-failed" ||
      !("claimDirectory" in result) ||
      result.claimDirectory === undefined
    ) throw new TypeError("claim-state classification failure omitted its claim directory");
    await expect(readFile(fixture.databasePath)).rejects.toMatchObject({ code: "ENOENT" });
    expect(await readFile(join(result.claimDirectory, "fabric.sqlite3"))).toEqual(before);
  });

  it("cleans a midway archive-write failure without changing any source hash", async () => {
    const fixture = await legacyFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const before = await sourceHashes(fixture.databasePath);
    let writes = 0;

    expect(() => archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    }, {
      writeArchiveFile: (path, bytes, mode) => {
        writes += 1;
        if (writes === 2) throw new Error("injected archive write failure");
        writeFileSync(path, bytes, { flag: "wx", mode });
        chmodSync(path, mode);
      },
    })).toThrow("injected archive write failure");

    expect(await sourceHashes(fixture.databasePath)).toEqual(before);
    await expect(readdir(fixture.archiveDirectory)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("rolls back a partial exact-source claim before archive publication", async () => {
    const fixture = await walFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const before = await sourceHashes(fixture.databasePath);
    let claims = 0;

    const result = archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    }, {
      claimSourceFile: (sourcePath, claimedPath) => {
        claims += 1;
        if (claims === 2) throw new Error("injected source claim failure");
        renameSync(sourcePath, claimedPath);
      },
    });

    expect(result).toMatchObject({
      status: "archive-complete-cutover-failed",
      code: "SOURCE_DISPLACEMENT_FAILED",
      freshBaseline: { status: "pending" },
      claimDirectory: expect.stringMatching(/\.fabric\.sqlite3\.cutover-claim-/u),
      claimDirectoryDisposition: "removed-after-rollback",
      recovery: { boundary: "archive-complete" },
    });
    if (
      result.status !== "archive-complete-cutover-failed" ||
      !("claimDirectory" in result) ||
      result.claimDirectory === undefined
    ) throw new TypeError("partial claim rollback omitted its claim directory");
    await expect(readdir(result.claimDirectory)).rejects.toMatchObject({ code: "ENOENT" });
    expect(await sourceHashes(fixture.databasePath)).toEqual(before);
    expect(createHash("sha256").update(await readFile(
      join(fixture.archiveDirectory, "source-set", "fabric.sqlite3"),
    )).digest("hex")).toBe(before[""]);
  });

  it("never leaves the original sidecars beside a database it did not restore", async () => {
    // A WAL is bound to its database by filename alone, so linking the original
    // -wal beside a database created by whichever process won the race hands
    // SQLite a foreign WAL to apply. `openFabricDatabase` auto-initialises when
    // the file is absent, so that racing process is typically Fabric itself,
    // which would then replace the new schema at its next open. Rollback must
    // fail before its first link rather than part-way through.
    const fixture = await walFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const before = await sourceHashes(fixture.databasePath);
    let claims = 0;

    const result = archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    }, {
      claimSourceFile: (sourcePath, claimedPath) => {
        renameSync(sourcePath, claimedPath);
        claims += 1;
        // Occupy the freed main path the instant it is claimed, exactly as an
        // auto-initialising open would.
        if (claims === 1) {
          writeFileSync(fixture.databasePath, "intruder database\n", { mode: 0o640 });
        }
      },
    });

    expect(result).toMatchObject({
      status: "archive-complete-cutover-failed",
      code: "SOURCE_DISPLACEMENT_FAILED",
      claimDirectory: expect.stringMatching(/\.fabric\.sqlite3\.cutover-claim-/u),
      claimDirectoryDisposition: "preserved",
    });
    if (
      result.status !== "archive-complete-cutover-failed" ||
      !("claimDirectory" in result)
    ) throw new TypeError("failed source displacement omitted its claim directory");
    const after = await sourceHashes(fixture.databasePath);
    // The intruder's file is untouched and, critically, stands alone: no
    // original sidecar was linked next to it.
    expect(after[""]).toBe(createHash("sha256").update("intruder database\n").digest("hex"));
    expect(Object.keys(after)).toStrictEqual([""]);
    // The complete original set is still recoverable, and the archive is intact.
    expect(createHash("sha256").update(await readFile(
      join(fixture.archiveDirectory, "source-set", "fabric.sqlite3"),
    )).digest("hex")).toBe(before[""]);
    expect(await sourceHashes(join(result.claimDirectory, "fabric.sqlite3"))).toEqual(before);
  });

  it("retains a complete hash-identical archive when fresh initialisation fails", async () => {
    const fixture = await legacyFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const before = await sourceHashes(fixture.databasePath);

    const result = await archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    }, {
      initialiseFreshDatabase: () => {
        throw new Error("injected fresh baseline failure");
      },
    });

    expect(result).toMatchObject({
      status: "archive-complete-fresh-init-failed",
      code: "FRESH_BASELINE_INITIALISATION_FAILED",
      freshBaseline: { status: "failed", error: "injected fresh baseline failure" },
      recovery: {
        boundary: "archive-complete",
        action: expect.stringContaining("restore every member"),
      },
    });
    await expect(readFile(fixture.databasePath)).rejects.toMatchObject({ code: "ENOENT" });
    const archivedHashes: Record<string, string> = {};
    for (const suffix of Object.keys(before)) {
      archivedHashes[suffix] = createHash("sha256").update(await readFile(
        join(fixture.archiveDirectory, "source-set", `fabric.sqlite3${suffix}`),
      )).digest("hex");
    }
    expect(archivedHashes).toEqual(before);
  });

  it("returns an archive-complete result if claimed-source cleanup fails", async () => {
    const fixture = await legacyFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");
    const before = await sourceHashes(fixture.databasePath);

    const result = archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    }, {
      removeClaimedSource: () => {
        throw new Error("injected claimed-source cleanup failure");
      },
    });

    expect(result).toMatchObject({
      status: "archive-complete-cutover-failed",
      code: "SOURCE_DISPLACEMENT_CLEANUP_FAILED",
      claimDirectory: expect.stringMatching(/\.fabric\.sqlite3\.cutover-claim-/u),
      claimDirectoryDisposition: "preserved",
      freshBaseline: { status: "pending" },
      recovery: { boundary: "archive-complete" },
    });
    if (
      result.status !== "archive-complete-cutover-failed" ||
      !("claimDirectory" in result)
    ) throw new TypeError("claimed-source cleanup failure omitted its claim directory");
    await expect(readFile(fixture.databasePath)).rejects.toMatchObject({ code: "ENOENT" });
    expect(createHash("sha256").update(await readFile(
      join(fixture.archiveDirectory, "source-set", "fabric.sqlite3"),
    )).digest("hex")).toBe(before[""]);
    expect(await sourceHashes(join(result.claimDirectory, "fabric.sqlite3"))).toEqual(before);
  });

  it("never reports completion if its durable completion receipt cannot be finalised", async () => {
    const fixture = await legacyFixture();
    const preview = inspectDatabaseArchiveAndFresh(fixture.databasePath);
    if (preview.status !== "confirmation-required") throw new TypeError("expected confirmation preview");

    const result = archiveAndFreshDatabase({
      databasePath: fixture.databasePath,
      archiveDirectory: fixture.archiveDirectory,
      confirmSourceSetSha256: preview.confirmation.sourceSetSha256,
    }, {
      finaliseReceipt: () => {
        throw new Error("injected receipt finalisation failure");
      },
    });

    expect(result).toMatchObject({
      status: "archive-complete-cutover-failed",
      code: "RECEIPT_FINALISATION_FAILED",
      freshBaseline: { status: "current" },
      recovery: { boundary: "archive-complete" },
    });
    expect(inspectFabricDatabase(fixture.databasePath)).toEqual({ state: "current" });
    const receipt = JSON.parse(await readFile(
      join(fixture.archiveDirectory, "source-set", "receipt.json"),
      "utf8",
    )) as Record<string, unknown>;
    expect(receipt).toMatchObject({
      status: "archive-complete",
      freshBaseline: { status: "pending" },
    });
  });

  it("returns machine-readable invalid arguments instead of throwing", () => {
    expect(runDatabaseArchiveAndFreshCli(
      ["--unknown", "value"],
      "/absolute/fabric.sqlite3",
    )).toMatchObject({
      exitCode: 1,
      result: {
        status: "failed",
        code: "INVALID_ARGUMENT",
        sourcePreserved: true,
      },
    });
  });

  it.each(["empty", "current"] as const)(
    "returns a typed no-op for an %s database without creating an archive",
    async (state) => {
      const root = await mkdtemp(join(tmpdir(), `fabric-cutover-${state}-`));
      cleanup.push(root);
      const databasePath = join(root, "fabric.sqlite3");
      const archiveDirectory = join(root, "archive");
      if (state === "empty") {
        await writeFile(databasePath, "", { mode: 0o640 });
      } else {
        openFabricDatabase(databasePath).close();
      }
      const before = await sourceHashes(databasePath);

      const result = await archiveAndFreshDatabase({
        databasePath,
        archiveDirectory,
        confirmSourceSetSha256: `sha256:${"0".repeat(64)}`,
      });

      expect(result).toEqual({
        schemaVersion: 1,
        kind: "agent-fabric-database-archive-and-fresh",
        status: "no-op",
        reason: `database-${state}`,
        databasePath,
      });
      expect(await sourceHashes(databasePath)).toEqual(before);
      await expect(readdir(archiveDirectory)).rejects.toMatchObject({ code: "ENOENT" });
    },
  );
});
