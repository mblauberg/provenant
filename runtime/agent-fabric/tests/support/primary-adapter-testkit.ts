import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { parse, stringify } from "yaml";

import * as publicApi from "../../src/index.ts";
import { openFabric } from "../../src/index.ts";

import { ROOT_AUTHORITY } from "./stage1-fixture.ts";
import { commitFixtureRepository, writeWrapperPackageScaffold } from "./fixture-repository.ts";
import { createCurrentSessionRun } from "./current-session-testkit.ts";

export type PublicFunction = (...args: unknown[]) => unknown;

export function requirePublicFunction(name: string): PublicFunction {
  const value: unknown = Reflect.get(publicApi, name);
  if (typeof value !== "function") {
    throw new Error(`public agent-fabric API ${name} is not implemented`);
  }
  return (...args: unknown[]) => Reflect.apply(value, undefined, args);
}

export function primaryAdapterFixtureCommand(adapterId: string): string[] {
  return [
    process.execPath,
    "--import",
    "tsx",
    fileURLToPath(new URL("./primary-adapter-fixture.ts", import.meta.url)),
    adapterId,
  ];
}

export function repositoryPath(relativePath: string): string {
  return fileURLToPath(new URL(`../../../../${relativePath}`, import.meta.url));
}

function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

function compatibilityAdapter(options: {
  adapterId: string;
  implementationPath: string;
  implementationHash: string;
  entrypointPath?: string;
  entrypointHash?: string;
  schemaPath: string;
  schemaHash: string;
}): Record<string, unknown> {
  const implementation = {
    kind: "fixture-process",
    installed_version: "1.0.0-fixture",
    executable: options.implementationPath,
    executable_sha256: options.implementationHash,
    ...(options.entrypointPath === undefined ? {} : {
      entrypoint: options.entrypointPath,
      entrypoint_sha256: options.entrypointHash,
    }),
    provider_identity: "apple-designated",
  };
  return {
    enabled: false,
    delivery_stage: 3,
    implementation,
    contract: {
      adapter_version: 1,
      protocol: `${options.adapterId}-fixture`,
      protocol_version: "1",
      schema_source: options.schemaPath,
      schema_sha256: options.schemaHash,
      capability_fixture_version: 1,
    },
    runtime_range: { platforms: [process.platform] },
    model_family_constraints: { allowed: [], requires_explicit_model: true },
    official_source_url: "https://example.invalid/fixture",
    unresolved_pins: [],
  };
}

export async function createPrimaryCompatibilityFixture(): Promise<{
  directory: string;
  compatibilityPath: string;
  schemaPath: string;
  artifactPaths: string[];
}> {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-primary-compatibility-"));
  const executablePath = join(directory, "fixture-adapter");
  const protocolSchemaPath = join(directory, "fixture-protocol.json");
  const executableBytes = "fixture adapter executable\n";
  const schemaBytes = `${JSON.stringify({ schemaVersion: 1, protocolVersion: 1 })}\n`;
  await writeFile(executablePath, executableBytes, { mode: 0o700 });
  await writeFile(protocolSchemaPath, schemaBytes, { mode: 0o600 });

  const adapters = Object.fromEntries(
    ["claude-agent-sdk", "codex-app-server", "herdr"].map((adapterId) => [
      adapterId,
      compatibilityAdapter({
        adapterId,
        implementationPath: executablePath,
        implementationHash: sha256(executableBytes),
        ...(adapterId === "claude-agent-sdk" ? {
          entrypointPath: executablePath,
          entrypointHash: sha256(executableBytes),
        } : {}),
        schemaPath: protocolSchemaPath,
        schemaHash: sha256(schemaBytes),
      }),
    ]),
  );
  const compatibilityPath = join(directory, "adapter-compatibility.yaml");
  await writeFile(
    compatibilityPath,
    stringify({
      schema_version: 1,
      verification_date: "2026-07-10",
      adapter_contract_version: 1,
      capability_fixture_version: 1,
      activation_policy: {
        real_adapters_require_separate_gate: true,
        default_enabled: false,
      },
      adapters,
    }),
  );
  return {
    directory,
    compatibilityPath,
    schemaPath: repositoryPath("runtime/agent-fabric/schemas/adapter-compatibility.schema.json"),
    artifactPaths: [executablePath, protocolSchemaPath],
  };
}

export async function createPortableActivatedPrimaryFixture(): Promise<{
  directory: string;
  compatibilityPath: string;
  schemaPath: string;
  configPath: string;
  artifactPaths: string[];
}> {
  const fixture = await createPrimaryCompatibilityFixture();
  const wrapperPath = join(fixture.directory, "fixture-wrapper.js");
  const wrapperBytes = "export const portableFixtureWrapper = true;\n";
  await writeFile(wrapperPath, wrapperBytes, { mode: 0o600 });
  await writeWrapperPackageScaffold(fixture.directory);
  await commitFixtureRepository(fixture.directory);

  const value: unknown = parse(await readFile(fixture.compatibilityPath, "utf8"));
  if (!isRecord(value) || !isRecord(value.adapters)) {
    throw new TypeError("portable compatibility fixture is invalid");
  }
  for (const adapterId of ["claude-agent-sdk", "codex-app-server"]) {
    const adapter = value.adapters[adapterId];
    if (!isRecord(adapter) || !isRecord(adapter.implementation)) {
      throw new TypeError(`portable compatibility entry is invalid: ${adapterId}`);
    }
    adapter.enabled = true;
    adapter.implementation.wrapper_entrypoint = wrapperPath;
  }
  await writeFile(fixture.compatibilityPath, stringify(value));

  const configPath = join(fixture.directory, "agent-fabric.yaml");
  await writeFile(configPath, stringify({
    schemaVersion: 1,
    allowedAdapters: ["claude-agent-sdk", "codex-app-server"],
    activeAdapters: ["claude-agent-sdk", "codex-app-server"],
    allowedProfiles: ["headless", "paired-visible"],
    adapters: {
      "claude-agent-sdk": { command: [process.execPath, wrapperPath] },
      "codex-app-server": { command: [process.execPath, wrapperPath] },
    },
    workspaceRoots: [fixture.directory],
    limits: { maximumConcurrentProviderTurns: 2 },
  }));
  await mkdir(join(fixture.directory, "config", "review-profiles"), { recursive: true });
  await writeFile(
    join(
      fixture.directory,
      "config",
      "review-profiles",
      "certifying-review-four-slot-v1.json",
    ),
    `${JSON.stringify({
      schemaVersion: 1,
      profileId: "portable-activated-primary-fixture",
      chairProfiles: [],
    }, null, 2)}\n`,
  );
  return {
    ...fixture,
    configPath,
    artifactPaths: [...fixture.artifactPaths, wrapperPath],
  };
}

export async function readJsonObject(path: string): Promise<Record<string, unknown>> {
  const value: unknown = JSON.parse(await readFile(path, "utf8"));
  if (!isRecord(value)) {
    throw new TypeError(`${path} must contain an object`);
  }
  return value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export async function createInterventionFixture() {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-intervention-"));
  const databasePath = join(directory, "fabric.sqlite3");
  const fabric = await openFabric({ databasePath, workspaceRoots: [directory] });
  const run = await createCurrentSessionRun({
    databasePath,
    workspaceRoot: directory,
    runId: "run-stage3-intervention",
    projectRunDirectory: directory,
    chair: { agentId: "chair", authority: ROOT_AUTHORITY },
  });
  return {
    directory,
    fabric,
    chair: fabric.connect(run.chairCapability),
  };
}
