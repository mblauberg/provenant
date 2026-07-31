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

// One readonly connection serving every renewal lookup within a single MCP
// request or status composition. The owner opens a fresh port per request and
// closes it in a finally, so nothing is cached across daemon generation
// changes: the next request always observes the database anew. The connection
// itself opens lazily on first lookup, so a request whose fast path never
// reaches the database opens nothing, and it is then reused so a roster of
// seats costs one open instead of one per lookup. One connection, one scope:
// not a pool.
export type RosterReadPort = {
  seatGenerationMintedAt(generation: string): string | null;
  chairAuthorityExpiresAt(input: { runId: string; chairAgentId: string }): string | null;
  close(): void;
};

export function openRosterReadPort(databasePath: string): RosterReadPort {
  let connection: Database.Database | null | undefined;
  let closed = false;
  const database = (): Database.Database | null => {
    if (closed) throw new Error("MCP roster read port is closed");
    if (connection === undefined) {
      try {
        connection = new Database(databasePath, { readonly: true, fileMustExist: true });
      } catch {
        // An absent or unreadable database yields null lookups for the rest
        // of this request, matching the per-lookup readers this port replaced.
        connection = null;
      }
    }
    return connection;
  };
  return {
    // The mint time is read from the daemon's generation record rather than
    // stored in seat metadata, because replayed generations must stay
    // byte-identical on disk while created_at is already immutable per
    // generation in the database.
    seatGenerationMintedAt(generation: string): string | null {
      const reader = database();
      if (reader === null) return null;
      try {
        const row = reader.prepare(
          "SELECT created_at FROM mcp_seat_generations WHERE generation=?",
        ).get(generation) as { created_at?: unknown } | undefined;
        return typeof row?.created_at === "number" && Number.isFinite(row.created_at)
          ? new Date(row.created_at).toISOString()
          : null;
      } catch {
        return null;
      }
    },
    chairAuthorityExpiresAt(input: { runId: string; chairAgentId: string }): string | null {
      const reader = database();
      if (reader === null) return null;
      try {
        const row = reader.prepare(`
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
      }
    },
    close(): void {
      if (closed) return;
      closed = true;
      connection?.close();
      connection = undefined;
    },
  };
}

export function seatExpiryWarningDue(input: {
  port: RosterReadPort;
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
    mintedAt: input.port.seatGenerationMintedAt(input.generation),
    expiresAt: input.expiresAt,
    now,
  });
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
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
