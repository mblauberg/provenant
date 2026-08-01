import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";
import { stringify } from "yaml";

import {
  verifyAdapterCompatibility,
  verifySpawnWrapperProvenance,
} from "../../src/adapters/compatibility.ts";
import { commitFixtureRepository, writeWrapperPackageScaffold } from "../support/fixture-repository.ts";

function repositoryPath(relativePath: string): string {
  return fileURLToPath(new URL(`../../../../${relativePath}`, import.meta.url));
}

type ProvenanceFixture = {
  directory: string;
  compatibilityPath: string;
  schemaPath: string;
  wrapperPath: string;
  executablePath: string;
  repositoryCommit: string;
};

const temporaryDirectories: string[] = [];

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map((directory) => rm(directory, { recursive: true, force: true })));
});

async function writeCompatibility(
  fixture: Omit<ProvenanceFixture, "repositoryCommit">,
  wrapperEntrypoint: string,
): Promise<void> {
  await writeFile(
    fixture.compatibilityPath,
    stringify({
      schema_version: 1,
      activation_policy: { real_adapters_require_separate_gate: true, default_enabled: false, executable_resolution_version: 2 },
      adapters: {
        fixture: {
          enabled: true,
          delivery_stage: 4,
          implementation: {
            kind: "fixture",
            executable: fixture.executablePath,
            provider_identity: "apple-designated",
            wrapper_entrypoint: wrapperEntrypoint,
          },
          contract: {
            protocol: "fixture",
          },
          runtime_range: { platforms: [process.platform] },
          model_family_constraints: { allowed: ["fixture"], requires_explicit_model: true },
          official_source_url: "https://example.invalid",
        },
      },
    }),
  );
}

async function createProvenanceFixture(): Promise<ProvenanceFixture> {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-wrapper-provenance-"));
  temporaryDirectories.push(directory);
  const wrapperPath = join(directory, "wrapper.js");
  const executablePath = join(directory, "provider");
  await Promise.all([
    writeFile(wrapperPath, 'export const execute = () => "safe";\n'),
    writeFile(executablePath, "provider executable\n", { mode: 0o700 }),
  ]);
  await writeWrapperPackageScaffold(directory);
  const repositoryCommit = await commitFixtureRepository(directory);
  const fixture = {
    directory,
    compatibilityPath: join(directory, "adapter-compatibility.yaml"),
    schemaPath: repositoryPath("runtime/agent-fabric/schemas/adapter-compatibility.schema.json"),
    wrapperPath,
    executablePath,
  };
  await writeCompatibility(fixture, wrapperPath);
  return { ...fixture, repositoryCommit };
}

function verify(fixture: Pick<ProvenanceFixture, "compatibilityPath" | "schemaPath">) {
  return verifyAdapterCompatibility({
    compatibilityPath: fixture.compatibilityPath,
    schemaPath: fixture.schemaPath,
    adapterIds: ["fixture"],
    requireEnabled: true,
  });
}

describe("adapter wrapper Git provenance", () => {
  it("accepts a tracked clean wrapper without scanning sibling source spans", async () => {
    const fixture = await createProvenanceFixture();
    await writeFile(join(fixture.directory, "src", "untracked-sibling.js"), "export const shadow = true;\n");

    await expect(verify(fixture)).resolves.toMatchObject({
      valid: true,
      wrapperProvenance: [{
        adapterId: "fixture",
        repositoryCommit: fixture.repositoryCommit,
        wrapperPath: "wrapper.js",
      }],
    });
  });

  it("fails closed when the tracked wrapper is modified", async () => {
    const fixture = await createProvenanceFixture();
    await writeFile(fixture.wrapperPath, 'export const execute = () => "tampered";\n');

    await expect(verify(fixture)).rejects.toMatchObject({
      code: "ADAPTER_COMPATIBILITY_INVALID",
      message: expect.stringContaining("wrapper entrypoint differs from its committed content"),
    });
  });

  it("fails closed when the configured wrapper is not tracked", async () => {
    const fixture = await createProvenanceFixture();
    const untrackedWrapper = join(fixture.directory, "untracked-wrapper.js");
    await writeFile(untrackedWrapper, 'export const execute = () => "untracked";\n');
    await writeCompatibility(fixture, untrackedWrapper);

    await expect(verify(fixture)).rejects.toMatchObject({
      code: "ADAPTER_COMPATIBILITY_INVALID",
      message: expect.stringContaining("wrapper entrypoint is not tracked at the repository HEAD"),
    });
  });

  it("rechecks the simple wrapper pin before spawn", async () => {
    const fixture = await createProvenanceFixture();
    const expected = { repositoryCommit: fixture.repositoryCommit, wrapperPath: "wrapper.js" };

    await expect(verifySpawnWrapperProvenance({
      adapterId: "fixture",
      command: [process.execPath, fixture.wrapperPath],
      expected,
    })).resolves.toBeUndefined();

    await writeFile(fixture.wrapperPath, 'export const execute = () => "spawn tampered";\n');
    await expect(verifySpawnWrapperProvenance({
      adapterId: "fixture",
      command: [process.execPath, fixture.wrapperPath],
      expected,
    })).rejects.toMatchObject({
      code: "ADAPTER_COMPATIBILITY_INVALID",
      message: expect.stringContaining("wrapper entrypoint differs from its committed content"),
    });
  });
});
