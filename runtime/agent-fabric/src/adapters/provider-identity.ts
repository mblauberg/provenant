import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { lstat, readFile, realpath, stat } from "node:fs/promises";
import { dirname, isAbsolute, join, relative } from "node:path";
import { promisify } from "node:util";

import { FabricError } from "../errors.js";
import { ADAPTER_INTERFACE_PROBE_INCOMPLETE } from "./provider-interface.js";

const execFileAsync = promisify(execFile);
const CODESIGN_PROBE_TIMEOUT_MS = 15_000;

const VENDORS = {
  "claude-agent-sdk": { teamId: "Q6L2SF6YDW", identifier: "com.anthropic.claude-code" },
  "codex-app-server": { teamId: "2DC432GLL2", identifier: "codex" },
  agy: { teamId: "EQHXZ8M8AV", identifier: "cli" },
  "kiro-acp": { teamId: "94KV3E626L", identifier: "kiro-cli" },
} as const;

export type ProviderPathObservation = {
  canonicalPath: string;
  regularFile: boolean;
  ownerUid: number;
  mode: number;
  sha256: string;
};

export type ProviderIdentityPort = {
  inspectPath(path: string): Promise<ProviderPathObservation>;
  inspectDirectory(path: string): Promise<{ canonicalPath: string; directory: boolean; ownerUid: number; mode: number }>;
  verifySignature(path: string): Promise<void>;
  signingIdentity(path: string): Promise<{ teamId: string; identifier: string }>;
  currentUid(): number;
};

export type ProviderIdentityObservation = ProviderPathObservation & {
  adapterId: string;
  assurance: "full-vendor-identity" | "partial-signed-helpers" | "owner-controlled-install-root";
  signing: Array<{ path: string; teamId: string; identifier: string }>;
};

function codesignProbeTimedOut(error: unknown): boolean {
  return typeof error === "object" && error !== null && "killed" in error && error.killed === true;
}

async function runCodesign(arguments_: string[], path: string): Promise<{ stdout: string; stderr: string }> {
  try {
    return await execFileAsync("/usr/bin/codesign", [...arguments_, path], {
      encoding: "utf8",
      timeout: CODESIGN_PROBE_TIMEOUT_MS,
      killSignal: "SIGKILL",
    });
  } catch (error: unknown) {
    if (codesignProbeTimedOut(error)) {
      throw new FabricError(
        ADAPTER_INTERFACE_PROBE_INCOMPLETE,
        `provider codesign probe did not complete: ${path}`,
        { cause: error },
      );
    }
    throw error;
  }
}

async function inspectPath(path: string): Promise<ProviderPathObservation> {
  let canonicalPath: string;
  try {
    canonicalPath = await realpath(path);
    const metadata = await stat(canonicalPath);
    const bytes = await readFile(canonicalPath);
    return {
      canonicalPath,
      regularFile: metadata.isFile(),
      ownerUid: metadata.uid,
      mode: metadata.mode & 0o777,
      sha256: createHash("sha256").update(bytes).digest("hex"),
    };
  } catch (error: unknown) {
    throw new FabricError("ADAPTER_ARTIFACT_MISSING", `provider executable is unavailable: ${path}`, { cause: error });
  }
}

async function signingIdentity(path: string): Promise<{ teamId: string; identifier: string }> {
  try {
    const result = await runCodesign(["-dv", "--verbose=4"], path);
    const output = `${result.stdout}\n${result.stderr}`;
    const teamId = /^TeamIdentifier=(.+)$/mu.exec(output)?.[1];
    const identifier = /^Identifier=(.+)$/mu.exec(output)?.[1];
    if (teamId === undefined || identifier === undefined) throw new Error("codesign identity fields are missing");
    return { teamId, identifier };
  } catch (error: unknown) {
    if (error instanceof FabricError) throw error;
    throw new FabricError("ADAPTER_IDENTITY_MISMATCH", `provider signing identity is unavailable: ${path}`, { cause: error });
  }
}

async function verifySignature(path: string): Promise<void> {
  try {
    await runCodesign(["--verify", "--strict"], path);
  } catch (error: unknown) {
    if (error instanceof FabricError) throw error;
    throw new FabricError("ADAPTER_IDENTITY_MISMATCH", `provider signature is invalid: ${path}`, { cause: error });
  }
}

const SYSTEM_PORT: ProviderIdentityPort = {
  inspectPath,
  inspectDirectory: async (path) => {
    const canonicalPath = await realpath(path);
    const metadata = await lstat(canonicalPath);
    return { canonicalPath, directory: metadata.isDirectory(), ownerUid: metadata.uid, mode: metadata.mode & 0o777 };
  },
  verifySignature,
  signingIdentity,
  currentUid: () => process.getuid?.() ?? -1,
};

function assertSafeFile(observation: ProviderPathObservation, expectedOwner?: number): void {
  if (!observation.regularFile || (observation.mode & 0o022) !== 0 ||
      (expectedOwner !== undefined && observation.ownerUid !== expectedOwner)) {
    throw new FabricError("ADAPTER_PATH_UNSAFE", `provider executable path is not a safe regular file: ${observation.canonicalPath}`);
  }
}

