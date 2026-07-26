import { randomBytes } from "node:crypto";
import {
  chmodSync,
  closeSync,
  constants,
  fsyncSync,
  lstatSync,
  linkSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  realpathSync,
  renameSync,
  rmdirSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";

import {
  assertSameSourceSet,
  type FabricDatabaseCutoverInspection,
  inspectFabricDatabaseForCutover,
  SQLITE_SOURCE_SUFFIXES,
  stableSourceSet,
  stableSourceSetSha256,
  type SchemaCutoverFieldMismatch,
  type StableFileIdentity,
  type StableSourceSet,
} from "../core/migrations.js";
import { initialiseCurrentDatabase } from "../persistence/sqlite.js";

type SourceMemberReceipt = Readonly<{
  suffix: typeof SQLITE_SOURCE_SUFFIXES[number];
  archiveName: string;
  identity: StableFileIdentity;
}>;

type DatabaseCutoverBase = Readonly<{
  schemaVersion: 1;
  kind: "agent-fabric-database-archive-and-fresh";
  databasePath: string;
}>;

export type DatabaseArchiveAndFreshResult =
  | DatabaseCutoverBase & Readonly<{
    status: "no-op";
    reason: "database-absent" | "database-empty" | "database-current";
  }>
  | DatabaseCutoverBase & Readonly<{
    status: "confirmation-required";
    mismatch: Readonly<{
      code: "SCHEMA_CUTOVER_REQUIRED";
      message: string;
      fields: readonly SchemaCutoverFieldMismatch[];
    }>;
    confirmation: Readonly<{ sourceSetSha256: string; confirmed: false }>;
    source: Readonly<{ members: readonly SourceMemberReceipt[] }>;
  }>
  | DatabaseCutoverBase & Readonly<{
    status: "conflict";
    code: "CUTOVER_CONFIRMATION_MISMATCH";
    archiveDirectory: string;
    confirmation: Readonly<{
      sourceSetSha256: string;
      suppliedSourceSetSha256: string;
      confirmed: false;
    }>;
    sourcePreserved: true;
  }>
  | DatabaseCutoverBase & Readonly<{
    status: "conflict";
    code: "PARTIAL_SQLITE_SOURCE_SET";
    confirmation: Readonly<{ sourceSetSha256: string; confirmed: false }>;
    source: Readonly<{ members: readonly SourceMemberReceipt[] }>;
    sourcePreserved: true;
  }>
  | DatabaseCutoverBase & Readonly<{
    status: "completed";
    archiveDirectory: string;
    receiptPath: string;
    mismatch: Readonly<{
      code: "SCHEMA_CUTOVER_REQUIRED";
      message: string;
      fields: readonly SchemaCutoverFieldMismatch[];
    }>;
    confirmation: Readonly<{ sourceSetSha256: string; confirmed: true }>;
    source: Readonly<{ disposition: "archived"; members: readonly SourceMemberReceipt[] }>;
    freshBaseline: Readonly<{ status: "current"; currentVersion: 1 }>;
  }>
  | DatabaseCutoverBase & Readonly<{
    status: "archive-complete-fresh-init-failed";
    code: "FRESH_BASELINE_INITIALISATION_FAILED";
    archiveDirectory: string;
    receiptPath: string;
    confirmation: Readonly<{ sourceSetSha256: string; confirmed: true }>;
    source: Readonly<{ disposition: "archived"; members: readonly SourceMemberReceipt[] }>;
    freshBaseline: Readonly<{ status: "failed"; error: string }>;
    recovery: Readonly<{
      boundary: "archive-complete";
      action: string;
    }>;
  }>
  | DatabaseCutoverBase & Readonly<{
    status: "archive-complete-cutover-failed";
    code:
      | "SOURCE_DISPLACEMENT_FAILED"
      | "SOURCE_DISPLACEMENT_CLEANUP_FAILED"
      | "RECEIPT_FINALISATION_FAILED";
    archiveDirectory: string;
    receiptPath: string;
    confirmation: Readonly<{ sourceSetSha256: string; confirmed: true }>;
    source: Readonly<{ disposition: "archived"; members: readonly SourceMemberReceipt[] }>;
    freshBaseline: Readonly<{ status: "pending" | "current"; currentVersion?: 1 }>;
    message: string;
    recovery: Readonly<{
      boundary: "archive-complete";
      action: string;
    }>;
  }>
  | DatabaseCutoverBase & Readonly<{
    status: "failed";
    code:
      | "ARCHIVE_DESTINATION_EXISTS"
      | "ARCHIVE_WRITE_FAILED"
      | "INVALID_ARGUMENT"
      | "SOURCE_SET_CHANGED"
      | "SOURCE_SET_INVALID";
    message: string;
    sourcePreserved: true;
  }>;

export type DatabaseArchiveAndFreshOptions = Readonly<{
  databasePath: string;
  archiveDirectory: string;
  confirmSourceSetSha256?: string;
}>;

export type DatabaseArchiveAndFreshDependencies = Readonly<{
  afterInspection?: (
    inspection: Extract<FabricDatabaseCutoverInspection, { state: "incompatible" }>,
  ) => void;
  writeArchiveFile?: (path: string, bytes: Buffer, mode: number) => void;
  beforeSourceClaim?: () => void;
  claimSourceFile?: (sourcePath: string, claimedPath: string) => void;
  removeClaimedSource?: (claimedDirectory: string) => void;
  initialiseFreshDatabase?: (databasePath: string) => void;
  finaliseReceipt?: (receiptPath: string, receipt: Record<string, unknown>) => void;
}>;

function sourceMembers(databasePath: string, sources: StableSourceSet): SourceMemberReceipt[] {
  return SQLITE_SOURCE_SUFFIXES.flatMap((suffix) => {
    const source = sources.get(suffix);
    return source === undefined
      ? []
      : [{ suffix, archiveName: `${basename(databasePath)}${suffix}`, identity: source.identity }];
  });
}

function isCompleteSqliteSourceShape(sources: StableSourceSet): boolean {
  const hasWal = sources.has("-wal");
  const hasShm = sources.has("-shm");
  const hasJournal = sources.has("-journal");
  return hasWal === hasShm && !(hasJournal && (hasWal || hasShm));
}

function inspectionResult(
  databasePath: string,
  inspection: FabricDatabaseCutoverInspection,
): DatabaseArchiveAndFreshResult {
  const base = {
    schemaVersion: 1,
    kind: "agent-fabric-database-archive-and-fresh",
    databasePath,
  } as const;
  if (inspection.state === "absent") {
    return { ...base, status: "no-op", reason: "database-absent" };
  }
  if (inspection.state === "empty") {
    return { ...base, status: "no-op", reason: "database-empty" };
  }
  if (inspection.state === "current") {
    return { ...base, status: "no-op", reason: "database-current" };
  }
  if (!isCompleteSqliteSourceShape(inspection.sources)) {
    return {
      ...base,
      status: "conflict",
      code: "PARTIAL_SQLITE_SOURCE_SET",
      confirmation: {
        sourceSetSha256: stableSourceSetSha256(inspection.sources),
        confirmed: false,
      },
      source: { members: sourceMembers(databasePath, inspection.sources) },
      sourcePreserved: true,
    };
  }
  return {
    ...base,
    status: "confirmation-required",
    mismatch: inspection.mismatch,
    confirmation: {
      sourceSetSha256: stableSourceSetSha256(inspection.sources),
      confirmed: false,
    },
    source: { members: sourceMembers(databasePath, inspection.sources) },
  };
}

export function inspectDatabaseArchiveAndFresh(databasePath: string): DatabaseArchiveAndFreshResult {
  return inspectionResult(databasePath, inspectFabricDatabaseForCutover(databasePath));
}

function syncPath(path: string): void {
  const handle = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    fsyncSync(handle);
  } finally {
    closeSync(handle);
  }
}

