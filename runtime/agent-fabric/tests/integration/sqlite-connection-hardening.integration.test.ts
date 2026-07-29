import Database from "better-sqlite3";
import { createHash } from "node:crypto";
import { lstat, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { openFabricDatabase } from "../../src/persistence/sqlite.ts";
import {
  inspectFabricDatabase,
  inspectFabricDatabaseForCutover,
  withPrivateDatabaseClone,
} from "../../src/core/migrations.ts";

const directories: string[] = [];

async function expectExactFileBytes(path: string, expected: Buffer): Promise<void> {
  const actual = await readFile(path);
  expect(actual.byteLength).toBe(expected.byteLength);
  expect(actual.equals(expected)).toBe(true);
}

async function preservationSnapshot(directory: string): Promise<unknown> {
  const directoryStat = await lstat(directory);
  const entries = await Promise.all((await readdir(directory)).sort().map(async (name) => {
    const path = join(directory, name);
    const metadata = await lstat(path);
    return {
      name,
      kind: metadata.isFile() ? "file" : metadata.isDirectory() ? "directory" : "other",
      mode: metadata.mode,
      uid: metadata.uid,
      gid: metadata.gid,
      nlink: metadata.nlink,
      size: metadata.size,
      mtimeMs: metadata.mtimeMs,
      ctimeMs: metadata.ctimeMs,
      digest: metadata.isFile()
        ? createHash("sha256").update(await readFile(path)).digest("hex")
        : null,
    };
  }));
  return {
    directory: {
      mode: directoryStat.mode,
      uid: directoryStat.uid,
      gid: directoryStat.gid,
      nlink: directoryStat.nlink,
      mtimeMs: directoryStat.mtimeMs,
      ctimeMs: directoryStat.ctimeMs,
    },
    entries,
  };
}

afterEach(async () => {
  await Promise.all(directories.splice(0).map((directory) => rm(directory, { recursive: true, force: true })));
});

describe("SQLite connection hardening", () => {
  it("rejects a pre-existing empty file as incompatible without mutation", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-cutover-invalid-"));
    directories.push(directory);
    const path = join(directory, "fabric.sqlite3");
    await writeFile(path, "", { mode: 0o640 });
    const beforeBytes = await readFile(path);
    const beforeStat = await stat(path);
    const beforeEntries = await readdir(directory);

    expect(() => openFabricDatabase(path)).toThrowError(
      expect.objectContaining({ code: "SCHEMA_CUTOVER_REQUIRED", preserved: true }),
    );

    await expectExactFileBytes(path, beforeBytes);
    expect((await stat(path)).mode).toBe(beforeStat.mode);
    expect((await stat(path)).mtimeMs).toBe(beforeStat.mtimeMs);
    expect(await readdir(directory)).toEqual(beforeEntries);
  });

  it("propagates a pre-existing non-SQLite format failure without mutation", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-format-invalid-"));
    directories.push(directory);
    const path = join(directory, "fabric.sqlite3");
    await writeFile(path, "not a SQLite database\n", { mode: 0o640 });
    const beforeBytes = await readFile(path);
    const beforeStat = await stat(path);
    const beforeEntries = await readdir(directory);

    expect(() => openFabricDatabase(path)).toThrowError(
      expect.objectContaining({ code: "SQLITE_NOTADB" }),
    );

    await expectExactFileBytes(path, beforeBytes);
    expect((await stat(path)).mode).toBe(beforeStat.mode);
    expect((await stat(path)).mtimeMs).toBe(beforeStat.mtimeMs);
    expect(await readdir(directory)).toEqual(beforeEntries);
  });

  it("rejects an older schema before mutating its database or filesystem evidence", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-cutover-"));
    directories.push(directory);
    const path = join(directory, "fabric.sqlite3");
    const legacy = new Database(path);
    legacy.exec(`
      CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
      INSERT INTO schema_migrations(version, applied_at) VALUES (14, '2026-07-12T00:00:00Z');
      CREATE TABLE legacy_sentinel(value TEXT NOT NULL);
      INSERT INTO legacy_sentinel(value) VALUES ('preserve-me');
    `);
    legacy.close();
    const beforeBytes = await readFile(path);
    const beforeStat = await stat(path);
    const beforeEntries = await readdir(directory);

    expect(() => openFabricDatabase(path)).toThrowError(
      expect.objectContaining({ code: "SCHEMA_CUTOVER_REQUIRED", preserved: true }),
    );

    await expectExactFileBytes(path, beforeBytes);
    expect((await stat(path)).mode).toBe(beforeStat.mode);
    expect((await stat(path)).mtimeMs).toBe(beforeStat.mtimeMs);
    expect(await readdir(directory)).toEqual(beforeEntries);
  });

  it("reapplies connection-local pragmas on every reopen", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-sqlite-"));
    directories.push(directory);
    const path = join(directory, "fabric.sqlite3");
    for (let attempt = 0; attempt < 2; attempt += 1) {
      const database = openFabricDatabase(path);
      expect(database.pragma("foreign_keys", { simple: true })).toBe(1);
      expect(database.pragma("trusted_schema", { simple: true })).toBe(0);
      expect(database.pragma("busy_timeout", { simple: true })).toBe(5_000);
      expect(database.pragma("journal_mode", { simple: true })).toBe("wal");
      expect(database.prepare(`
        SELECT fabric_topology_plan_digest(?) AS digest
      `).get('{"a":1,"planDigest":"omitted"}')).toEqual({
        digest: `sha256:${createHash("sha256").update('{"a":1}').digest("hex")}`,
      });
      expect((await lstat(path)).mode & 0o777).toBe(0o600);
      for (const sibling of [`${path}-wal`, `${path}-shm`]) {
        await expect(lstat(sibling).then((metadata) => metadata.mode & 0o777)).resolves.toBe(0o600);
      }
      database.close();
      await expect(lstat(`${path}.unclean`)).rejects.toMatchObject({ code: "ENOENT" });
    }
  });

  it("rejects a tampered current catalog without mutation", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-cutover-tampered-"));
    directories.push(directory);
    const path = join(directory, "fabric.sqlite3");
    openFabricDatabase(path).close();
    const tamper = new Database(path);
    tamper.exec("DROP INDEX tasks_by_state");
    tamper.close();
    const beforeBytes = await readFile(path);
    const beforeStat = await stat(path);
    const beforeEntries = await readdir(directory);

    expect(() => openFabricDatabase(path)).toThrowError(
      expect.objectContaining({ code: "SCHEMA_CUTOVER_REQUIRED", preserved: true }),
    );

    await expectExactFileBytes(path, beforeBytes);
    expect((await stat(path)).mode).toBe(beforeStat.mode);
    expect((await stat(path)).mtimeMs).toBe(beforeStat.mtimeMs);
    expect(await readdir(directory)).toEqual(beforeEntries);
  }, 5_000);

  it("detects catalog drift committed only to WAL through a private clone without touching the source tree", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-cutover-wal-"));
    directories.push(directory);
    const path = join(directory, "fabric.sqlite3");
    openFabricDatabase(path).close();
    const writer = new Database(path);
    writer.pragma("journal_mode = WAL");
    writer.pragma("wal_autocheckpoint = 0");
    writer.pragma("wal_checkpoint(TRUNCATE)");
    const checkpointedMain = await readFile(path);
    writer.exec("DROP INDEX tasks_by_state");
    await expectExactFileBytes(path, checkpointedMain);
    expect((await stat(`${path}-wal`)).size).toBeGreaterThan(0);
    await writeFile(join(directory, "custody-marker"), "preserve every entry\n", { mode: 0o640 });
    const before = await preservationSnapshot(directory);

    expect(() => inspectFabricDatabase(path)).toThrowError(
      expect.objectContaining({ code: "SCHEMA_CUTOVER_REQUIRED", preserved: true }),
    );

    expect(await preservationSnapshot(directory)).toEqual(before);
    writer.close();
  });

  it("retries a concurrent source write without reporting a schema cutover", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-inspection-retry-"));
    directories.push(directory);
    const path = join(directory, "fabric.sqlite3");
    openFabricDatabase(path).close();
    let inspections = 0;

    const result = withPrivateDatabaseClone(path, () => {
      inspections += 1;
      if (inspections === 1) {
        const writer = new Database(path);
        writer.pragma("user_version = 1");
        writer.close();
      }
      return "current";
    });

    expect(result).toBe("current");
    expect(inspections).toBe(2);
  });

  it("surfaces bounded inspection instability without cutover preservation semantics", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-inspection-unstable-"));
    directories.push(directory);
    const path = join(directory, "fabric.sqlite3");
    openFabricDatabase(path).close();
    let inspections = 0;

    expect(() => withPrivateDatabaseClone(path, () => {
      inspections += 1;
      const writer = new Database(path);
      writer.pragma(`user_version = ${String(inspections)}`);
      writer.close();
    })).toThrowError(expect.objectContaining({
      code: "DATABASE_INSPECTION_UNSTABLE",
      preserved: false,
    }));
    expect(inspections).toBe(3);
  });

  it("propagates a stable database format failure instead of inventing a schema mismatch", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-inspection-format-"));
    directories.push(directory);
    const path = join(directory, "fabric.sqlite3");
    await writeFile(path, "not a SQLite database\n", { mode: 0o600 });

    expect(() => inspectFabricDatabaseForCutover(path)).toThrowError(
      expect.objectContaining({ code: "SQLITE_NOTADB" }),
    );
  });

  it("runs bounded integrity checks after an unclean marker", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-sqlite-"));
    directories.push(directory);
    const path = join(directory, "fabric.sqlite3");
    openFabricDatabase(path).close();
    const raw = new Database(path);
    raw.pragma("foreign_keys = OFF");
    raw.prepare("INSERT INTO authorities VALUES ('orphan','missing-run',NULL,'{}','hash',1)").run();
    raw.close();
    await writeFile(`${path}.unclean`, "", { mode: 0o600 });

    expect(() => openFabricDatabase(path)).toThrowError(
      expect.objectContaining({ code: "PERSISTENCE_FOREIGN_KEY_CHECK_FAILED" }),
    );
  });
});
