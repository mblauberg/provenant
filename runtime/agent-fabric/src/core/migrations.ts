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
  statSync,
  utimesSync,
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
  | "SCHEMA_CUTOVER_REQUIRED"
  | "DATABASE_INSPECTION_UNSTABLE";

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

/**
 * True when SQLite itself judged the file unusable as a database, as opposed to
 * the environment failing to read it. These are the verdicts an archive-and-fresh
 * cutover is designed to resolve.
 */
function sqliteFormatVerdict(error: unknown): boolean {
  return (
    errno(error, "SQLITE_NOTADB") ||
    errno(error, "SQLITE_CORRUPT") ||
    errno(error, "SQLITE_FORMAT") ||
    errno(error, "SQLITE_ERROR")
  );
}

function inspectionUnstable(message: string, cause?: unknown): SchemaBaselineError {
  return new SchemaBaselineError(
    "DATABASE_INSPECTION_UNSTABLE",
    message,
    cause === undefined ? undefined : { cause },
  );
}

function currentSchemaRow(database: Database.Database): FabricSchemaRow {
  let rows: FabricSchemaRow[];
  try {
    rows = database.prepare(`
      SELECT epoch,baseline_sha256,catalog_sha256 FROM fabric_schema
    `).all() as FabricSchemaRow[];
  } catch (error: unknown) {
    if (!errno(error, "SQLITE_ERROR")) throw error;
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

function stableSourceFile(
  path: string,
  required: boolean,
  context: SourceReadContext,
): StableSourceFile | undefined {
  let handle: number;
  try {
    handle = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  } catch (error: unknown) {
    if (!required && errno(error, "ENOENT")) return undefined;
    if (required && context.presenceObserved && errno(error, "ENOENT")) {
      throw inspectionUnstable(
        "database source disappeared during read-only schema inspection",
        error,
      );
    }
    if (errno(error, "ELOOP")) {
      // A symbolic link where a stable read already succeeded is a source-set
      // race, not a standing property of the database.
      throw context.stableReadObserved
        ? inspectionUnstable("database source became a symbolic link during read-only schema inspection", error)
        : cutover("database source contains a symbolic link", error);
    }
    throw error;
  }
  try {
    const before = fstatSync(handle, { bigint: true });
    if (!before.isFile() || before.nlink !== 1n) {
      // Likewise, a type or link-count transition is only transient when an
      // earlier full read observed a single-link regular file.
      throw context.stableReadObserved
        ? inspectionUnstable("database source stopped being a single-link regular file during read-only schema inspection")
        : cutover("database source is not a single-link regular file");
    }
    const bytes = readFileSync(handle);
    const after = fstatSync(handle, { bigint: true });
    const beforeIdentity = metadataIdentity(before, bytes);
    const afterIdentity = metadataIdentity(after, bytes);
    if (JSON.stringify(beforeIdentity) !== JSON.stringify(afterIdentity)) {
      throw inspectionUnstable("database source changed while being cloned");
    }
    let pathMetadata: BigIntStats;
    try {
      pathMetadata = lstatSync(path, { bigint: true });
    } catch (error: unknown) {
      if (errno(error, "ENOENT")) {
        throw inspectionUnstable(
          "database source disappeared during read-only schema inspection",
          error,
        );
      }
      throw error;
    }
    if (!pathMetadata.isFile() || pathMetadata.isSymbolicLink()) {
      throw inspectionUnstable("database source identity changed while being cloned");
    }
    const pathIdentity = metadataIdentity(pathMetadata, bytes);
    if (JSON.stringify(afterIdentity) !== JSON.stringify(pathIdentity)) {
      throw inspectionUnstable("database source identity changed while being cloned");
    }
    return { bytes, identity: afterIdentity };
  } finally {
    closeSync(handle);
  }
}

/**
 * Distinguishes what an earlier read of this source already established.
 *
 * `presenceObserved` means the required source was seen to exist. It is what
 * separates "the database was never there" from "it disappeared under us".
 *
 * `stableReadObserved` is strictly stronger: a full stable read previously
 * succeeded, so the source was a single-link regular file. Only then is a
 * symlink, directory or link-count transition necessarily a race rather than a
 * standing property. An `lstat` alone establishes presence, not stability, so
 * the two are tracked separately.
 */
type SourceReadContext = Readonly<{
  presenceObserved: boolean;
  stableReadObserved: boolean;
}>;

const UNOBSERVED_SOURCE: SourceReadContext = { presenceObserved: false, stableReadObserved: false };
const RECHECKED_SOURCE: SourceReadContext = { presenceObserved: true, stableReadObserved: true };

function readStableSourceSet(
  databasePath: string,
  context: SourceReadContext,
): StableSourceSet {
  const sources = new Map<SqliteSourceSuffix, StableSourceFile>();
  for (const suffix of SQLITE_SOURCE_SUFFIXES) {
    const source = stableSourceFile(
      `${databasePath}${suffix}`,
      suffix === "",
      context,
    );
    if (source !== undefined) sources.set(suffix, source);
  }
  return sources;
}

export function stableSourceSet(databasePath: string): StableSourceSet {
  return readStableSourceSet(databasePath, UNOBSERVED_SOURCE);
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
      throw inspectionUnstable(
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

function createPrivateDatabaseClone(
  databasePath: string,
  presenceObserved = false,
): Readonly<{
  cloneDirectory: string;
  clonePath: string;
  sources: StableSourceSet;
}> {
  // The caller's prior `lstat` establishes presence only. This first read is
  // what establishes stability, so it must not treat a standing symlink or
  // hard-linked database as a transient race.
  const sources = readStableSourceSet(databasePath, { presenceObserved, stableReadObserved: false });
  const cloneDirectory = mkdtempSync(join(tmpdir(), "agent-fabric-schema-inspection-"));
  chmodSync(cloneDirectory, 0o700);
  const clonePath = join(cloneDirectory, "fabric.sqlite3");
  try {
    const main = sources.get("");
    if (main === undefined) {
      throw inspectionUnstable("database source disappeared during read-only schema inspection");
    }
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
    injectTestInspectionRace(databasePath);
    assertSameSourceSet(sources, readStableSourceSet(databasePath, RECHECKED_SOURCE));
    return { cloneDirectory, clonePath, sources };
  } catch (error: unknown) {
    rmSync(cloneDirectory, { recursive: true, force: true });
    throw error;
  }
}

const DATABASE_INSPECTION_ATTEMPTS = 5;
const DATABASE_INSPECTION_BACKOFF_MS = [10, 20, 40, 80] as const;

function databaseInspectionAttempts(): number {
  const configured = process.env.NODE_ENV === "test"
    ? Number.parseInt(process.env.AGENT_FABRIC_TEST_DATABASE_INSPECTION_ATTEMPTS ?? "", 10)
    : Number.NaN;
  return Number.isSafeInteger(configured) && configured > 0
    ? configured
    : DATABASE_INSPECTION_ATTEMPTS;
}

function injectTestInspectionRace(databasePath: string): void {
  console.error("DEBUG migrations race hook", process.env.NODE_ENV, process.env.AGENT_FABRIC_TEST_DATABASE_INSPECTION_RACE_PATH, databasePath);
  if (
    process.env.NODE_ENV !== "test" ||
    process.env.AGENT_FABRIC_TEST_DATABASE_INSPECTION_RACE_PATH !== databasePath
  ) return;
  const current = statSync(databasePath);
  const nextTimestamp = new Date(current.mtimeMs + 1_000);
  // Test-only metadata mutation makes the source recheck fail without a
  // competing writer process or an elapsed-time race.
  utimesSync(databasePath, nextTimestamp, nextTimestamp);
}

/**
 * Blocks the calling thread for the given delay.
 *
 * Inspection is synchronous all the way down to `openSync`/`readFileSync`, and
 * every caller is a CLI entry point or daemon bootstrap rather than request
 * serving, so a bounded sleep is preferable to making the whole inspection
 * stack async. The total worst-case pause is 150ms.
 */
function sleepSync(milliseconds: number): void {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, milliseconds);
}

/**
 * Retries an inspection that observed an unstable source set.
 *
 * Retrying without pausing is close to useless against a live writer: the
 * attempts land inside the same write, so a bounded backoff between them is
 * what actually lets a busy-but-healthy database converge.
 */
function retryUnstableDatabaseInspection<T>(operation: () => T): T {
  const attempts = databaseInspectionAttempts();
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    try {
      return operation();
    } catch (error: unknown) {
      if (
        !(error instanceof SchemaBaselineError) ||
        error.code !== "DATABASE_INSPECTION_UNSTABLE" ||
        attempt === attempts
      ) throw error;
      const backoff = DATABASE_INSPECTION_BACKOFF_MS[attempt - 1];
      if (backoff !== undefined) sleepSync(backoff);
    }
  }
  throw new Error("unreachable database inspection retry state");
}

/**
 * Runs an inspection against the canonical private database clone.
 *
 * SQLite may recover or checkpoint the clone. The live main file and every
 * recovery sidecar are identity-checked before and after the operation, and
 * the private clone is always removed by this owner.
 */
export function withPrivateDatabaseClone<T>(
  databasePath: string,
  operation: (clonePath: string) => T,
): T {
  return retryUnstableDatabaseInspection(() => {
    const clone = createPrivateDatabaseClone(databasePath);
    try {
      let result: T;
      try {
        result = operation(clone.clonePath);
      } catch (error: unknown) {
        assertSameSourceSet(clone.sources, readStableSourceSet(databasePath, RECHECKED_SOURCE));
        throw error;
      }
      assertSameSourceSet(clone.sources, readStableSourceSet(databasePath, RECHECKED_SOURCE));
      return result;
    } finally {
      rmSync(clone.cloneDirectory, { recursive: true, force: true });
    }
  });
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
function inspectFabricDatabaseForCutoverOnce(
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
    clone = createPrivateDatabaseClone(databasePath, true);
    try {
      database = new BetterSqlite3(clone.clonePath);
      database.pragma("trusted_schema = OFF");
      const objectCount = userSchemaObjectCount(database);
      if (objectCount === 0) {
        assertSameSourceSet(clone.sources, readStableSourceSet(databasePath, RECHECKED_SOURCE));
        return { state: "empty", sources: clone.sources };
      }

      try {
        assertCurrentSchema(database);
        assertSameSourceSet(clone.sources, readStableSourceSet(databasePath, RECHECKED_SOURCE));
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
        assertSameSourceSet(clone.sources, readStableSourceSet(databasePath, RECHECKED_SOURCE));
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
      assertSameSourceSet(clone.sources, readStableSourceSet(databasePath, RECHECKED_SOURCE));
      // A file SQLite itself rejects as malformed or not a database IS an
      // incompatible database, and archive-and-fresh exists to repair exactly
      // that. Only such native verdicts are classified; every other failure
      // (permissions, I/O) propagates rather than being relabelled.
      if (!sqliteFormatVerdict(error)) throw error;
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

export function inspectFabricDatabaseForCutover(
  databasePath: string,
): FabricDatabaseCutoverInspection {
  return retryUnstableDatabaseInspection(
    () => inspectFabricDatabaseForCutoverOnce(databasePath),
  );
}

/**
 * Coordination tables whose rows a schema cutover would move out of the live
 * database. The list is fixed rather than derived so an incompatible schema
 * cannot widen what a gate prompt reads, and every entry degrades to `null`
 * when that epoch never defined the table.
 */
export const RETAINED_WORK_TABLES = [
  "projects",
  "project_sessions",
  "runs",
  "agents",
  "tasks",
  "messages",
  "receipts",
] as const;

export type RetainedWorkCensus = Readonly<{
  /** `rows` is null when the table is absent or unreadable at this epoch. */
  tables: readonly Readonly<{ table: string; rows: number | null }>[];
}>;

/**
 * Counts the coordination rows a cutover would displace, through the same
 * read-only private-clone boundary as the cutover inspection. It never opens
 * the live database and never reports a count it could not read.
 */
function inspectRetainedWorkOnce(databasePath: string): RetainedWorkCensus {
  let database: Database.Database | undefined;
  let clone: ReturnType<typeof createPrivateDatabaseClone> | undefined;
  try {
    clone = createPrivateDatabaseClone(databasePath);
    database = new BetterSqlite3(clone.clonePath, { readonly: true });
    database.pragma("trusted_schema = OFF");
    const reader = database;
    return {
      tables: RETAINED_WORK_TABLES.map((table) => {
        try {
          const row: unknown = reader.prepare(`SELECT count(*) AS rows FROM "${table}"`).get();
          const rows = typeof row === "object" && row !== null && "rows" in row ? row.rows : undefined;
          return { table, rows: Number.isSafeInteger(rows) ? rows as number : null };
        } catch {
          return { table, rows: null };
        }
      }),
    };
  } catch (error: unknown) {
    if (
      error instanceof SchemaBaselineError &&
      error.code === "DATABASE_INSPECTION_UNSTABLE"
    ) throw error;
    return { tables: RETAINED_WORK_TABLES.map((table) => ({ table, rows: null })) };
  } finally {
    try {
      database?.close();
    } finally {
      if (clone !== undefined) rmSync(clone.cloneDirectory, { recursive: true, force: true });
    }
  }
}

export function inspectRetainedWork(databasePath: string): RetainedWorkCensus {
  return retryUnstableDatabaseInspection(() => inspectRetainedWorkOnce(databasePath));
}

function inspectFabricDatabaseOnce(databasePath: string): FabricDatabaseInspection {
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
    clone = createPrivateDatabaseClone(databasePath, true);
    // SQLite may recover/checkpoint its private WAL or journal and may create a
    // private SHM file. It has no writable relationship with the source path.
    database = new BetterSqlite3(clone.clonePath);
    database.pragma("trusted_schema = OFF");
    assertCurrentSchema(database);
    assertSameSourceSet(clone.sources, readStableSourceSet(databasePath, RECHECKED_SOURCE));
  } catch (error: unknown) {
    if (error instanceof SchemaBaselineError) {
      if (error.code === "SCHEMA_CUTOVER_REQUIRED" && clone !== undefined) {
        assertSameSourceSet(clone.sources, readStableSourceSet(databasePath, RECHECKED_SOURCE));
      }
      throw error;
    }
    if (clone !== undefined) {
      assertSameSourceSet(clone.sources, readStableSourceSet(databasePath, RECHECKED_SOURCE));
    }
    throw error;
  } finally {
    try {
      database?.close();
    } finally {
      if (clone !== undefined) rmSync(clone.cloneDirectory, { recursive: true, force: true });
    }
  }
  let after;
  try {
    after = lstatSync(databasePath);
  } catch (error: unknown) {
    if (errno(error, "ENOENT")) {
      throw inspectionUnstable(
        "database source disappeared during read-only schema inspection",
        error,
      );
    }
    throw error;
  }
  if (
    before.dev !== after.dev ||
    before.ino !== after.ino ||
    before.size !== after.size ||
    before.mtimeMs !== after.mtimeMs ||
    before.ctimeMs !== after.ctimeMs
  ) {
    throw inspectionUnstable("database identity changed during read-only schema inspection");
  }
  return { state: "current" };
}

/** Read-only cutover gate used before any daemon/runtime mutation. */
export function inspectFabricDatabase(databasePath: string): FabricDatabaseInspection {
  return retryUnstableDatabaseInspection(() => inspectFabricDatabaseOnce(databasePath));
}
