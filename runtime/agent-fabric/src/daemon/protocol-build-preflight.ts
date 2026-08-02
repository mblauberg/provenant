import { execFile } from "node:child_process";
import { access } from "node:fs/promises";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const execFileAsync = promisify(execFile);
const preflightPath = fileURLToPath(
  new URL("../../../../scripts/agent-fabric-protocol-preflight", import.meta.url),
);
// The dist this process will import belongs to the product tree beside this
// module. AGENTS_HOME names product code for the shipped adapter commands, but
// this preflight is bound to the code-adjacent product root so its freshness
// check cannot be redirected to a different product selection.
const installRoot = fileURLToPath(new URL("../../../../", import.meta.url)).replace(/\/$/u, "");

type ProtocolBuildPreflightErrorCode =
  | "AGENT_FABRIC_PROTOCOL_BUILD_STALE"
  | "AGENT_FABRIC_PREFLIGHT_INCOMPLETE";

export class ProtocolBuildPreflightError extends Error {
  readonly code: ProtocolBuildPreflightErrorCode;

  constructor(
    message: string,
    options?: ErrorOptions,
    code: ProtocolBuildPreflightErrorCode = "AGENT_FABRIC_PROTOCOL_BUILD_STALE",
  ) {
    super(message, options);
    this.name = "ProtocolBuildPreflightError";
    this.code = code;
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
    ) {
      throw new ProtocolBuildPreflightError(
        `missing preflight script at ${preflightPath}; repair: git -C "${installRoot}" restore --source=HEAD -- scripts/agent-fabric-protocol-preflight`,
        { cause: error },
        "AGENT_FABRIC_PREFLIGHT_INCOMPLETE",
      );
    }
    throw error;
  }
  try {
    await execFileAsync(preflightPath, { env: { ...process.env, AGENTS_HOME: installRoot } });
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
