import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import type { AuthorityInput } from "../../src/domain/types.ts";
import { parseMcpPeerProvisionArguments, peerExpiry } from "../../src/cli/mcp-peer-provision.ts";
import {
  MAXIMUM_SEAT_LIFETIME_MS,
  SEAT_EXPIRY_WARNING_CAP_MS,
  mcpRosterRenewalCommand,
  openRosterReadPort,
  seatExpiryWarningDue,
  seatExpiryWarningWindowMs,
} from "../../src/cli/mcp-roster-renewal.ts";
import { ROOT_AUTHORITY } from "../support/stage1-fixture.ts";
import { shellCommandArguments } from "../support/shell-command-arguments.ts";

const DAY_MS = 24 * 60 * 60 * 1_000;
const cleanup: string[] = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map(async (path) => rm(path, { recursive: true, force: true })));
});

function generationDatabase(root: string, generation: string, createdAt: number): string {
  const databasePath = join(root, "fabric-v1.sqlite3");
  const database = new Database(databasePath);
  try {
    // Only the columns the reader touches; the production schema is wider.
    database.exec(
      "CREATE TABLE IF NOT EXISTS mcp_seat_generations (generation TEXT PRIMARY KEY, created_at INTEGER NOT NULL)",
    );
    database.prepare(
      "INSERT INTO mcp_seat_generations(generation, created_at) VALUES (?, ?)",
    ).run(generation, createdAt);
  } finally {
    database.close();
  }
  return databasePath;
}

// The one warning check in these tests still opens the port itself, so every
// assertion exercises the request-scoped surface production code passes in.
function warningDue(input: {
  databasePath: string;
  generation: string;
  expiresAt: string;
  now: number;
}): boolean {
  const port = openRosterReadPort(input.databasePath);
  try {
    return seatExpiryWarningDue({
      port,
      generation: input.generation,
      expiresAt: input.expiresAt,
      now: input.now,
    });
  } finally {
    port.close();
  }
}

async function emittedExpiry(input: {
  now: number;
  currentExpiresAt: string;
  chairAuthorityExpiresAt: string | null;
}): Promise<string | null> {
  const command = mcpRosterRenewalCommand({
    project: "/fixture/project with spaces",
    peerSeat: "agy",
    productRoot: "/fixture/product",
    ...input,
  });
  if (command === null) return null;
  expect(command).toContain("'/fixture/product/scripts/agent-fabric'");

  const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-roster-renewal-"));
  cleanup.push(temporaryRoot);
  const commandArguments = await shellCommandArguments(command, temporaryRoot);
  expect(commandArguments.slice(0, 2)).toEqual(["mcp", "peer-provision"]);
  const parsed = parseMcpPeerProvisionArguments(commandArguments.slice(2));
  expect(parsed).toMatchObject({
    project: "/fixture/project with spaces",
    seats: ["agy"],
  });
  expect(parsed.expiresAt).toBeDefined();
  // An unknown ceiling has no authority to validate against, so the two bounds that
  // do not depend on it are checked directly instead of through peerExpiry.
  const expiresAt = input.chairAuthorityExpiresAt === null
    ? String(parsed.expiresAt)
    : peerExpiry(
        parsed.expiresAt,
        { expiresAt: input.chairAuthorityExpiresAt } as AuthorityInput,
        input.currentExpiresAt,
      );
  expect(Date.parse(expiresAt) - Date.now()).toBeLessThanOrEqual(MAXIMUM_SEAT_LIFETIME_MS);
  expect(Date.parse(expiresAt)).toBeGreaterThan(Date.now());
  return expiresAt;
}

