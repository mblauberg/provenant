import { join } from "node:path";
import { copyFile, mkdir, readFile, rm, stat, unlink, writeFile } from "node:fs/promises";

import { afterEach, describe, expect, it, vi } from "vitest";
import { parse } from "yaml";

import { runSourceCli } from "../support/cli-process.ts";
import { createResolvedStage4Compatibility } from "../support/stage4-pi-agy-testkit.ts";
import { resolveAdapterExecutableCli } from "../../src/cli/adapter-executable.ts";
import { FabricError } from "../../src/errors.ts";

type Fixture = Awaited<ReturnType<typeof createResolvedStage4Compatibility>>;

const fixtures: Fixture[] = [];

afterEach(async () => {
  vi.unstubAllEnvs();
  await Promise.all(fixtures.splice(0).map((fixture) => rm(fixture.directory, { recursive: true, force: true })));
});

async function fixtureExecutable(fixture: Fixture): Promise<string> {
  const document: unknown = parse(await readFile(fixture.compatibilityPath, "utf8"));
  if (
    typeof document !== "object" || document === null || !("adapters" in document) ||
    typeof document.adapters !== "object" || document.adapters === null || !("agy" in document.adapters) ||
    typeof document.adapters.agy !== "object" || document.adapters.agy === null || !("implementation" in document.adapters.agy) ||
    typeof document.adapters.agy.implementation !== "object" || document.adapters.agy.implementation === null ||
    !("executable" in document.adapters.agy.implementation) ||
    typeof document.adapters.agy.implementation.executable !== "string"
  ) {
    throw new TypeError("fixture has no Agy executable");
  }
  return document.adapters.agy.implementation.executable;
}

async function resolveFixtureExecutable(fixture: Fixture) {
  const configPath = join(fixture.directory, "agent-fabric.yaml");
  await writeFile(configPath, "schemaVersion: 1\nallowedAdapters: [agy]\nactiveAdapters: [agy]\n");
  return await resolveAdapterExecutableCli([
    "--adapter", "agy",
    "--config", configPath,
    "--compatibility", fixture.compatibilityPath,
    "--compatibility-schema", fixture.schemaPath,
  ], {
    verifyProvider: async (input) => {
      try { await stat(input.executable); } catch (error: unknown) {
        throw new FabricError("ADAPTER_ARTIFACT_MISSING", "provider executable is unavailable", { cause: error });
      }
      return {} as never;
    },
  });
}