function pathExists(path: string): boolean {
  try {
    lstatSync(path);
    return true;
  } catch (error: unknown) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") return false;
    throw error;
  }
}

function assertArchivePathDisjoint(databasePath: string, archiveDirectory: string): void {
  const prospectiveRealPath = (path: string): string => {
    const absolutePath = resolve(path);
    let existing = absolutePath;
    while (!pathExists(existing)) existing = dirname(existing);
    return resolve(realpathSync(existing), relative(existing, absolutePath));
  };
  const archivePath = prospectiveRealPath(archiveDirectory);
  const canonicalDatabasePath = realpathSync(databasePath);
  for (const suffix of SQLITE_SOURCE_SUFFIXES) {
    const sourcePath = `${canonicalDatabasePath}${suffix}`;
    if (archivePath === sourcePath || archivePath.startsWith(`${sourcePath}${sep}`)) {
      throw new Error(
        `archive destination must not equal or descend from database source member ${sourcePath}`,
      );
    }
  }
}

function writeExclusiveFile(path: string, bytes: Buffer, mode: number): void {
  const handle = openSync(
    path,
    constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW,
    mode,
  );
  try {
    writeFileSync(handle, bytes);
    fsyncSync(handle);
  } finally {
    closeSync(handle);
  }
  chmodSync(path, mode);
  syncPath(path);
}