describe("MCP roster renewal command", () => {
  it("extends a normal roster expiry by 23 days", async () => {
    const now = Date.now();
    const currentExpiresAt = new Date(now + DAY_MS).toISOString();
    const expiresAt = await emittedExpiry({
      now,
      currentExpiresAt,
      chairAuthorityExpiresAt: new Date(now + 365 * DAY_MS).toISOString(),
    });

    expect(expiresAt).toBe(new Date(Date.parse(currentExpiresAt) + 23 * DAY_MS).toISOString());
  });

  it("clamps renewal to a chair authority ceiling ten days away", async () => {
    const now = Date.now();
    const chairAuthorityExpiresAt = new Date(now + 10 * DAY_MS).toISOString();
    const expiresAt = await emittedExpiry({
      now,
      currentExpiresAt: new Date(now + DAY_MS).toISOString(),
      chairAuthorityExpiresAt,
    });

    expect(expiresAt).toBe(chairAuthorityExpiresAt);
  });

  it("clamps renewal to a chair authority ceiling three days away", async () => {
    const now = Date.now();
    const chairAuthorityExpiresAt = new Date(now + 3 * DAY_MS).toISOString();
    const expiresAt = await emittedExpiry({
      now,
      currentExpiresAt: new Date(now + DAY_MS).toISOString(),
      chairAuthorityExpiresAt,
    });

    expect(expiresAt).toBe(chairAuthorityExpiresAt);
  });

  it("returns null when the chair authority ceiling is in the past", async () => {
    const now = Date.now();
    await expect(emittedExpiry({
      now,
      currentExpiresAt: new Date(now + DAY_MS).toISOString(),
      chairAuthorityExpiresAt: new Date(now - DAY_MS).toISOString(),
    })).resolves.toBeNull();
  });

  it("still emits a bounded renewal when the chair authority ceiling is unreadable", async () => {
    const now = Date.now();
    const expiresAt = await emittedExpiry({
      now,
      currentExpiresAt: new Date(now + DAY_MS).toISOString(),
      chairAuthorityExpiresAt: null,
    });

    expect(expiresAt).toBe(new Date(now + DAY_MS + 23 * DAY_MS).toISOString());
  });

  it("clamps a roster already near the maximum lifetime to both bounds", async () => {
    const now = Date.now();
    const chairAuthorityExpiresAt = new Date(now + MAXIMUM_SEAT_LIFETIME_MS).toISOString();
    const expiresAt = await emittedExpiry({
      now,
      currentExpiresAt: new Date(now + 30 * DAY_MS).toISOString(),
      chairAuthorityExpiresAt,
    });

    expect(expiresAt).toBe(chairAuthorityExpiresAt);
    expect(Date.parse(String(expiresAt)) - now).toBe(MAXIMUM_SEAT_LIFETIME_MS);
  });

  it("emits a recovery expiry measured from now for an expired roster", async () => {
    const now = Date.now();
    const expiresAt = await emittedExpiry({
      now,
      currentExpiresAt: new Date(now - 30 * DAY_MS).toISOString(),
      chairAuthorityExpiresAt: new Date(now + 365 * DAY_MS).toISOString(),
    });

    expect(expiresAt).toBe(new Date(now + 23 * DAY_MS).toISOString());
  });
});

describe("seat expiry warning window", () => {
  it("scales the window to the final quarter of the roster's lifetime", () => {
    const now = Date.now();
    const window = seatExpiryWarningWindowMs({
      mintedAt: new Date(now - 1 * 60 * 60 * 1_000).toISOString(),
      expiresAt: new Date(now + 23 * 60 * 60 * 1_000).toISOString(),
    });

    expect(window).toBe(6 * 60 * 60 * 1_000);
  });

  it("caps the window at seven days for a long-lived roster", () => {
    const now = Date.now();
    expect(seatExpiryWarningWindowMs({
      mintedAt: new Date(now).toISOString(),
      expiresAt: new Date(now + MAXIMUM_SEAT_LIFETIME_MS).toISOString(),
    })).toBe(SEAT_EXPIRY_WARNING_CAP_MS);
  });

  it.each([
    ["missing", null],
    ["unparseable", "not-a-time"],
    ["not before the expiry", "2099-01-01T00:00:00.000Z"],
  ])("falls back to the capped window when the mint time is %s", (_case, mintedAt) => {
    expect(seatExpiryWarningWindowMs({
      mintedAt,
      expiresAt: "2099-01-01T00:00:00.000Z",
    })).toBe(SEAT_EXPIRY_WARNING_CAP_MS);
  });

  it("falls back to the capped window when the mint time is in the future", () => {
    // A corrupted-but-parseable future created_at is not a lifetime. Honouring
    // it here would yield a (10h - 1h) / 4 window and silently shrink the
    // warning period, so it must fall back to legacy fixed-cap behaviour.
    const now = Date.now();
    expect(seatExpiryWarningWindowMs({
      mintedAt: new Date(now + 60 * 60 * 1_000).toISOString(),
      expiresAt: new Date(now + 10 * 60 * 60 * 1_000).toISOString(),
      now,
    })).toBe(SEAT_EXPIRY_WARNING_CAP_MS);
  });

  it("keeps warning under the capped window when created_at is corrupted into the future", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-warning-window-future-mint-"));
    cleanup.push(root);
    const now = Date.now();
    const generation = "c".repeat(64);
    // created_at five days in the future, expiry six days away: the corrupted
    // lifetime quarter would be six hours and the roster would stay silent for
    // almost its whole remaining life; legacy capped behaviour warns now.
    const databasePath = generationDatabase(root, generation, now + 5 * 24 * 60 * 60 * 1_000);

    expect(warningDue({
      databasePath,
      generation,
      expiresAt: new Date(now + 6 * 24 * 60 * 60 * 1_000).toISOString(),
      now,
    })).toBe(true);
  });

  it("keeps a fresh 24 hour roster out of the warning window for three quarters of its life", async () => {
    // Under a fixed 7 day threshold this roster would warn from mint to
    // expiry, so the warning would carry no information (#526).
    const root = await mkdtemp(join(tmpdir(), "fabric-warning-window-"));
    cleanup.push(root);
    const now = Date.now();
    const generation = "a".repeat(64);
    const databasePath = generationDatabase(root, generation, now - 60 * 60 * 1_000);
    const expiresAt = new Date(now + 23 * 60 * 60 * 1_000).toISOString();

    expect(warningDue({ databasePath, generation, expiresAt, now })).toBe(false);
    expect(warningDue({
      databasePath,
      generation,
      expiresAt,
      now: now + 18 * 60 * 60 * 1_000,
    })).toBe(true);
  });

  it("treats an expired roster as due and an unreadable database as the capped window", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-warning-window-fallback-"));
    cleanup.push(root);
    const now = Date.now();
    const generation = "b".repeat(64);
    const missingDatabasePath = join(root, "missing.sqlite3");

    expect(warningDue({
      databasePath: generationDatabase(root, generation, now - 60 * 60 * 1_000),
      generation,
      expiresAt: new Date(now - 1_000).toISOString(),
      now,
    })).toBe(true);
    expect(warningDue({
      databasePath: missingDatabasePath,
      generation,
      expiresAt: new Date(now + 6 * 24 * 60 * 60 * 1_000).toISOString(),
      now,
    })).toBe(true);
    expect(warningDue({
      databasePath: missingDatabasePath,
      generation,
      expiresAt: new Date(now + 8 * 24 * 60 * 60 * 1_000).toISOString(),
      now,
    })).toBe(false);
  });
});

