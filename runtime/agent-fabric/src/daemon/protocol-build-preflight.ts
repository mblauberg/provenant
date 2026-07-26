import { execFile } from "node:child_process";
import { access } from "node:fs/promises";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const execFileAsync = promisify(execFile);
const preflightPath = fileURLToPath(
  new URL("../../../../scripts/agent-fabric-protocol-preflight", import.meta.url),
);

export class ProtocolBuildPreflightError extends Error {
  readonly code = "AGENT_FABRIC_PROTOCOL_BUILD_STALE";

  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "ProtocolBuildPreflightError";
  }
}

export async function preflightProtocolBuild(): Promise<void> {
  try {
    await access(preflightPath);
  } catch (error: unknown) {
    if (
      error instanceof Error &&
      "code" in error &&
      error.code === "ENOENT"
    ) return;
    throw error;
  }
  try {
    await execFileAsync(preflightPath);
  } catch (error: unknown) {
    if (
      error instanceof Error &&
      "code" in error &&
      error.code === 78
    ) {
      const stderr = "stderr" in error && typeof error.stderr === "string"
        ? error.stderr.trim()
        : error.message;
      throw new ProtocolBuildPreflightError(stderr, { cause: error });
    }
    throw error;
  }
}