function receiptBytes(receipt: Record<string, unknown>): Buffer {
  return Buffer.from(`${JSON.stringify(receipt, null, 2)}\n`, "utf8");
}

function assertStagedArchive(
  stagedDatabasePath: string,
  expected: StableSourceSet,
): void {
  const actual = stableSourceSet(stagedDatabasePath);
  for (const suffix of SQLITE_SOURCE_SUFFIXES) {
    const expectedSource = expected.get(suffix);
    const actualSource = actual.get(suffix);
    if (expectedSource === undefined && actualSource === undefined) continue;
    if (
      expectedSource === undefined ||
      actualSource === undefined ||
      expectedSource.identity.sha256 !== actualSource.identity.sha256 ||
      expectedSource.identity.size !== actualSource.identity.size ||
      (BigInt(expectedSource.identity.mode) & 0o7777n) !==
        (BigInt(actualSource.identity.mode) & 0o7777n)
    ) {
      throw new Error(`staged database archive does not match source suffix ${JSON.stringify(suffix)}`);
    }
  }
}

function archiveReceipt(
  databasePath: string,
  sourceSetSha256: string,
  inspection: Extract<FabricDatabaseCutoverInspection, { state: "incompatible" }>,
  status: "archive-complete" | "completed",
): Record<string, unknown> {
  return {
    schemaVersion: 1,
    kind: "agent-fabric-database-archive-and-fresh-receipt",
    status,
    databaseName: basename(databasePath),
    confirmation: { sourceSetSha256, confirmed: true },
    mismatch: {
      code: inspection.mismatch.code,
      fields: inspection.mismatch.fields.map(({ field }) => ({ field })),
    },
    source: {
      disposition: "archived",
      members: sourceMembers(databasePath, inspection.sources),
    },
    freshBaseline: status === "completed"
      ? { status: "current", currentVersion: 1 }
      : { status: "pending" },
    recovery: status === "completed"
      ? null
      : {
        boundary: "archive-complete",
        action: "Do not start Fabric. If fresh initialisation fails, preserve this archive and restore every receipt member before retrying.",
      },
  };
}

type ClaimedSourceSet = Readonly<{
  directory: string;
  databasePath: string;
}>;

function assertClaimedSourceSet(expected: StableSourceSet, actual: StableSourceSet): void {
  const stableClaimFields = [
    "dev",
    "ino",
    "mode",
    "uid",
    "gid",
    "rdev",
    "size",
    "mtimeNs",
    "birthtimeNs",
    "sha256",
  ] as const;
  for (const suffix of SQLITE_SOURCE_SUFFIXES) {
    const before = expected.get(suffix);
    const after = actual.get(suffix);
    if (
      before === undefined ||
      after === undefined ||
      stableClaimFields.some((field) => before.identity[field] !== after.identity[field])
    ) {
      if (before === undefined && after === undefined) continue;
      throw new Error(
        `claimed database source does not match confirmed member at suffix ${JSON.stringify(suffix)}`,
      );
    }
  }
}

