import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { stringify } from "yaml";

import { commitFixtureRepository, writeWrapperPackageScaffold } from "./fixture-repository.ts";

type Stage4AdapterId = "pi-rpc" | "agy";

const FAMILIES: Record<Stage4AdapterId, string[]> = {
  "pi-rpc": ["generic-open", "open-weight"],
  agy: ["google"],
};

export function stage4RepositoryPath(relativePath: string): string {
  return fileURLToPath(new URL(`../../../../${relativePath}`, import.meta.url));
}

export function stage4SchemaPath(): string {
  return stage4RepositoryPath("runtime/agent-fabric/schemas/adapter-compatibility.schema.json");
}

export function stage4FixtureCommand(adapterId: Stage4AdapterId): string[] {
  return [
    process.execPath,
    "--import",
    "tsx",
    fileURLToPath(new URL("./stage4-pi-agy-fixture.ts", import.meta.url)),
    adapterId,
    JSON.stringify(FAMILIES[adapterId]),
  ];
}

export async function createResolvedStage4Compatibility(adapterId: Stage4AdapterId): Promise<{
  directory: string;
  compatibilityPath: string;
  schemaPath: string;
}> {
  const directory = await mkdtemp(join(tmpdir(), `agent-fabric-${adapterId}-`));
  const executablePath = join(directory, `${adapterId}-fixture`);
  const wrapperPath = join(directory, `${adapterId}-wrapper.js`);
  const executableBytes = `${adapterId} deterministic fixture\n`;
  await writeFile(executablePath, executableBytes, { mode: 0o700 });
  await writeFile(wrapperPath, "export const fixtureWrapper = true;\n", { mode: 0o600 });
  await writeWrapperPackageScaffold(directory);
  await commitFixtureRepository(directory);
  const compatibilityPath = join(directory, "adapter-compatibility.yaml");
  await writeFile(
    compatibilityPath,
    stringify({
      schema_version: 1,
      activation_policy: { real_adapters_require_separate_gate: true, default_enabled: false, executable_resolution_version: 2 },
      adapters: {
        [adapterId]: {
          enabled: true,
          delivery_stage: 4,
          implementation: {
            kind: "fixture-process",
            executable: executablePath,
            provider_identity: "apple-designated",
            wrapper_entrypoint: wrapperPath,
          },
          contract: {
            protocol: `${adapterId}-fixture`,
          },
          runtime_range: { platforms: [process.platform] },
          model_family_constraints: {
            allowed: FAMILIES[adapterId],
            requires_explicit_model: true,
          },
          official_source_url: "https://example.invalid/deterministic-fixture",
        },
      },
    }),
  );
  return { directory, compatibilityPath, schemaPath: stage4SchemaPath() };
}
