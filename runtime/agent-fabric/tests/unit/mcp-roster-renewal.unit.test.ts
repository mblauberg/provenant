import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import type { AuthorityInput } from "../../src/domain/types.ts";
import { parseMcpPeerProvisionArguments, peerExpiry } from "../../src/cli/mcp-peer-provision.ts";
import {
  MAXIMUM_SEAT_LIFETIME_MS,
  mcpRosterRenewalCommand,
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
});