function assertSourcePathsAbsent(databasePath: string): void {
  for (const suffix of SQLITE_SOURCE_SUFFIXES) {
    if (pathExists(`${databasePath}${suffix}`)) {
      throw new Error(
        `database source set changed during archive claim at suffix ${JSON.stringify(suffix)}`,
      );
    }
  }
}

function restoreClaimedSourceSet(
  databasePath: string,
  claimed: ClaimedSourceSet,
  expected: StableSourceSet,
): void {
  for (const suffix of [...SQLITE_SOURCE_SUFFIXES].reverse()) {
    const claimedPath = `${claimed.databasePath}${suffix}`;
    if (!pathExists(claimedPath)) continue;
    const sourcePath = `${databasePath}${suffix}`;
    // linkSync is an exclusive restore: unlike rename, it cannot overwrite a
    // path created by a concurrent writer.
    linkSync(claimedPath, sourcePath);
    unlinkSync(claimedPath);
  }
  syncPath(dirname(databasePath));
  assertClaimedSourceSet(expected, stableSourceSet(databasePath));
  rmdirSync(claimed.directory);
}

function claimSourceSet(
  databasePath: string,
  expected: StableSourceSet,
  claimSourceFile: (sourcePath: string, claimedPath: string) => void,
): ClaimedSourceSet {
  const directory = mkdtempSync(join(
    dirname(databasePath),
    `.${basename(databasePath)}.cutover-claim-`,
  ));
  chmodSync(directory, 0o700);
  const claimed = { directory, databasePath: join(directory, basename(databasePath)) };
  try {
    for (const suffix of SQLITE_SOURCE_SUFFIXES) {
      if (!expected.has(suffix)) continue;
      claimSourceFile(`${databasePath}${suffix}`, `${claimed.databasePath}${suffix}`);
    }
    assertClaimedSourceSet(expected, stableSourceSet(claimed.databasePath));
    assertSourcePathsAbsent(databasePath);
    syncPath(directory);
    syncPath(dirname(databasePath));
    return claimed;
  } catch (error: unknown) {
    try {
      restoreClaimedSourceSet(databasePath, claimed, expected);
    } catch (restoreError: unknown) {
      throw new Error(
        `database source claim failed and exact rollback also failed: ${
          restoreError instanceof Error ? restoreError.message : String(restoreError)
        }`,
        { cause: error },
      );
    }
    throw error;
  }
}

function publishArchive(
  databasePath: string,
  archiveDirectory: string,
  sourceSetSha256: string,
  inspection: Extract<FabricDatabaseCutoverInspection, { state: "incompatible" }>,
  writeArchiveFile: (path: string, bytes: Buffer, mode: number) => void,
): string {
  const parent = dirname(archiveDirectory);
  mkdirSync(parent, { recursive: true, mode: 0o700 });
  const stagingDirectory = mkdtempSync(join(
    parent,
    `.${basename(archiveDirectory)}.staging-${randomBytes(6).toString("hex")}-`,
  ));
  chmodSync(stagingDirectory, 0o700);
  let archiveReserved = false;
  let archivePublished = false;
  try {
    for (const member of sourceMembers(databasePath, inspection.sources)) {
      const source = inspection.sources.get(member.suffix);
      if (source === undefined) throw new TypeError("source receipt member disappeared");
      const mode = Number(BigInt(source.identity.mode) & 0o7777n);
      writeArchiveFile(join(stagingDirectory, member.archiveName), source.bytes, mode);
    }
    assertStagedArchive(
      join(stagingDirectory, basename(databasePath)),
      inspection.sources,
    );
    writeArchiveFile(
      join(stagingDirectory, "receipt.json"),
      receiptBytes(archiveReceipt(
        databasePath,
        sourceSetSha256,
        inspection,
        "archive-complete",
      )),
      0o600,
    );
    syncPath(stagingDirectory);
    assertSameSourceSet(inspection.sources, stableSourceSet(databasePath));

    // This exclusive claim is the no-overwrite guard. The staged source set is
    // published only into a directory created by this invocation.
    mkdirSync(archiveDirectory, { recursive: false, mode: 0o700 });
    archiveReserved = true;
    renameSync(stagingDirectory, join(archiveDirectory, "source-set"));
    archivePublished = true;
    syncPath(archiveDirectory);
    syncPath(parent);
    return join(archiveDirectory, "source-set", "receipt.json");
  } catch (error: unknown) {
    if (archivePublished) {
      try {
        renameSync(join(archiveDirectory, "source-set"), stagingDirectory);
        rmdirSync(archiveDirectory);
        archivePublished = false;
        archiveReserved = false;
      } catch (rollbackError: unknown) {
        throw new Error(
          `archive publication failed and exact rollback also failed: ${
            rollbackError instanceof Error ? rollbackError.message : String(rollbackError)
          }`,
          { cause: error },
        );
      }
    }
    rmSync(stagingDirectory, { recursive: true, force: true });
    if (archiveReserved) rmdirSync(archiveDirectory);
    throw error;
  }
}