describe("roster read port", () => {
  function canonicalJson(value: unknown): string {
    if (value === null || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
      return JSON.stringify(value);
    }
    if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
    if (typeof value === "object") {
      const record = value as Record<string, unknown>;
      return `{${Object.keys(record).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`).join(",")}}`;
    }
    throw new TypeError("roster read port fixture value is not JSON-compatible");
  }

  function authorityDatabase(root: string): string {
    const databasePath = join(root, "fabric-v1.sqlite3");
    const database = new Database(databasePath);
    try {
      // Only the columns the reader touches; the production schema is wider.
      database.exec(
        "CREATE TABLE agents (run_id TEXT, agent_id TEXT, authority_id TEXT);" +
        "CREATE TABLE authorities (run_id TEXT, authority_id TEXT, authority_json TEXT, authority_hash TEXT)",
      );
      const authorityJson = canonicalJson(ROOT_AUTHORITY);
      database.prepare(
        "INSERT INTO agents(run_id, agent_id, authority_id) VALUES (?, ?, ?)",
      ).run("run-port", "codex-chair", "authority-port");
      database.prepare(
        "INSERT INTO authorities(run_id, authority_id, authority_json, authority_hash) VALUES (?, ?, ?, ?)",
      ).run(
        "run-port",
        "authority-port",
        authorityJson,
        createHash("sha256").update(authorityJson).digest("hex"),
      );
    } finally {
      database.close();
    }
    return databasePath;
  }

  it("reads the chair authority expiry and answers a missing agent with null", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-roster-read-port-"));
    cleanup.push(root);
    const port = openRosterReadPort(authorityDatabase(root));
    try {
      expect(port.chairAuthorityExpiresAt({ runId: "run-port", chairAgentId: "codex-chair" }))
        .toBe(ROOT_AUTHORITY.expiresAt);
      expect(port.chairAuthorityExpiresAt({ runId: "run-port", chairAgentId: "absent" })).toBeNull();
    } finally {
      port.close();
    }
  });

  it("serves every lookup in its scope over the one connection it opened", async () => {
    // The database file is unlinked between the two lookups, so only the
    // connection the first lookup opened can answer the second: a port that
    // reopened per lookup would return null instead.
    const root = await mkdtemp(join(tmpdir(), "fabric-roster-read-port-reuse-"));
    cleanup.push(root);
    const generation = "e".repeat(64);
    const createdAt = Date.now() - 60 * 60 * 1_000;
    const databasePath = generationDatabase(root, generation, createdAt);
    const port = openRosterReadPort(databasePath);
    try {
      expect(port.seatGenerationMintedAt(generation)).toBe(new Date(createdAt).toISOString());
      await rm(databasePath);
      expect(port.seatGenerationMintedAt(generation)).toBe(new Date(createdAt).toISOString());
    } finally {
      port.close();
    }
  });

  it("answers every lookup with null when the database is absent", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-roster-read-port-absent-"));
    cleanup.push(root);
    const port = openRosterReadPort(join(root, "missing.sqlite3"));
    try {
      expect(port.seatGenerationMintedAt("f".repeat(64))).toBeNull();
      expect(port.chairAuthorityExpiresAt({ runId: "run-port", chairAgentId: "codex-chair" })).toBeNull();
    } finally {
      port.close();
    }
  });

  it("refuses lookups once closed and tolerates a second close", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-roster-read-port-closed-"));
    cleanup.push(root);
    const generation = "a".repeat(64);
    const port = openRosterReadPort(generationDatabase(root, generation, Date.now()));
    expect(port.seatGenerationMintedAt(generation)).not.toBeNull();
    port.close();
    port.close();
    expect(() => port.seatGenerationMintedAt(generation)).toThrow("MCP roster read port is closed");
  });
});
