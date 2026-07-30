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
  seatExpiryWarningDue,
  seatExpiryWarningWindowMs,
} from "../../src/cli/mcp-roster-renewal.ts";
import { shellCommandArguments } from "../support/shell-command-arguments.ts";

const DAY_MS = 24 * 60 * 60 * 1_000;
const cleanup: string[] = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map(async (path) => rm(path, { recursive: true, force: true })));
});

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

  it("keeps a fresh 24 hour roster out of the warning window for three quarters of its life", async () => {
    // Under a fixed 7 day threshold this roster would warn from mint to
    // expiry, so the warning would carry no information (#526).
    const root = await mkdtemp(join(tmpdir(), "fabric-warning-window-"));
    cleanup.push(root);
    const now = Date.now();
    const generation = "a".repeat(64);
    const databasePath = generationDatabase(root, generation, now - 60 * 60 * 1_000);
    const expiresAt = new Date(now + 23 * 60 * 60 * 1_000).toISOString();

    expect(seatExpiryWarningDue({ databasePath, generation, expiresAt, now })).toBe(false);
    expect(seatExpiryWarningDue({
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

    expect(seatExpiryWarningDue({
      databasePath: generationDatabase(root, generation, now - 60 * 60 * 1_000),
      generation,
      expiresAt: new Date(now - 1_000).toISOString(),
      now,
    })).toBe(true);
    expect(seatExpiryWarningDue({
      databasePath: missingDatabasePath,
      generation,
      expiresAt: new Date(now + 6 * 24 * 60 * 60 * 1_000).toISOString(),
      now,
    })).toBe(true);
    expect(seatExpiryWarningDue({
      databasePath: missingDatabasePath,
      generation,
      expiresAt: new Date(now + 8 * 24 * 60 * 60 * 1_000).toISOString(),
      now,
    })).toBe(false);
  });
});