function replaceReceipt(
  receiptPath: string,
  receipt: Record<string, unknown>,
): void {
  const temporaryPath = `${receiptPath}.tmp-${randomBytes(12).toString("hex")}`;
  try {
    writeExclusiveFile(temporaryPath, receiptBytes(receipt), 0o600);
    renameSync(temporaryPath, receiptPath);
    syncPath(dirname(receiptPath));
  } finally {
    rmSync(temporaryPath, { force: true });
  }
}

export function archiveAndFreshDatabase(
  options: DatabaseArchiveAndFreshOptions,
  dependencies: DatabaseArchiveAndFreshDependencies = {},
): DatabaseArchiveAndFreshResult {
  const inspection = inspectFabricDatabaseForCutover(options.databasePath);
  const preview = inspectionResult(options.databasePath, inspection);
  if (inspection.state !== "incompatible" || preview.status !== "confirmation-required") {
    return preview;
  }
  if (options.confirmSourceSetSha256 === undefined) return preview;
  const sourceSetSha256 = stableSourceSetSha256(inspection.sources);
  if (options.confirmSourceSetSha256 !== sourceSetSha256) {
    return {
      schemaVersion: 1,
      kind: "agent-fabric-database-archive-and-fresh",
      status: "conflict",
      code: "CUTOVER_CONFIRMATION_MISMATCH",
      databasePath: options.databasePath,
      archiveDirectory: options.archiveDirectory,
      confirmation: {
        sourceSetSha256,
        suppliedSourceSetSha256: options.confirmSourceSetSha256,
        confirmed: false,
      },
      sourcePreserved: true,
    };
  }
  if (!isAbsolute(options.archiveDirectory)) {
    throw new Error("database archive-and-fresh requires an absolute --archive directory");
  }
  assertArchivePathDisjoint(options.databasePath, options.archiveDirectory);
  dependencies.afterInspection?.(inspection);

  const receiptPath = publishArchive(
    options.databasePath,
    options.archiveDirectory,
    sourceSetSha256,
    inspection,
    dependencies.writeArchiveFile ?? writeExclusiveFile,
  );
  const recoveryAction =
    "Do not start Fabric. Preserve the archive, move aside any incomplete fresh source set, restore every member named by receipt.json to the database path with its recorded mode, verify every sha256, then retry.";
  const displacementRecoveryAction =
    "Do not start Fabric or overwrite any live source path. Preserve the archive and every live or private claim path, compare their identities and sha256 values with receipt.json, and manually reconcile any newer or unarchived race-winner state before retrying.";
  let claimed: ClaimedSourceSet;
  try {
    dependencies.beforeSourceClaim?.();
    assertSameSourceSet(inspection.sources, stableSourceSet(options.databasePath));
    claimed = claimSourceSet(
      options.databasePath,
      inspection.sources,
      dependencies.claimSourceFile ?? renameSync,
    );
  } catch (error: unknown) {
    return {
      schemaVersion: 1,
      kind: "agent-fabric-database-archive-and-fresh",
      status: "archive-complete-cutover-failed",
      code: "SOURCE_DISPLACEMENT_FAILED",
      databasePath: options.databasePath,
      archiveDirectory: options.archiveDirectory,
      receiptPath,
      confirmation: { sourceSetSha256, confirmed: true },
      source: {
        disposition: "archived",
        members: sourceMembers(options.databasePath, inspection.sources),
      },
      freshBaseline: { status: "pending" },
      message: error instanceof Error ? error.message : String(error),
      recovery: { boundary: "archive-complete", action: displacementRecoveryAction },
    };
  }
  try {
    (dependencies.removeClaimedSource ?? ((path) => rmSync(path, { recursive: true })))(claimed.directory);
    syncPath(dirname(options.databasePath));
  } catch (error: unknown) {
    return {
      schemaVersion: 1,
      kind: "agent-fabric-database-archive-and-fresh",
      status: "archive-complete-cutover-failed",
      code: "SOURCE_DISPLACEMENT_CLEANUP_FAILED",
      databasePath: options.databasePath,
      archiveDirectory: options.archiveDirectory,
      receiptPath,
      confirmation: { sourceSetSha256, confirmed: true },
      source: {
        disposition: "archived",
        members: sourceMembers(options.databasePath, inspection.sources),
      },
      freshBaseline: { status: "pending" },
      message: error instanceof Error ? error.message : String(error),
      recovery: { boundary: "archive-complete", action: recoveryAction },
    };
  }
  try {
    (dependencies.initialiseFreshDatabase ?? initialiseCurrentDatabase)(options.databasePath);
    const freshSources = stableSourceSet(options.databasePath);
    const freshInspection = inspectDatabaseArchiveAndFresh(options.databasePath);
    if (
      !isCompleteSqliteSourceShape(freshSources) ||
      freshInspection.status !== "no-op" ||
      freshInspection.reason !== "database-current"
    ) {
      throw new Error(
        `fresh baseline source set is not exclusively current: ${JSON.stringify(freshInspection)}`,
      );
    }
  } catch (error: unknown) {
    const recovery = {
      boundary: "archive-complete",
      action: recoveryAction,
    } as const;
    const failedReceipt = {
      ...archiveReceipt(
        options.databasePath,
        sourceSetSha256,
        inspection,
        "archive-complete",
      ),
      status: "archive-complete-fresh-init-failed",
      freshBaseline: {
        status: "failed",
        code: "FRESH_BASELINE_INITIALISATION_FAILED",
      },
      recovery,
    };
    try {
      replaceReceipt(receiptPath, failedReceipt);
    } catch {
      // The already-durable archive-complete receipt retains the same recovery
      // boundary even if the failure update itself cannot be persisted.
    }
    return {
      schemaVersion: 1,
      kind: "agent-fabric-database-archive-and-fresh",
      status: "archive-complete-fresh-init-failed",
      code: "FRESH_BASELINE_INITIALISATION_FAILED",
      databasePath: options.databasePath,
      archiveDirectory: options.archiveDirectory,
      receiptPath,
      confirmation: { sourceSetSha256, confirmed: true },
      source: {
        disposition: "archived",
        members: sourceMembers(options.databasePath, inspection.sources),
      },
      freshBaseline: {
        status: "failed",
        error: error instanceof Error ? error.message : String(error),
      },
      recovery,
    };
  }
  const receipt = archiveReceipt(
    options.databasePath,
    sourceSetSha256,
    inspection,
    "completed",
  );
  try {
    (dependencies.finaliseReceipt ?? replaceReceipt)(receiptPath, receipt);
  } catch (error: unknown) {
    return {
      schemaVersion: 1,
      kind: "agent-fabric-database-archive-and-fresh",
      status: "archive-complete-cutover-failed",
      code: "RECEIPT_FINALISATION_FAILED",
      databasePath: options.databasePath,
      archiveDirectory: options.archiveDirectory,
      receiptPath,
      confirmation: { sourceSetSha256, confirmed: true },
      source: {
        disposition: "archived",
        members: sourceMembers(options.databasePath, inspection.sources),
      },
      freshBaseline: { status: "current", currentVersion: 1 },
      message: error instanceof Error ? error.message : String(error),
      recovery: {
        boundary: "archive-complete",
        action: "Do not repeat the cutover. The fresh baseline is current, but receipt finalisation failed; preserve the archive and reconcile receipt.json before starting Fabric.",
      },
    };
  }
  return {
    schemaVersion: 1,
    kind: "agent-fabric-database-archive-and-fresh",
    status: "completed",
    databasePath: options.databasePath,
    archiveDirectory: options.archiveDirectory,
    receiptPath,
    mismatch: inspection.mismatch,
    confirmation: { sourceSetSha256, confirmed: true },
    source: {
      disposition: "archived",
      members: sourceMembers(options.databasePath, inspection.sources),
    },
    freshBaseline: { status: "current", currentVersion: 1 },
  };
}

