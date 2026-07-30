import Database from "better-sqlite3";

import { readStoredAuthority } from "../authority/stored-authority.js";
import { fabricCliCommand } from "./root-resolution.js";
import type { McpSeat } from "./seat-store.js";

// The warning starts at seven days. Extending the stored expiry by 23 days
// therefore always requests a future expiry no more than 30 days from now.
const ROSTER_RENEWAL_EXTENSION_MS = 23 * 24 * 60 * 60 * 1_000;

// Owned here rather than in mcp-peer-provision so that the emitter of a renewal
// command and the validator of that command read one definition, and so that the
// import between the two modules stays one-directional.
export const MAXIMUM_SEAT_LIFETIME_MS = 31 * 24 * 60 * 60 * 1_000;

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
  productRoot?: string;
}): string | null {
  const currentExpiresAt = Date.parse(input.currentExpiresAt);
  if (!Number.isFinite(currentExpiresAt)) throw new Error("MCP roster expiry is invalid");
  const expiresAt = validateAndClampPeerExpiry({
    requested: new Date(currentExpiresAt + ROSTER_RENEWAL_EXTENSION_MS).toISOString(),
    chairAuthorityExpiresAt: input.chairAuthorityExpiresAt,
    now: input.now ?? Date.now(),
  });
  if (expiresAt === null) return null;
  return `${fabricCliCommand({ productRootFlag: input.productRoot })} mcp peer-provision ` +
    `--project ${shellQuote(input.project)} --seat ${input.peerSeat} --expires-at ${expiresAt}`;
}

export function mcpBootstrapRenewalCommand(
  project: string,
  chairSeat: McpSeat,
  productRoot?: string,
): string {
  return `cd ${shellQuote(project)} && ${fabricCliCommand({ productRootFlag: productRoot })} bootstrap --seat ${chairSeat}`;
}