describe("adapter executable resolver CLI", () => {
  it("prints the activated stable compatibility executable after validation", async () => {
    const fixture = await createResolvedStage4Compatibility("agy");
    fixtures.push(fixture);
    const executable = await fixtureExecutable(fixture);

    const result = await resolveFixtureExecutable(fixture);

    expect(result).toBe(executable);
  });

  it("loads configuration from the instance root and schemas from the product root", async () => {
    const fixture = await createResolvedStage4Compatibility("agy");
    fixtures.push(fixture);
    const instanceRoot = join(fixture.directory, "instance");
    const productRoot = join(fixture.directory, "product");
    await Promise.all([
      mkdir(join(instanceRoot, "config"), { recursive: true }),
      mkdir(join(productRoot, "runtime", "agent-fabric", "schemas"), { recursive: true }),
    ]);
    await Promise.all([
      writeFile(
        join(instanceRoot, "config", "agent-fabric.yaml"),
        "schemaVersion: 1\nallowedAdapters: [agy]\nactiveAdapters: [agy]\n",
      ),
      copyFile(fixture.compatibilityPath, join(instanceRoot, "config", "adapter-compatibility.yaml")),
      copyFile(
        fixture.schemaPath,
        join(productRoot, "runtime", "agent-fabric", "schemas", "adapter-compatibility.schema.json"),
      ),
    ]);

    const result = await resolveAdapterExecutableCli([
      "--adapter", "agy",
      "--product-root", productRoot,
      "--instance-root", instanceRoot,
    ], {
      verifyProvider: async () => ({} as never),
    });

    expect(result).toBe(await fixtureExecutable(fixture));
  });

  it("gives explicit --agents-home precedence over AGENT_FABRIC_PRODUCT_ROOT", async () => {
    const fixture = await createResolvedStage4Compatibility("agy");
    fixtures.push(fixture);
    const agentsHome = join(fixture.directory, "explicit-agents-home");
    await Promise.all([
      mkdir(join(agentsHome, "config"), { recursive: true }),
      mkdir(join(agentsHome, "runtime", "agent-fabric", "schemas"), { recursive: true }),
    ]);
    await Promise.all([
      writeFile(
        join(agentsHome, "config", "agent-fabric.yaml"),
        "schemaVersion: 1\nallowedAdapters: [agy]\nactiveAdapters: [agy]\n",
      ),
      copyFile(fixture.compatibilityPath, join(agentsHome, "config", "adapter-compatibility.yaml")),
      copyFile(
        fixture.schemaPath,
        join(agentsHome, "runtime", "agent-fabric", "schemas", "adapter-compatibility.schema.json"),
      ),
    ]);
    vi.stubEnv("AGENT_FABRIC_PRODUCT_ROOT", join(fixture.directory, "wrong-product"));

    const result = await resolveAdapterExecutableCli([
      "--adapter", "agy",
      "--agents-home", agentsHome,
    ], {
      verifyProvider: async () => ({} as never),
    });

    expect(result).toBe(await fixtureExecutable(fixture));
  });

  it("fails closed when the stable executable is missing", async () => {
    const fixture = await createResolvedStage4Compatibility("agy");
    fixtures.push(fixture);
    await unlink(await fixtureExecutable(fixture));

    await expect(resolveFixtureExecutable(fixture)).rejects.toMatchObject({ code: "ADAPTER_ARTIFACT_MISSING" });
  });

  it("fails closed when the adapter is not active", async () => {
    const fixture = await createResolvedStage4Compatibility("agy");
    fixtures.push(fixture);
    const configPath = join(fixture.directory, "agent-fabric.yaml");
    await writeFile(configPath, "schemaVersion: 1\nallowedAdapters: [agy]\nactiveAdapters: []\n");

    const result = await runSourceCli([
      "adapter", "executable", "--adapter", "agy",
      "--config", configPath,
      "--compatibility", fixture.compatibilityPath,
      "--compatibility-schema", fixture.schemaPath,
    ]);

    expect(result.exitCode).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).toContain("adapter is not active in trusted Fabric configuration");
  });

  it("rejects a value-taking option with no value", async () => {
    const result = await runSourceCli([
      "adapter", "executable", "--adapter", "agy", "--compatibility",
    ]);

    expect(result.exitCode).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).toContain("adapter executable requires a value for --compatibility");
  });

  it("rejects a flag-looking option value", async () => {
    const result = await runSourceCli([
      "adapter", "executable", "--adapter", "agy", "--compatibility", "--config", "fixture.yaml",
    ]);

    expect(result.exitCode).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).toContain("adapter executable requires a value for --compatibility");
  });

  it("rejects an unknown option", async () => {
    const result = await runSourceCli([
      "adapter", "executable", "--adapter", "agy", "--compatibilty", "fixture.yaml",
    ]);

    expect(result.exitCode).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).toContain("adapter executable received unknown option: --compatibilty");
  });

  it.each([
    ["--adapter", ["--adapter", "agy", "--adapter", "agy"]],
    ["--config", ["--adapter", "agy", "--config", "one.yaml", "--config", "two.yaml"]],
  ])("rejects duplicate %s options", async (option, arguments_) => {
    const result = await runSourceCli(["adapter", "executable", ...arguments_]);

    expect(result.exitCode).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).toContain(`adapter executable received duplicate option: ${option}`);
  });
});
