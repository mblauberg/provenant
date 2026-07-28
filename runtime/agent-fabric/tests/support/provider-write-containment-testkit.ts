import { realpathSync } from "node:fs";
import { resolve } from "node:path";

export const CONTAINMENT_CASE_IDS = [
  "positive-owned-write",
  "filesystem-path-escapes",
  "filesystem-subprocess-escapes",
  "filesystem-native-edit-escapes",
  "filesystem-git-c-escape",
  "filesystem-symlink-escapes",
  "filesystem-symlink-swap",
  "git-metadata-mutations",
  "unreceipted-temp-writes",
  "denied-path-and-credential-reads",
  "network-tool-egress",
  "hostile-settings-cannot-widen",
  "synthetic-secret-exfiltration",
  "admission-rejects-raw-controls",
  "admission-rejects-external-effects",
  "lifecycle-revoked",
  "lifecycle-expired",
  "lifecycle-owner-generation-changed",
  "lifecycle-write-lease-removed",
  "lifecycle-restart-before-execution",
  "lifecycle-restart-after-provider-acceptance",
] as const;

export const EXECUTION_MODES = ["fresh", "resume"] as const;

export const BOUND_FIXTURE_VARIABLES = [
  "$AFTER_ACCEPTANCE_CUT",
  "$BEFORE_EXECUTION_CUT",
  "$CLAUDE_CONFIG_DIR",
  "$CREDENTIALS",
  "$CREDENTIAL_HARDLINK",
  "$GIT_COMMON",
  "$HOST_TEMP_TARGET",
  "$HTTP_TRAP",
  "$LIFECYCLE_SETUP",
  "$LIFECYCLE_TARGET",
  "$LINK_GIT_COMMON",
  "$LINK_HOME",
  "$LINK_OUTSIDE",
  "$LINK_SIBLING",
  "$LINK_SWAP",
  "$NATIVE_WRITE_TOOLS",
  "$OUTSIDE",
  "$PILOT",
  "$PRIMARY",
  "$PRIVATE_TEMP",
  "$SIBLING",
  "$SYNTHETIC_HOME",
  "$TCP_TRAP",
  "$TEMP_TARGET",
  "$TEMP_WRITE_TARGETS",
  "$TMPDIR",
  "$TOOL",
  "$UNIX_TRAP",
] as const;

export type ContainmentTuple = Readonly<{
  operation: string;
  target: string;
  status: string;
}>;

export type ContainmentCase = Readonly<{
  id: typeof CONTAINMENT_CASE_IDS[number];
  specLine: number;
  failureMapClass: "T" | "A" | "L";
  expected: ContainmentTuple[];
  turnStatus?: string;
  acceptedEffectStatuses?: string[];
}>;

type ClaudeSettings = Readonly<{
  env?: Record<string, string | undefined>;
  sandbox?: {
    filesystem?: {
      allowWrite?: string[];
      denyWrite?: string[];
      allowRead?: string[];
      allowManagedReadPathsOnly?: boolean;
    };
    network?: {
      allowedDomains?: string[];
      allowManagedDomainsOnly?: boolean;
      allowUnixSockets?: string[];
      allowAllUnixSockets?: boolean;
      allowLocalBinding?: boolean;
    };
  };
}>;

type CodexSettings = Readonly<{
  environments?: unknown[];
  sandboxPolicy?: {
    type?: string;
    writableRoots?: string[];
    networkAccess?: boolean;
    excludeTmpdirEnvVar?: boolean;
    excludeSlashTmp?: boolean;
  };
}>;

function isNetworkOperation(operation: string): boolean {
  return [
    "http-connect",
    "tcp-connect",
    "dns-lookup",
    "loopback-connect",
    "unix-connect",
    "socket-bind",
    "proxy-connect",
  ].includes(operation);
}

function isReadOperation(operation: string): boolean {
  return operation === "read" || operation === "read-secret";
}

function targetIsInsidePilot(target: string): boolean {
  if (target.startsWith("$LINK_")) return true;
  if (target === "$CREDENTIAL_HARDLINK") return true;
  return target === "$PILOT" || target.startsWith("$PILOT/");
}

function targetIsEnvironment(target: string): boolean {
  return target.startsWith("env:");
}

// Accepts `undefined` because the callers index a possibly-empty list. An absent
// entry is not a filesystem root, and the length check beside it already rejects
// that shape; narrowing here would only push the same check to both call sites.
function isFilesystemRoot(root: string | undefined): boolean {
  return root === "/" || root === "";
}

/**
 * Declarative model of the vendor settings requested by the adapters.
 *
 * It does not execute a syscall and therefore can only support a
 * `projection-only` verdict. Keeping the model here, separate from adapter
 * production code, makes every assumption reviewable.
 */
export function wouldDenyClaude(
  nativeSettings: unknown,
  tuple: ContainmentTuple,
): boolean {
  const settings = nativeSettings as ClaudeSettings;
  if (isNetworkOperation(tuple.operation)) {
    return settings.sandbox?.network?.allowManagedDomainsOnly === true &&
      settings.sandbox.network.allowedDomains?.length === 0 &&
      settings.sandbox.network.allowUnixSockets?.length === 0 &&
      settings.sandbox.network.allowAllUnixSockets === false &&
      settings.sandbox.network.allowLocalBinding === false;
  }
  if (targetIsEnvironment(tuple.target)) {
    return settings.env !== undefined &&
      !Object.hasOwn(settings.env, tuple.target.slice("env:".length));
  }
  if (isReadOperation(tuple.operation)) {
    return settings.sandbox?.filesystem?.allowManagedReadPathsOnly === true &&
      settings.sandbox.filesystem.allowRead?.length === 1 &&
      !targetIsInsidePilot(tuple.target);
  }
  const allowed = settings.sandbox?.filesystem?.allowWrite ?? [];
  return allowed.length !== 1 || isFilesystemRoot(allowed[0]) || !targetIsInsidePilot(tuple.target);
}

export function wouldDenyCodex(
  nativeSettings: unknown,
  tuple: ContainmentTuple,
): boolean {
  const settings = nativeSettings as CodexSettings;
  if (isNetworkOperation(tuple.operation)) {
    return settings.sandboxPolicy?.networkAccess === false;
  }
  if (targetIsEnvironment(tuple.target)) {
    return settings.environments?.length === 0;
  }
  if (isReadOperation(tuple.operation)) {
    return false;
  }
  if (tuple.operation === "write-temp") {
    return settings.sandboxPolicy?.excludeTmpdirEnvVar === true &&
      settings.sandboxPolicy.excludeSlashTmp === true;
  }
  const writable = settings.sandboxPolicy?.writableRoots ?? [];
  return writable.length !== 1 || isFilesystemRoot(writable[0]) || !targetIsInsidePilot(tuple.target);
}

export function assertDistinctTempRoots(tmpdir: string, privateTemp: string): void {
  if (realpathSync(tmpdir) === realpathSync(privateTemp)) {
    throw new Error("$TMPDIR and $PRIVATE_TEMP must have different canonical paths");
  }
}

export function hydrateTarget(target: string, pilot: string): string {
  return target.replace("$PILOT", resolve(pilot));
}
