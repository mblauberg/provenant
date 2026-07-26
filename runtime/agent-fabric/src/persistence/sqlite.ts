import { randomBytes } from "node:crypto";
import Database from "better-sqlite3";
import {
  chmodSync,
  closeSync,
  constants,
  existsSync,
  fsyncSync,
  linkSync,
  openSync,
  rmSync,
  unlinkSync,
} from "node:fs";
import { dirname } from "node:path";

import {
  applyMigrations,
  assertCurrentSchema,
  inspectFabricDatabase,
  registerFabricSqlFunctions,
  SQLITE_SOURCE_SUFFIXES,
} from "../core/migrations.js";
import {
  assertDatabaseIntegrity,
  prepareUncleanMarker,
  removeUncleanMarker,
  runOpenMaintenance,
} from "./invariants.js";

function syncPath(path: string): void {
  const handle = openSync(path, constants.O_RDONLY);
  try {
    fsyncSync(handle);
  } finally {
    closeSync(handle);
  }
}

function removeCreationFiles(path: string): void {
  for (const suffix of SQLITE_SOURCE_SUFFIXES) {
    rmSync(`${path}${suffix}`, { force: true });
  }
}

export function initialiseCurrentDatabase(databasePath: string): void {
  const temporaryPath = `${databasePath}.creating-${process.pid}-${randomBytes(12).toString("hex")}`;
  let database: Database.Database | undefined;
  try {
    database = new Database(temporaryPath);
    chmodSync(temporaryPath, 0o600);
    database.pragma("synchronous = FULL");
    database.pragma("foreign_keys = ON");
    database.pragma("busy_timeout = 5000");
    database.pragma("trusted_schema = OFF");
    applyMigrations(database);
    assertDatabaseIntegrity(database);
    database.close();
    database = undefined;
    syncPath(temporaryPath);
    try {
      linkSync(temporaryPath, databasePath);
    } catch (error: unknown) {
      if (!(error instanceof Error && "code" in error && error.code === "EEXIST")) throw error;
      inspectFabricDatabase(databasePath);
      return;
    }
    unlinkSync(temporaryPath);
    syncPath(dirname(databasePath));
    inspectFabricDatabase(databasePath);
  } finally {
    database?.close();
    removeCreationFiles(temporaryPath);
  }
}

export function openFabricDatabase(databasePath: string): Database.Database {
  const priorUmask = process.umask(0o077);
  let database: Database.Database | undefined;
  try {
    const inspection = inspectFabricDatabase(databasePath);
    if (inspection.state === "absent") initialiseCurrentDatabase(databasePath);

    database = new Database(databasePath, { fileMustExist: true });
    registerFabricSqlFunctions(database);
    // Recheck on the exact writable handle before any persistent pragma,
    // permission or marker mutation.
    database.pragma("trusted_schema = OFF");
    assertCurrentSchema(database);
    chmodSync(databasePath, 0o600);
    database.pragma("journal_mode = WAL");
    database.pragma("synchronous = FULL");
    database.pragma("foreign_keys = ON");
    database.pragma("busy_timeout = 5000");
    const marker = prepareUncleanMarker(databasePath);
    if (marker.wasUnclean) assertDatabaseIntegrity(database);
    runOpenMaintenance(database);
    for (const path of [`${databasePath}-wal`, `${databasePath}-shm`]) {
      if (existsSync(path)) chmodSync(path, 0o600);
    }
    const originalClose = database.close.bind(database);
    database.close = (() => {
      originalClose();
      removeUncleanMarker(marker.markerPath);
    }) as Database.Database["close"];
    return database;
  } catch (error: unknown) {
    database?.close();
    throw error;
  } finally {
    process.umask(priorUmask);
  }
}
