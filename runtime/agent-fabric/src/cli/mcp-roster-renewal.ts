import type { McpSeat } from "./seat-store.js";

// The warning starts at seven days. Extending the stored expiry by 23 days
// therefore always requests a future expiry no more than 30 days from now.
const ROSTER_RENEWAL_EXTENSION_MS = 23 * 24 * 60 * 60 * 1_000;

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

export function mcpRosterRenewalCommand(input: {
  project: string;
  peerSeat: McpSeat;
  currentExpiresAt: string;
}): string {
  const currentExpiresAt = Date.parse(input.currentExpiresAt);
  if (!Number.isFinite(currentExpiresAt)) throw new Error("MCP roster expiry is invalid");
  const expiresAt = new Date(currentExpiresAt + ROSTER_RENEWAL_EXTENSION_MS).toISOString();
  return `"$HOME/.agents/scripts/agent-fabric" mcp peer-provision ` +
    `--project ${shellQuote(input.project)} --seat ${input.peerSeat} --expires-at ${expiresAt}`;
}

export function mcpBootstrapRenewalCommand(project: string, chairSeat: McpSeat): string {
  return `cd ${shellQuote(project)} && "$HOME/.agents/scripts/agent-fabric" bootstrap --seat ${chairSeat}`;
}