function assertSigning(actual: { teamId: string; identifier: string }, expected: { teamId: string; identifier?: string }): void {
  if (actual.teamId !== expected.teamId || (expected.identifier !== undefined && actual.identifier !== expected.identifier)) {
    throw new FabricError("ADAPTER_IDENTITY_MISMATCH", "provider signing identity does not match the expected vendor");
  }
}

async function verifiedSigningIdentity(port: ProviderIdentityPort, path: string): Promise<{ teamId: string; identifier: string }> {
  try {
    await port.verifySignature(path);
  } catch (error: unknown) {
    if (error instanceof FabricError) throw error;
    throw new FabricError("ADAPTER_IDENTITY_MISMATCH", `provider signature is invalid: ${path}`, { cause: error });
  }
  return await port.signingIdentity(path);
}

/**
 * Re-resolves and verifies the provider launcher immediately before provider
 * spawn. Digests are returned as observations and are deliberately not an
 * admission input: normal vendor updates must not require a registry edit.
 */
export async function verifyProviderExecutableIdentity(input: {
  adapterId: string;
  executable: string;
  cursorInstallRoot?: string;
  providerInstallRoot?: string;
}, port: ProviderIdentityPort = SYSTEM_PORT): Promise<ProviderIdentityObservation> {
  if (!isAbsolute(input.executable)) {
    throw new FabricError("ADAPTER_PATH_UNSAFE", `provider executable must be absolute: ${input.executable}`);
  }
  const executable = await port.inspectPath(input.executable);
  const expectedVendor = VENDORS[input.adapterId as keyof typeof VENDORS];
  if (expectedVendor !== undefined) {
    assertSafeFile(executable);
    const signing = await verifiedSigningIdentity(port, executable.canonicalPath);
    assertSigning(signing, expectedVendor);
    return {
      ...executable,
      adapterId: input.adapterId,
      assurance: "full-vendor-identity",
      signing: [{ path: executable.canonicalPath, ...signing }],
    };
  }
  if (input.adapterId === "opencode-acp" && input.providerInstallRoot !== undefined) {
    const rootObservation = await port.inspectDirectory(input.providerInstallRoot);
    const root = rootObservation.canonicalPath;
    const contained = relative(root, executable.canonicalPath);
    if (contained.length === 0 || contained.startsWith("..") || isAbsolute(contained)) {
      throw new FabricError("ADAPTER_PATH_UNSAFE", "OpenCode launcher is outside its canonical install root");
    }
    assertSafeFile(executable, port.currentUid());
    for (let current = dirname(executable.canonicalPath); current.startsWith(root); current = dirname(current)) {
      const metadata = await port.inspectDirectory(current);
      if (metadata.ownerUid !== port.currentUid() || (metadata.mode & 0o022) !== 0 || !metadata.directory) {
        throw new FabricError("ADAPTER_PATH_UNSAFE", `OpenCode install component is unsafe: ${current}`);
      }
      if (current === root) break;
    }
    return {
      ...executable,
      adapterId: input.adapterId,
      assurance: "owner-controlled-install-root",
      signing: [],
    };
  }
  if (input.adapterId !== "cursor-agent" || input.cursorInstallRoot === undefined) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", `provider identity policy is missing: ${input.adapterId}`);
  }
  const rootObservation = await port.inspectDirectory(input.cursorInstallRoot);
  const root = rootObservation.canonicalPath;
  const contained = relative(root, executable.canonicalPath);
  if (contained.length === 0 || contained.startsWith("..") || isAbsolute(contained)) {
    throw new FabricError("ADAPTER_PATH_UNSAFE", "Cursor launcher is outside its canonical install root");
  }
  assertSafeFile(executable, port.currentUid());
  // Check every user-controlled directory below the declared root. The root
  // and launcher must remain owned by the current user and non-writable by
  // group/other; the unsigned shell/JS bundle is therefore labelled partial.
  for (let current = dirname(executable.canonicalPath); current.startsWith(root); current = dirname(current)) {
    const metadata = await port.inspectDirectory(current);
    if (metadata.ownerUid !== port.currentUid() || (metadata.mode & 0o022) !== 0 || !metadata.directory) {
      throw new FabricError("ADAPTER_PATH_UNSAFE", `Cursor install component is unsafe: ${current}`);
    }
    if (current === root) break;
  }
  const directory = dirname(executable.canonicalPath);
  const helperPath = join(directory, "spawn-helper");
  const nodePath = join(directory, "node");
  const helper = await verifiedSigningIdentity(port, helperPath);
  const node = await verifiedSigningIdentity(port, nodePath);
  assertSigning(helper, { teamId: "DCNK4UB866" });
  assertSigning(node, { teamId: "HX7739G8FX" });
  return {
    ...executable,
    adapterId: input.adapterId,
    assurance: "partial-signed-helpers",
    signing: [{ path: helperPath, ...helper }, { path: nodePath, ...node }],
  };
}
