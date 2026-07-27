import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import { afterEach, describe, expect, it } from "vitest";
import { parse, stringify } from "yaml";

function repositoryPath(relativePath: string): string {
  return fileURLToPath(new URL(`../../../../${relativePath}`, import.meta.url));
}

const execFileAsync = promisify(execFile);
const temporaryDirectories: string[] = [];

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map((directory) => rm(directory, { recursive: true, force: true })));
});

function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

describe("compatibility:pin", () => {
  it("rewrites the SDK pin and removes retired integrity/schema fields", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-compatibility-pin-"));
    temporaryDirectories.push(directory);
    const packageRoot = join(directory, "claude-agent-sdk");
    const compatibilityPath = join(directory, "adapter-compatibility.yaml");
    const lockfilePath = join(directory, "package-lock.json");
    const scriptPath = repositoryPath("runtime/agent-fabric/scripts/pin-primary-compatibility.mjs");
    const entrypointBytes = "export const pinnedFixture = true;\n";
    await mkdir(packageRoot, { recursive: true });
    await writeFile(join(packageRoot, "package.json"), JSON.stringify({
      name: "@anthropic-ai/claude-agent-sdk",
      version: "9.9.9",
    }));
    await writeFile(join(packageRoot, "sdk.mjs"), entrypointBytes);
    await writeFile(lockfilePath, JSON.stringify({
      packages: {
        "node_modules/@anthropic-ai/claude-agent-sdk": {
          version: "9.9.9",
          resolved: "https://example.invalid/old.tgz",
        },
      },
    }));
    await writeFile(compatibilityPath, stringify({
      schema_version: 1,
      verification_date: "2026-07-01",
      adapter_contract_version: 1,
      capability_fixture_version: 1,
      activation_policy: { real_adapters_require_separate_gate: true, default_enabled: false },
      adapters: {
        "claude-agent-sdk": {
          enabled: true,
          delivery_stage: 3,
          implementation: {
            kind: "npm-package",
            package: "@anthropic-ai/claude-agent-sdk",
            installed_version: "0.0.0",
            lock_integrity_sha512: "retired-lock-pin",
            resolved_artifact: "https://example.invalid/old.tgz",
            entrypoint: "node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs",
            entrypoint_sha256: "0".repeat(64),
            wrapper_entrypoint: "runtime/agent-fabric/src/adapters/providers/claude-agent-sdk.ts",
          },
          contract: {
            adapter_version: 1,
            protocol: "fixture",
            protocol_version: "fixture",
            schema_source: "runtime/agent-fabric/schemas/retired.d.ts",
            schema_sha256: "f".repeat(64),
            capability_fixture_version: 1,
          },
          runtime_range: { platforms: [process.platform] },
          model_family_constraints: { allowed: ["fixture"], requires_explicit_model: true },
          official_source_url: "https://example.invalid",
          unresolved_pins: [],
        },
      },
    }));

    const result = await execFileAsync(process.execPath, [
      scriptPath,
      "--compatibility",
      compatibilityPath,
      "--package-root",
      packageRoot,
      "--lockfile",
      lockfilePath,
    ]);
    expect(result.stdout).toContain("9.9.9");

    const document = parse(await readFile(compatibilityPath, "utf8")) as {
      verification_date: string;
      adapters: Record<string, {
        implementation?: Record<string, unknown>;
        contract?: Record<string, unknown>;
      }>;
    };
    const adapter = document.adapters["claude-agent-sdk"];
    const implementation = adapter?.implementation;
    expect(implementation).toBeDefined();
    if (implementation === undefined) throw new Error("pin script removed implementation metadata");
    expect(implementation).toMatchObject({
      installed_version: "9.9.9",
      entrypoint_sha256: sha256(entrypointBytes),
      resolved_artifact: "https://example.invalid/old.tgz",
    });
    expect(implementation.lock_integrity_sha512).toBeUndefined();
    expect(adapter?.contract).not.toHaveProperty("schema_source");
    expect(adapter?.contract).not.toHaveProperty("schema_sha256");
    expect(document.verification_date).toBe("2026-07-01");
  });

  it("moves every SDK version-bearing field together", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-compatibility-pin-"));
    temporaryDirectories.push(directory);
    const packageRoot = join(directory, "claude-agent-sdk");
    const compatibilityPath = join(directory, "adapter-compatibility.yaml");
    const lockfilePath = join(directory, "package-lock.json");
    const scriptPath = repositoryPath("runtime/agent-fabric/scripts/pin-primary-compatibility.mjs");
    const entrypointBytes = "export const bumpedFixture = true;\n";
    const resolvedArtifact = "https://registry.example.test/custom/claude-agent-sdk-9.9.10.tgz";
    await mkdir(packageRoot, { recursive: true });
    await writeFile(join(packageRoot, "package.json"), JSON.stringify({
      name: "@anthropic-ai/claude-agent-sdk",
      version: "9.9.10",
    }));
    await writeFile(join(packageRoot, "sdk.mjs"), entrypointBytes);
    await writeFile(lockfilePath, JSON.stringify({
      packages: {
        "node_modules/@anthropic-ai/claude-agent-sdk": {
          version: "9.9.10",
          resolved: resolvedArtifact,
        },
      },
    }));
    await writeFile(compatibilityPath, stringify({
      adapters: {
        "claude-agent-sdk": {
          implementation: {
            package: "@anthropic-ai/claude-agent-sdk",
            installed_version: "9.9.9",
            resolved_artifact: "https://example.invalid/stale.tgz",
            entrypoint: "node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs",
            entrypoint_sha256: "0".repeat(64),
          },
          contract: {
            protocol_version: "sdk-9.9.9",
          },
        },
      },
    }));

    await execFileAsync(process.execPath, [
      scriptPath,
      "--compatibility",
      compatibilityPath,
      "--package-root",
      packageRoot,
      "--lockfile",
      lockfilePath,
    ]);

    const document = parse(await readFile(compatibilityPath, "utf8")) as {
      adapters: Record<string, {
        implementation: Record<string, unknown>;
        contract: Record<string, unknown>;
      }>;
    };
    const adapter = document.adapters["claude-agent-sdk"];
    expect(adapter?.implementation).toMatchObject({
      installed_version: "9.9.10",
      entrypoint_sha256: sha256(entrypointBytes),
      resolved_artifact: resolvedArtifact,
    });
    expect(adapter?.contract.protocol_version).toBe("sdk-9.9.10");
  });

  it("rejects a lockfile version that disagrees with the installed package", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-compatibility-pin-"));
    temporaryDirectories.push(directory);
    const packageRoot = join(directory, "claude-agent-sdk");
    const compatibilityPath = join(directory, "adapter-compatibility.yaml");
    const lockfilePath = join(directory, "package-lock.json");
    const scriptPath = repositoryPath("runtime/agent-fabric/scripts/pin-primary-compatibility.mjs");
    await mkdir(packageRoot, { recursive: true });
    await writeFile(join(packageRoot, "package.json"), JSON.stringify({
      name: "@anthropic-ai/claude-agent-sdk",
      version: "9.9.10",
    }));
    await writeFile(join(packageRoot, "sdk.mjs"), "export {};\n");
    await writeFile(lockfilePath, JSON.stringify({
      packages: {
        "node_modules/@anthropic-ai/claude-agent-sdk": {
          version: "9.9.9",
          resolved: "https://example.invalid/claude-agent-sdk-9.9.9.tgz",
        },
      },
    }));
    await writeFile(compatibilityPath, stringify({
      adapters: {
        "claude-agent-sdk": {
          implementation: {
            package: "@anthropic-ai/claude-agent-sdk",
            entrypoint: "node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs",
          },
        },
      },
    }));

    await expect(execFileAsync(process.execPath, [
      scriptPath,
      "--compatibility",
      compatibilityPath,
      "--package-root",
      packageRoot,
      "--lockfile",
      lockfilePath,
    ])).rejects.toMatchObject({
      code: 1,
      stderr: expect.stringContaining("disagrees with package-lock.json version"),
    });
  });

  it("rejects a lockfile with no SDK package entry", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-compatibility-pin-"));
    temporaryDirectories.push(directory);
    const packageRoot = join(directory, "claude-agent-sdk");
    const compatibilityPath = join(directory, "adapter-compatibility.yaml");
    const lockfilePath = join(directory, "package-lock.json");
    const scriptPath = repositoryPath("runtime/agent-fabric/scripts/pin-primary-compatibility.mjs");
    await mkdir(packageRoot, { recursive: true });
    await writeFile(join(packageRoot, "package.json"), JSON.stringify({
      name: "@anthropic-ai/claude-agent-sdk",
      version: "9.9.10",
    }));
    await writeFile(join(packageRoot, "sdk.mjs"), "export {};\n");
    await writeFile(lockfilePath, JSON.stringify({ packages: {} }));
    await writeFile(compatibilityPath, stringify({
      adapters: {
        "claude-agent-sdk": {
          implementation: {
            package: "@anthropic-ai/claude-agent-sdk",
            entrypoint: "node_modules/@anthropic-ai/claude-agent-sdk/sdk.mjs",
          },
        },
      },
    }));

    await expect(execFileAsync(process.execPath, [
      scriptPath,
      "--compatibility",
      compatibilityPath,
      "--package-root",
      packageRoot,
      "--lockfile",
      lockfilePath,
    ])).rejects.toMatchObject({
      code: 1,
      stderr: expect.stringContaining("has no packages entry for node_modules/@anthropic-ai/claude-agent-sdk"),
    });
  });
});