function cliOption(arguments_: string[], name: string): string | undefined {
  const indexes = arguments_.flatMap((value, index) => value === name ? [index] : []);
  if (indexes.length > 1) throw new Error(`${name} may be provided only once`);
  const index = indexes[0];
  if (index === undefined) return undefined;
  const value = arguments_[index + 1];
  if (value === undefined || value.startsWith("--")) throw new Error(`${name} requires a value`);
  return value;
}

function cutoverFailure(databasePath: string, error: unknown): DatabaseArchiveAndFreshResult {
  const message = error instanceof Error ? error.message : String(error);
  let code: Extract<DatabaseArchiveAndFreshResult, { status: "failed" }>["code"] =
    "ARCHIVE_WRITE_FAILED";
  if (error instanceof Error && "code" in error && error.code === "EEXIST") {
    code = "ARCHIVE_DESTINATION_EXISTS";
  } else if (/option|requires|absolute --archive/iu.test(message)) {
    code = "INVALID_ARGUMENT";
  } else if (/changed|disappeared/iu.test(message)) {
    code = "SOURCE_SET_CHANGED";
  } else if (
    /symbolic link|single-link regular file|not a regular non-symlink|archive destination must not/iu
      .test(message)
  ) {
    code = "SOURCE_SET_INVALID";
  }
  return {
    schemaVersion: 1,
    kind: "agent-fabric-database-archive-and-fresh",
    status: "failed",
    code,
    databasePath,
    message,
    sourcePreserved: true,
  };
}

