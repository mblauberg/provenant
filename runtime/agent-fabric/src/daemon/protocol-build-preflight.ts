import { execFile } from "node:child_process";
import { access } from "node:fs/promises";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const execFileAsync = promisify(execFile);
const preflightPath = fileURLToPath(
  new URL("../../../../scripts/agent-fabric-protocol-preflight", import.meta.url),
);
// The dist this process will import is the one beside this module, which is not
// necessarily the tree named by an ambient AGENTS_HOME: that variable selects a
// config/state home and callers legitimately point it at a synthetic one. The
// preflight defaults agents_home to AGENTS_HOME, so leaving it inherited judges
// a tree whose freshness says nothing about the code already loaded here.
const installRoot = fileURLToPath(new URL("../../../../", import.meta.url)).replace(/\/$/u, "");

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
