import Database from "better-sqlite3";

import { readStoredAuthority } from "../authority/stored-authority.js";
import { fabricCliCommand } from "../domain/fabric-roots.js";
import type { McpSeat } from "./seat-store.js";

// The warning window never exceeds seven days. Extending the effective expiry
// by 23 days therefore always requests a future expiry no more than 30 days
// from now.
const ROSTER_RENEWAL_EXTENSION_MS = 23 * 24 * 60 * 60 * 1_000;

// Owned here rather than in mcp-peer-provision so that the emitter of a renewal
// command and the validator of that command read one definition, and so that the
// import between the two modules stays one-directional.
export const MAXIMUM_SEAT_LIFETIME_MS = 31 * 24 * 60 * 60 * 1_000;

// A fixed warning threshold can exceed a roster's whole lifetime: a 24 hour
// bootstrap roster under a fixed 7 day threshold warns from mint to expiry and
// the warning carries no information (#526). The window is therefore the final
// quarter of the roster's own recorded lifetime, capped at seven days so a
// long-lived roster still warns a bounded time before expiry.
export const SEAT_EXPIRY_WARNING_CAP_MS = 7 * 24 * 60 * 60 * 1_000;
const SEAT_EXPIRY_WARNING_LIFETIME_DIVISOR = 4;

export function seatExpiryWarningWindowMs(input: {
  mintedAt: string | null;
  expiresAt: string;
  now?: number;
}): number {
  const expiresAt = Date.parse(input.expiresAt);
  const mintedAt = input.mintedAt === null ? Number.NaN : Date.parse(input.mintedAt);
  const now = input.now ?? Date.now();
  if (
    !Number.isFinite(expiresAt) ||
    !Number.isFinite(mintedAt) ||
    mintedAt >= expiresAt ||
    mintedAt > now
  ) {
    // Without a readable mint time the lifetime is unknown, so the capped
    // fixed window is the only defensible fallback. A mint time in the future
    // is corruption rather than a lifetime: honouring it would silently
    // shrink the warning window, so it falls back the same way.
    return SEAT_EXPIRY_WARNING_CAP_MS;
  }
  return Math.min(
    (expiresAt - mintedAt) / SEAT_EXPIRY_WARNING_LIFETIME_DIVISOR,
    SEAT_EXPIRY_WARNING_CAP_MS,
  );
}

// The mint time is read from the daemon's generation record rather than stored
// in seat metadata, because replayed generations must stay byte-identical on
// disk while created_at is already immutable per generation in the database.
export function readSeatGenerationMintedAt(input: {
  databasePath: string;
  generation: string;
}): string | null {
  let database: Database.Database | undefined;
  try {
    database = new Database(input.databasePath, { readonly: true, fileMustExist: true });
    const row = database.prepare(
      "SELECT created_at FROM mcp_seat_generations WHERE generation=?",
    ).get(input.generation) as { created_at?: unknown } | undefined;
    return typeof row?.created_at === "number" && Number.isFinite(row.created_at)
      ? new Date(row.created_at).toISOString()
      : null;
  } catch {
    return null;
  } finally {
    database?.close();
  }
}

export function seatExpiryWarningDue(input: {
  databasePath: string;
  generation: string;
  expiresAt: string;
  now?: number;
}): boolean {
  const now = input.now ?? Date.now();
  const remainingMs = Date.parse(input.expiresAt) - now;
  // The window never exceeds the cap, so a roster with more than the cap
  // remaining is cleared without a database read.
  if (Number.isNaN(remainingMs) || remainingMs > SEAT_EXPIRY_WARNING_CAP_MS) return false;
  return remainingMs <= seatExpiryWarningWindowMs({
    mintedAt: readSeatGenerationMintedAt(input),
    expiresAt: input.expiresAt,
    now,
  });
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

export function readChairAuthorityExpiresAt(input: {
  databasePath: string;
  runId: string;
  chairAgentId: string;
}): string | null {
  let database: Database.Database | undefined;
  try {
    database = new Database(input.databasePath, { readonly: true, fileMustExist: true });
    const row = database.prepare(`
      SELECT authority.authority_json, authority.authority_hash
        FROM agents agent
        JOIN authorities authority
          ON authority.run_id=agent.run_id AND authority.authority_id=agent.authority_id
       WHERE agent.run_id=? AND agent.agent_id=?
    `).get(input.runId, input.chairAgentId) as {
      authority_json?: unknown;
      authority_hash?: unknown;
    } | undefined;
    return row === undefined ? null : readStoredAuthority(row, "chair authority").expiresAt;
  } catch {
    return null;
  } finally {
    database?.close();
  }
}

// Clamps a requested expiry to every bound peerExpiry enforces, so that a command
// this module emits cannot be rejected by the command it invokes. A null ceiling
// means the chair authority expiry could not be read, not that it is nearby: the
// two remaining bounds are still applied and the command is still emitted, because
// a rejected peer-provision costs one clear error whereas steering an operator to
// bootstrap discards the roster's peer seats and its lineage.
export function validateAndClampPeerExpiry(input: {
  requested: string;
  chairAuthorityExpiresAt: string | null;
  now: number;
}): string | null {
  const requested = Date.parse(input.requested);
  if (!Number.isFinite(requested) || new Date(requested).toISOString() !== input.requested) {
    return null;
  }
  const ceiling = input.chairAuthorityExpiresAt === null
    ? Number.POSITIVE_INFINITY
    : Date.parse(input.chairAuthorityExpiresAt);
  if (Number.isNaN(ceiling)) return null;

  const expiresAt = Math.min(requested, input.now + MAXIMUM_SEAT_LIFETIME_MS, ceiling);
  return expiresAt > input.now ? new Date(expiresAt).toISOString() : null;
}

export function mcpRosterRenewalCommand(input: {
  project: string;
  peerSeat: McpSeat;
  currentExpiresAt: string;
  chairAuthorityExpiresAt: string | null;
  now?: number;
  productRoot: string;
}): string | null {
  const currentExpiresAt = Date.parse(input.currentExpiresAt);
  if (!Number.isFinite(currentExpiresAt)) throw new Error("MCP roster expiry is invalid");
  const now = input.now ?? Date.now();
  // An expired roster extends from now rather than from its lapsed expiry, so
  // the recovery command stays valid however long ago the roster lapsed (#526).
  const expiresAt = validateAndClampPeerExpiry({
    requested: new Date(Math.max(currentExpiresAt, now) + ROSTER_RENEWAL_EXTENSION_MS).toISOString(),
    chairAuthorityExpiresAt: input.chairAuthorityExpiresAt,
    now,
  });
  if (expiresAt === null) return null;
  return `${fabricCliCommand({ productRootFlag: input.productRoot })} mcp peer-provision ` +
    `--project ${shellQuote(input.project)} --seat ${input.peerSeat} --expires-at ${expiresAt}`;
}

export function mcpBootstrapRenewalCommand(
  project: string,
  chairSeat: McpSeat,
  productRoot: string,
): string {
  return `cd ${shellQuote(project)} && ${fabricCliCommand({ productRootFlag: productRoot })} bootstrap --seat ${chairSeat}`;
}