export function runDatabaseArchiveAndFreshCli(
  arguments_: string[],
  defaultDatabasePath: string,
): Readonly<{ result: DatabaseArchiveAndFreshResult; exitCode: 0 | 1 | 4 }> {
  let databasePath = defaultDatabasePath;
  try {
    const allowed = new Set(["--database", "--archive", "--confirm-source-set"]);
    for (let index = 0; index < arguments_.length; index += 2) {
      const name = arguments_[index];
      if (name === undefined || !allowed.has(name)) {
        throw new Error(`unknown database archive-and-fresh option: ${name ?? ""}`);
      }
      if (arguments_[index + 1] === undefined) throw new Error(`${name} requires a value`);
    }
    databasePath = cliOption(arguments_, "--database") ?? defaultDatabasePath;
    const archiveDirectory = cliOption(arguments_, "--archive");
    if (archiveDirectory === undefined) {
      throw new Error("database archive-and-fresh requires --archive ABSOLUTE_NEW_DIRECTORY");
    }
    const confirmSourceSetSha256 = cliOption(arguments_, "--confirm-source-set");
    const result = archiveAndFreshDatabase({
      databasePath,
      archiveDirectory,
      ...(confirmSourceSetSha256 === undefined
        ? {}
        : { confirmSourceSetSha256 }),
    });
    return {
      result,
      exitCode: result.status === "archive-complete-fresh-init-failed" ||
          result.status === "archive-complete-cutover-failed"
        ? 4
        : result.status === "failed" || result.status === "conflict"
          ? 1
          : 0,
    };
  } catch (error: unknown) {
    return { result: cutoverFailure(databasePath, error), exitCode: 1 };
  }
}
