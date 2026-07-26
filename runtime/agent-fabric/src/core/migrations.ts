import { createHash } from "node:crypto";
import {
  type BigIntStats,
  chmodSync,
  closeSync,
  constants,
  existsSync,
  fstatSync,
  lstatSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import type Database from "better-sqlite3";
import BetterSqlite3 from "better-sqlite3";

export const FABRIC_SCHEMA_EPOCH = "agent-fabric-pre-release-v1" as const;

type CurrentSchemaManifest = Readonly<{
  schemaVersion: 1;
  epoch: typeof FABRIC_SCHEMA_EPOCH;
  baselineFile: "0001-current-baseline.sql";
  baselineSha256: string;
  catalogSha256: string;
  objectCount: number;
}>;

type SchemaArtifacts = Readonly<{
  manifest: CurrentSchemaManifest;
  sql: string;
}>;

type FabricSchemaRow = Readonly<{
  epoch: string;
  baseline_sha256: string;
  catalog_sha256: string;
}>;

type CatalogRow = Readonly<{
  type: string;
  name: string;
  tbl_name: string;
  sql: string | null;
}>;

export type SchemaBaselineErrorCode =
  | "SCHEMA_BASELINE_INVALID"
  | "SCHEMA_CUTOVER_REQUIRED";

export class SchemaBaselineError extends Error {
  readonly code: SchemaBaselineErrorCode;
  readonly preserved: boolean;

  constructor(
    code: SchemaBaselineErrorCode,
    message: string,
    options?: Readonly<{ cause?: unknown; preserved?: boolean }>,
  ) {
    super(message, options?.cause === undefined ? undefined : { cause: options.cause });
    this.name = "SchemaBaselineError";
    this.code = code;
    this.preserved = options?.preserved ?? false;
  }
}

function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

function canonicalJson(value: unknown): string {
  if (
    value === null ||
    typeof value === "boolean" ||
    typeof value === "string"
  ) return JSON.stringify(value);
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new TypeError("canonical JSON number is not finite");
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (typeof value === "object") {
    const record = value as Record<string, unknown>;
    return `{${Object.keys(record).sort().map((key) => (
      `${JSON.stringify(key)}:${canonicalJson(record[key])}`
    )).join(",")}}`;
  }
  throw new TypeError("value is not JSON-compatible");
}

function topologyPlanDigest(planJson: unknown): string | null {
  if (typeof planJson !== "string") return null;
  try {
    const parsed: unknown = JSON.parse(planJson);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
    const body = { ...(parsed as Record<string, unknown>) };
    delete body.planDigest;
    return `sha256:${sha256(canonicalJson(body))}`;
  } catch {
    return null;
  }
}

/** Connection-local deterministic functions used by the pinned schema. */
export function registerFabricSqlFunctions(database: Database.Database): void {
  database.function(
    "fabric_topology_plan_digest",
    { deterministic: true },
    topologyPlanDigest,
  );
}

function artifactUrl(directory: "migrations" | "schemas", filename: string): URL {
  const candidates = [
    new URL(`../../${directory}/${filename}`, import.meta.url),
    new URL(`../../../${directory}/${filename}`, import.meta.url),
  ];
  const selected = candidates.find((candidate) => existsSync(candidate));
  if (selected === undefined) {
    throw new SchemaBaselineError(
      "SCHEMA_BASELINE_INVALID",
      `current schema artifact is unavailable: ${directory}/${filename}`,
    );
  }
  return selected;
}

function parseManifest(value: unknown): CurrentSchemaManifest {
  if (
    typeof value !== "object" ||
    value === null ||
    Array.isArray(value)
  ) {
    throw new SchemaBaselineError("SCHEMA_BASELINE_INVALID", "current schema manifest is not an object");
  }
  const manifest = value as Record<string, unknown>;
  const keys = Object.keys(manifest).sort();
  const expected = [
    "baselineFile",
    "baselineSha256",
    "catalogSha256",
    "epoch",
    "objectCount",
    "schemaVersion",
  ];
  if (
    keys.length !== expected.length ||
    keys.some((key, index) => key !== expected[index]) ||
    manifest.schemaVersion !== 1 ||
    manifest.epoch !== FABRIC_SCHEMA_EPOCH ||
    manifest.baselineFile !== "0001-current-baseline.sql" ||
    typeof manifest.baselineSha256 !== "string" ||
    !/^[0-9a-f]{64}$/u.test(manifest.baselineSha256) ||
    typeof manifest.catalogSha256 !== "string" ||
    !/^[0-9a-f]{64}$/u.test(manifest.catalogSha256) ||
    typeof manifest.objectCount !== "number" ||
    !Number.isSafeInteger(manifest.objectCount) ||
    manifest.objectCount < 1
  ) {
    throw new SchemaBaselineError("SCHEMA_BASELINE_INVALID", "current schema manifest is invalid");
  }
  return {
    schemaVersion: 1,
    epoch: FABRIC_SCHEMA_EPOCH,
    baselineFile: "0001-current-baseline.sql",
    baselineSha256: manifest.baselineSha256,
    catalogSha256: manifest.catalogSha256,
    objectCount: manifest.objectCount,
  };
}

function loadSchemaArtifacts(): SchemaArtifacts {
  const manifestValue: unknown = JSON.parse(
    readFileSync(artifactUrl("schemas", "database-baseline.v1.json"), "utf8"),
  );
  const manifest = parseManifest(manifestValue);
  const sql = readFileSync(artifactUrl("migrations", manifest.baselineFile), "utf8");
  if (sha256(sql) !== manifest.baselineSha256) {
    throw new SchemaBaselineError(
      "SCHEMA_BASELINE_INVALID",
      "current schema baseline does not match its pinned manifest",
    );
  }
  return { manifest, sql };
}

function catalogRows(database: Database.Database): CatalogRow[] {
  return database.prepare(`
    SELECT type,name,tbl_name,sql
      FROM sqlite_schema
     WHERE name NOT LIKE 'sqlite_%'
     ORDER BY type,name,tbl_name
  `).all() as CatalogRow[];
}

export function currentSchemaCatalogFingerprint(database: Database.Database): string {
  const canonical = JSON.stringify(
    catalogRows(database).map((row) => [row.type, row.name, row.tbl_name, row.sql]),
  );
  return sha256(canonical);
}

function cutover(message: string, cause?: unknown): SchemaBaselineError {
  return new SchemaBaselineError(
    "SCHEMA_CUTOVER_REQUIRED",
    `${message}; existing database preserved`,
    { ...(cause === undefined ? {} : { cause }), preserved: true },
  );
}

function currentSchemaRow(database: Database.Database): FabricSchemaRow {
  let rows: FabricSchemaRow[];
  try {
    rows = database.prepare(`
      SELECT epoch,baseline_sha256,catalog_sha256 FROM fabric_schema
    `).all() as FabricSchemaRow[];
  } catch (error: unknown) {
    throw cutover("database does not contain the current schema epoch", error);
  }
  if (rows.length !== 1 || rows[0] === undefined) {
    throw cutover("database schema epoch metadata is missing or ambiguous");
  }
  return rows[0];
}

export function assertCurrentSchema(database: Database.Database): void {
  const { manifest } = loadSchemaArtifacts();
  const row = currentSchemaRow(database);
  if (
    row.epoch !== manifest.epoch ||
    row.baseline_sha256 !== manifest.baselineSha256 ||
    row.catalog_sha256 !== manifest.catalogSha256
  ) {
    throw cutover("database schema epoch does not match this runtime");
  }
  const rows = catalogRows(database);
  if (
    rows.length !== manifest.objectCount ||
    currentSchemaCatalogFingerprint(database) !== manifest.catalogSha256
  ) {
    throw cutover("database schema catalog fingerprint does not match this runtime");
  }
}

function userSchemaObjectCount(database: Database.Database): number {
  const value = database.prepare(`
    SELECT COUNT(*) AS count FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'
  `).get() as { count: number };
  return value.count;
}

export const SQLITE_SOURCE_SUFFIXES = ["", "-wal", "-shm", "-journal"] as const;
export type SqliteSourceSuffix = typeof SQLITE_SOURCE_SUFFIXES[number];

export type StableFileIdentity = Readonly<{
  dev: string;
  ino: string;
  mode: string;
  nlink: string;
  uid: string;
  gid: string;
  rdev: string;
  size: string;
  blksize: string;
  blocks: string;
  mtimeNs: string;
  ctimeNs: string;
  birthtimeNs: string;
  sha256: string;
}>;

export type StableSourceFile = Readonly<{
  bytes: Buffer;
  identity: StableFileIdentity;
}>;

export type StableSourceSet = ReadonlyMap<SqliteSourceSuffix, StableSourceFile>;

function errno(error: unknown, code: string): boolean {
  return error instanceof Error && "code" in error && error.code === code;
}

function metadataIdentity(
  metadata: BigIntStats,
  bytes: Buffer,
): StableFileIdentity {
  return {
    dev: String(metadata.dev),
    ino: String(metadata.ino),
    mode: String(metadata.mode),
    nlink: String(metadata.nlink),
    uid: String(metadata.uid),
    gid: String(metadata.gid),
    rdev: String(metadata.rdev),
    size: String(metadata.size),
    blksize: String(metadata.blksize),
    blocks: String(metadata.blocks),
    mtimeNs: String(metadata.mtimeNs),
    ctimeNs: String(metadata.ctimeNs),
    birthtimeNs: String(metadata.birthtimeNs),
    sha256: createHash("sha256").update(bytes).digest("hex"),
  };
}

function stableSourceFile(path: string, required: boolean): StableSourceFile | undefined {
  let handle: number;
  try {
    handle = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  } catch (error: unknown) {
    if (!required && errno(error, "ENOENT")) return undefined;
    if (errno(error, "ELOOP")) throw cutover("database source contains a symbolic link", error);
    throw error;
  }
  try {
    const before = fstatSync(handle, { bigint: true });
    if (!before.isFile() || before.nlink !== 1n) {
      throw cutover("database source is not a single-link regular file");
    }
    const bytes = readFileSync(handle);
    const after = fstatSync(handle, { bigint: true });
    const beforeIdentity = metadataIdentity(before, bytes);
    const afterIdentity = metadataIdentity(after, bytes);
    if (JSON.stringify(beforeIdentity) !== JSON.stringify(afterIdentity)) {
      throw cutover("database source changed while being cloned");
    }
    const pathMetadata = lstatSync(path, { bigint: true });
    if (!pathMetadata.isFile() || pathMetadata.isSymbolicLink()) {
      throw cutover("database source identity changed while being cloned");
    }
    const pathIdentity = metadataIdentity(pathMetadata, bytes);
    if (JSON.stringify(afterIdentity) !== JSON.stringify(pathIdentity)) {
      throw cutover("database source identity changed while being cloned");
    }
    return { bytes, identity: afterIdentity };
  } finally {
    closeSync(handle);
  }
}

export function stableSourceSet(databasePath: string): StableSourceSet {
  const sources = new Map<SqliteSourceSuffix, StableSourceFile>();
  for (const suffix of SQLITE_SOURCE_SUFFIXES) {
    const source = stableSourceFile(`${databasePath}${suffix}`, suffix === "");
    if (source !== undefined) sources.set(suffix, source);
  }
  return sources;
}

export function assertSameSourceSet(expected: StableSourceSet, actual: StableSourceSet): void {
  for (const suffix of SQLITE_SOURCE_SUFFIXES) {
    const before = expected.get(suffix);
    const after = actual.get(suffix);
    if (
      before === undefined ||
      after === undefined ||
      JSON.stringify(before.identity) !== JSON.stringify(after.identity)
    ) {
      if (before === undefined && after === undefined) continue;
      const summary = (source: StableSourceFile | undefined): unknown => (
        source === undefined
          ? { present: false }
          : {
            present: true,
            dev: source.identity.dev,
            ino: source.identity.ino,
            mode: source.identity.mode,
            size: source.identity.size,
            mtimeNs: source.identity.mtimeNs,
            ctimeNs: source.identity.ctimeNs,
            sha256: source.identity.sha256,
          }
      );
      throw cutover(
        `database source set changed during read-only schema inspection at suffix ${JSON.stringify(suffix)}: ` +
        `expected ${JSON.stringify(summary(before))}, actual ${JSON.stringify(summary(after))}`,
      );
    }
  }
}

export function stableSourceSetSha256(sources: StableSourceSet): string {
  const identities = SQLITE_SOURCE_SUFFIXES.flatMap((suffix) => {
    const source = sources.get(suffix);
    return source === undefined ? [] : [[suffix, source.identity] as const];
  });
  return `sha256:${sha256(canonicalJson(identities))}`;
}

function createPrivateDatabaseClone(databasePath: string): Readonly<{
  cloneDirectory: string;
  clonePath: string;
  sources: StableSourceSet;
}> {
  const sources = stableSourceSet(databasePath);
  const cloneDirectory = mkdtempSync(join(tmpdir(), "agent-fabric-schema-inspection-"));
  chmodSync(cloneDirectory, 0o700);
  const clonePath = join(cloneDirectory, "fabric.sqlite3");
  try {
    const main = sources.get("");
    if (main === undefined) throw cutover("database source disappeared during read-only schema inspection");
    writeFileSync(clonePath, main.bytes, { flag: "wx", mode: 0o600 });
    // WAL and a rollback journal carry committed/recovery state. SHM is
    // deliberately rebuilt inside the private directory instead of copying
    // process-shared lock state from the source path.
    for (const suffix of ["-wal", "-journal"] as const) {
      const sidecar = sources.get(suffix);
      if (sidecar !== undefined) {
        writeFileSync(`${clonePath}${suffix}`, sidecar.bytes, { flag: "wx", mode: 0o600 });
      }
    }
    assertSameSourceSet(sources, stableSourceSet(databasePath));
    return { cloneDirectory, clonePath, sources };
  } catch (error: unknown) {
    rmSync(cloneDirectory, { recursive: true, force: true });
    throw error;
  }
}

/**
 * Initialises only a genuinely empty database connection. Existing current
 * state is verified; every other epoch is rejected without repair or backfill.
 */
export function applyMigrations(
  database: Database.Database,
): { applied: number[]; currentVersion: 1 } {
  registerFabricSqlFunctions(database);
  if (userSchemaObjectCount(database) > 0) {
    assertCurrentSchema(database);
    return { applied: [], currentVersion: 1 };
  }
  const { manifest, sql } = loadSchemaArtifacts();
  database.pragma("foreign_keys = ON");
  const apply = database.transaction(() => {
    database.exec(sql);
    const rows = catalogRows(database);
    const catalogSha256 = currentSchemaCatalogFingerprint(database);
    if (rows.length !== manifest.objectCount || catalogSha256 !== manifest.catalogSha256) {
      throw new SchemaBaselineError(
        "SCHEMA_BASELINE_INVALID",
        "current schema baseline produced an unexpected catalog fingerprint",
      );
    }
    database.prepare(`
      INSERT INTO fabric_schema(singleton,epoch,baseline_sha256,catalog_sha256)
      VALUES (1,?,?,?)
    `).run(manifest.epoch, manifest.baselineSha256, catalogSha256);
  });
  apply();
  assertCurrentSchema(database);
  return { applied: [1], currentVersion: 1 };
}

export type FabricDatabaseInspection = Readonly<{
  state: "absent" | "current";
}>;

export type SchemaCutoverFieldMismatch = Readonly<{
  field:
    | "databaseFormat"
    | "epoch"
    | "baselineSha256"
    | "recordedCatalogSha256"
    | "catalogSha256"
    | "objectCount";
  expected: string | number;
  actual: string | number | null;
}>;

export type FabricDatabaseCutoverInspection =
  | Readonly<{ state: "absent" }>
  | Readonly<{ state: "empty"; sources: StableSourceSet }>
  | Readonly<{ state: "current"; sources: StableSourceSet }>
  | Readonly<{
    state: "incompatible";
    sources: StableSourceSet;
    mismatch: Readonly<{
      code: "SCHEMA_CUTOVER_REQUIRED";
      message: string;
      fields: readonly SchemaCutoverFieldMismatch[];
    }>;
  }>;

function schemaMetadataRows(database: Database.Database): unknown[] | undefined {
  try {
    return database.prepare(`
      SELECT epoch,baseline_sha256,catalog_sha256 FROM fabric_schema
    `).all() as unknown[];
  } catch {
    return undefined;
  }
}

function observedSchemaValue(value: unknown): string | number | null {
  if (value === null || typeof value === "string" || typeof value === "number") return value;
  if (typeof value === "bigint") return `bigint:${String(value)}`;
  if (Buffer.isBuffer(value)) return `blob:${value.toString("hex")}`;
  return `${typeof value}:${JSON.stringify(value)}`;
}

/**
 * Classifies a cutover candidate through the same private-clone boundary as the
 * runtime gate while retaining the exact expected/observed schema mismatch.
 */
export function inspectFabricDatabaseForCutover(
  databasePath: string,
): FabricDatabaseCutoverInspection {
  try {
    lstatSync(databasePath);
  } catch (error: unknown) {
    if (errno(error, "ENOENT")) return { state: "absent" };
    throw error;
  }

  let database: Database.Database | undefined;
  let clone: ReturnType<typeof createPrivateDatabaseClone> | undefined;
  try {
    clone = createPrivateDatabaseClone(databasePath);
    try {
      database = new BetterSqlite3(clone.clonePath);
      database.pragma("trusted_schema = OFF");
      const objectCount = userSchemaObjectCount(database);
      if (objectCount === 0) {
        assertSameSourceSet(clone.sources, stableSourceSet(databasePath));
        return { state: "empty", sources: clone.sources };
      }

      try {
        assertCurrentSchema(database);
        assertSameSourceSet(clone.sources, stableSourceSet(databasePath));
        return { state: "current", sources: clone.sources };
      } catch (error: unknown) {
        if (
          !(error instanceof SchemaBaselineError) ||
          error.code !== "SCHEMA_CUTOVER_REQUIRED"
        ) throw error;
        const { manifest } = loadSchemaArtifacts();
        const rows = schemaMetadataRows(database);
        const row = rows?.length === 1 && typeof rows[0] === "object" && rows[0] !== null
          ? rows[0] as Record<string, unknown>
          : undefined;
        const absentRow = rows === undefined
          ? "metadata-table-unreadable"
          : `metadata-row-count:${rows.length}`;
        const observed = (field: string): string | number | null => (
          row === undefined
            ? absentRow
            : field in row
              ? observedSchemaValue(row[field])
              : "metadata-column-missing"
        );
        const catalogSha256 = currentSchemaCatalogFingerprint(database);
        const fields: SchemaCutoverFieldMismatch[] = [];
        const comparisons = [
          ["epoch", manifest.epoch, observed("epoch")],
          ["baselineSha256", manifest.baselineSha256, observed("baseline_sha256")],
          ["recordedCatalogSha256", manifest.catalogSha256, observed("catalog_sha256")],
          ["catalogSha256", manifest.catalogSha256, catalogSha256],
          ["objectCount", manifest.objectCount, objectCount],
        ] as const;
        for (const [field, expected, observed] of comparisons) {
          if (expected !== observed) fields.push({ field, expected, actual: observed });
        }
        assertSameSourceSet(clone.sources, stableSourceSet(databasePath));
        return {
          state: "incompatible",
          sources: clone.sources,
          mismatch: {
            code: "SCHEMA_CUTOVER_REQUIRED",
            message: error.message,
            fields,
          },
        };
      }
    } catch (error: unknown) {
      if (error instanceof SchemaBaselineError) throw error;
      assertSameSourceSet(clone.sources, stableSourceSet(databasePath));
      return {
        state: "incompatible",
        sources: clone.sources,
        mismatch: {
          code: "SCHEMA_CUTOVER_REQUIRED",
          message: "database format or schema fingerprint is not current; existing database preserved",
          fields: [{
            field: "databaseFormat",
            expected: "SQLite 3 current schema",
            actual: error instanceof Error ? error.message : String(error),
          }],
        },
      };
    }
  } finally {
    try {
      database?.close();
    } finally {
      if (clone !== undefined) rmSync(clone.cloneDirectory, { recursive: true, force: true });
    }
  }
}

/** Read-only cutover gate used before any daemon/runtime mutation. */
export function inspectFabricDatabase(databasePath: string): FabricDatabaseInspection {
  let before;
  try {
    before = lstatSync(databasePath);
  } catch (error: unknown) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") {
      return { state: "absent" };
    }
    throw error;
  }
  if (!before.isFile() || before.isSymbolicLink()) {
    throw cutover("database path is not a regular non-symlink file");
  }
  let database: Database.Database | undefined;
  let clone: ReturnType<typeof createPrivateDatabaseClone> | undefined;
  try {
    clone = createPrivateDatabaseClone(databasePath);
    // SQLite may recover/checkpoint its private WAL or journal and may create a
    // private SHM file. It has no writable relationship with the source path.
    database = new BetterSqlite3(clone.clonePath);
    database.pragma("trusted_schema = OFF");
    assertCurrentSchema(database);
    assertSameSourceSet(clone.sources, stableSourceSet(databasePath));
  } catch (error: unknown) {
    if (error instanceof SchemaBaselineError) throw error;
    throw cutover("database format or schema fingerprint is not current", error);
  } finally {
    try {
      database?.close();
    } finally {
      if (clone !== undefined) rmSync(clone.cloneDirectory, { recursive: true, force: true });
    }
  }
  const after = lstatSync(databasePath);
  if (
    before.dev !== after.dev ||
    before.ino !== after.ino ||
    before.size !== after.size ||
    before.mtimeMs !== after.mtimeMs ||
    before.ctimeMs !== after.ctimeMs
  ) {
    throw cutover("database identity changed during read-only schema inspection");
  }
  return { state: "current" };
}
