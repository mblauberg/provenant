import { join, resolve } from "node:path";

import { verifyAdapterCompatibility } from "../src/adapters/compatibility.js";
import { verifyProviderExecutableIdentity } from "../src/adapters/provider-identity.js";
import { probeProviderInterface } from "../src/adapters/provider-interface.js";
import { FabricError } from "../src/errors.js";

const root = resolve(import.meta.dirname, "../../..");
const adapterIds = ["claude-agent-sdk", "codex-app-server"];

function argumentValue(arguments_: string[], name: string): string | undefined {
  const index = arguments_.indexOf(name);
  if (index === -1) return undefined;
  const value = arguments_[index + 1];
  if (value === undefined || value.startsWith("--")) {
    throw new Error(`${name} requires a value`);
  }
  return value;
}

const arguments_ = process.argv.slice(2);
const compatibilityOnly = arguments_.includes("--compatibility-only");
const compatibilityPath = resolve(
  argumentValue(arguments_, "--compatibility")
    ?? join(root, "config/adapter-compatibility.yaml"),
);

try {
  const result = await verifyAdapterCompatibility({
    compatibilityPath,
    schemaPath: join(root, "runtime/agent-fabric/schemas/adapter-compatibility.schema.json"),
    adapterIds,
    requireEnabled: true,
  });
  const observations = [];
  if (!compatibilityOnly) {
    for (const adapterId of adapterIds) {
      const executable = result.resolvedExecutables[adapterId];
      if (executable === undefined) throw new Error(`provider executable is missing: ${adapterId}`);
      const identity = await verifyProviderExecutableIdentity({ adapterId, executable });
      const contract = await probeProviderInterface({ adapterId, executable });
      observations.push({ adapterId, canonicalPath: identity.canonicalPath, version: contract.version, sha256: identity.sha256, assurance: identity.assurance, signing: identity.signing });
    }
  }

  process.stdout.write(`${JSON.stringify({
    status: "pass",
    adapterIds: result.adapterIds,
    verifiedArtifactCount: result.verifiedArtifactCount,
    observations,
  })}\n`);
} catch (error: unknown) {
  if (!(error instanceof FabricError)) throw error;
  process.stderr.write(`${JSON.stringify({
    status: "fail",
    code: error.code,
    message: error.message,
  })}\n`);
  process.exitCode = 1;
}
