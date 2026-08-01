export const PRIMARY_ADAPTER_IDS = ["claude-agent-sdk", "codex-app-server"] as const;

export type PrimaryAdapterId = typeof PRIMARY_ADAPTER_IDS[number];

export function isPrimaryAdapter(adapterId: string): adapterId is PrimaryAdapterId {
  return (PRIMARY_ADAPTER_IDS as readonly string[]).includes(adapterId);
}
