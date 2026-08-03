export const PRIMARY_ADAPTER_IDS = ["claude-agent-sdk", "codex-app-server"] as const;

export function isPrimaryAdapter(adapterId: string): boolean {
  return (PRIMARY_ADAPTER_IDS as readonly string[]).includes(adapterId);
}

export function isMandatoryPrimaryAdapter(
  adapterId: string,
  activeAdapterIds: readonly string[] = PRIMARY_ADAPTER_IDS,
): boolean {
  return isPrimaryAdapter(adapterId) && activeAdapterIds.includes(adapterId);
}
