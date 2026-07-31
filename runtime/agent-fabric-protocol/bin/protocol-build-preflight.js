import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const candidates = [
  new URL("../../../scripts/agent-fabric-protocol-preflight", import.meta.url),
  new URL("../../../../scripts/agent-fabric-protocol-preflight", import.meta.url),
];
const preflightUrl = candidates.find((candidate) => existsSync(candidate));

/**
 * Run the shell-owned freshness predicate before importing a Node bin's dist.
 *
 * The bin process already paid Node startup, so this adds only the shared shell
 * preflight and deliberately does not opt into the doctor-only loadability
 * probe. stderr is inherited so the canonical typed error and exact repair line
 * remain owned by scripts/agent-fabric-protocol-preflight.
 */
export function protocolBuildPreflightPassed() {
  if (preflightUrl === undefined) {
    process.stderr.write(
      "AGENT_FABRIC_PREFLIGHT_INCOMPLETE: missing scripts/agent-fabric-protocol-preflight\n"
      + "repair: reinstall the harness scripts directory, including scripts/lib\n",
    );
    process.exitCode = 78;
    return false;
  }
  const preflightPath = fileURLToPath(preflightUrl);
  const installRoot = dirname(dirname(preflightPath));
  const result = spawnSync(preflightPath, {
    // An absent AGENTS_HOME must remain absent so preflight can derive and own
    // this install root. When one was genuinely inherited, retain the previous
    // loaded-tree override; its presence keeps autobuild disabled.
    env: process.env.AGENTS_HOME === undefined
      ? process.env
      : { ...process.env, AGENTS_HOME: installRoot },
    stdio: "inherit",
  });
  if (result.error !== undefined) throw result.error;
  if (result.status !== 0) {
    process.exitCode = result.status ?? 1;
    return false;
  }

  const attestationPath = join(
    installRoot,
    "runtime",
    "agent-fabric",
    "scripts",
    "verify-npm-ci-attestation.mjs",
  );
  if (!existsSync(attestationPath)) {
    process.stderr.write(
      "AGENT_FABRIC_PREFLIGHT_INCOMPLETE: missing runtime/agent-fabric/scripts/verify-npm-ci-attestation.mjs\n"
      + "repair: reinstall the harness scripts directory, including runtime/agent-fabric/scripts/lib\n",
    );
    process.exitCode = 78;
    return false;
  }
  const attestation = spawnSync(process.execPath, [attestationPath, installRoot], {
    stdio: "inherit",
  });
  if (attestation.error !== undefined) throw attestation.error;
  if (attestation.status !== 0) {
    process.exitCode = attestation.status ?? 1;
    return false;
  }
  return true;
}
