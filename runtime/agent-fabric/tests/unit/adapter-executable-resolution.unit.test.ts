import { mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import {
  validateEnabledAdapterExecutables,
  resolveAdapterExecutable,
  resolveExecutableOnPath,
} from "../../src/adapters/compatibility.ts";

const temporaryDirectories: string[] = [];

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map((directory) => rm(directory, { recursive: true, force: true })));
});

async function writeCompatibility(directory: string, executables: Record<string, string>): Promise<string> {
  const compatibilityPath = join(directory, "adapter-compatibility.yaml");
  const adapters = Object.entries(executables)
    .map(([adapterId, executable]) => [
      `  ${adapterId}:`,
      "    enabled: true",
      "    implementation:",
      "      kind: native-cli",
      `      executable: ${JSON.stringify(executable)}`,
    ].join("\n"))
    .join("\n");
  await writeFile(compatibilityPath, `schema_version: 1\nadapters:\n${adapters}\n`);
  return compatibilityPath;
}

describe("adapter executable resolution", () => {
  it("PATH resolution uses name when executable is not absolute", async () => {
    await expect(resolveExecutableOnPath("codex", {
      path: "/fake/bin",
      lookup: async (name) => name === "codex" ? "/fake/bin/codex" : undefined,
    })).resolves.toBe("/fake/bin/codex");

    await expect(resolveExecutableOnPath("/missing/codex", {
      path: "/fake/bin",
      lookup: async () => undefined,
    })).resolves.toBe("/missing/codex");
  });

  it("runtime override takes precedence over PATH resolution", async () => {
    await expect(resolveAdapterExecutable({
      executable: "codex",
      executableOverride: "/testing/codex",
      path: "/fake/bin",
      lookup: async () => "/fake/bin/codex",
    })).resolves.toBe("/testing/codex");
  });

  it("reports an unavailable optional adapter and continues when primary executables resolve", async () => {
    const directory = await mkdtemp(join(process.cwd(), ".adapter-resolution-"));
    temporaryDirectories.push(directory);
    const executable = join(directory, "provider");
    await writeFile(executable, "#!/bin/sh\nexit 0\n", { mode: 0o700 });
    const compatibilityPath = await writeCompatibility(directory, {
      "claude-agent-sdk": executable,
      "codex-app-server": executable,
      agy: join(directory, "missing-agy"),
    });

    await expect(validateEnabledAdapterExecutables({ compatibilityPath })).resolves.toMatchObject({
      unavailableOptionalAdapters: [{ adapterId: "agy" }],
    });
  });

  it("reports every unavailable adapter before failing for an unavailable primary", async () => {
    const directory = await mkdtemp(join(process.cwd(), ".adapter-resolution-"));
    temporaryDirectories.push(directory);
    const compatibilityPath = await writeCompatibility(directory, {
      "claude-agent-sdk": join(directory, "missing-claude"),
      "codex-app-server": join(directory, "missing-codex"),
      agy: join(directory, "missing-agy"),
      "cursor-agent": join(directory, "missing-cursor"),
    });

    await expect(validateEnabledAdapterExecutables({ compatibilityPath })).rejects.toThrow(
      /claude-agent-sdk.*codex-app-server.*agy.*cursor-agent/su,
    );
  });

  it("fails when an enabled primary has no implementation executable", async () => {
    const directory = await mkdtemp(join(process.cwd(), ".adapter-resolution-"));
    temporaryDirectories.push(directory);
    const compatibilityPath = join(directory, "adapter-compatibility.yaml");
    await writeFile(compatibilityPath, "schema_version: 1\nadapters:\n  claude-agent-sdk:\n    enabled: true\n");

    await expect(validateEnabledAdapterExecutables({ compatibilityPath })).rejects.toThrow(
      "enabled adapter has no provider executable: claude-agent-sdk",
    );
  });

  it("uses the named executable permission constant", async () => {
    const source = await readFile(new URL("../../src/adapters/compatibility.ts", import.meta.url), "utf8");
    expect(source).toContain("access(path, constants.X_OK)");
  });

  it("rejects executable directories but accepts symlinks to executable files", async () => {
    const directory = await mkdtemp(join(process.cwd(), ".adapter-resolution-"));
    temporaryDirectories.push(directory);
    const executable = join(directory, "provider");
    const link = join(directory, "provider-link");
    await writeFile(executable, "#!/bin/sh\nexit 0\n", { mode: 0o700 });
    await symlink(executable, link);

    await expect(validateEnabledAdapterExecutables({
      compatibilityPath: await writeCompatibility(directory, {
        "claude-agent-sdk": executable,
        "codex-app-server": executable,
        agy: directory,
      }),
    })).resolves.toMatchObject({
      unavailableOptionalAdapters: [{
        adapterId: "agy",
        reason: expect.stringMatching(/not resolvable/su),
      }],
    });

    await expect(validateEnabledAdapterExecutables({
      compatibilityPath: await writeCompatibility(directory, {
        "claude-agent-sdk": executable,
        "codex-app-server": link,
        agy: join(directory, "missing-agy"),
      }),
    })).resolves.toMatchObject({
      resolvedExecutables: {
        "claude-agent-sdk": executable,
        "codex-app-server": link,
      },
      unavailableOptionalAdapters: [{ adapterId: "agy" }],
    });
  });

});
