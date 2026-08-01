import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

import {
  isExecutableFile,
  resolveAdapterExecutable,
  resolveExecutableOnPath,
  validateEnabledAdapterExecutables,
} from "../../src/adapters/compatibility.ts";

async function writeRegistry(directory: string, adapters: Record<string, unknown>): Promise<string> {
  const path = join(directory, "adapter-compatibility.yaml");
  await writeFile(path, JSON.stringify({ adapters }));
  return path;
}

function adapter(enabled: boolean, executable: string): Record<string, unknown> {
  return { enabled, implementation: { executable } };
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

  it("native PATH resolution finds executable files and rejects non-executable files", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-executable-"));
    try {
      const executable = join(directory, "codex");
      await writeFile(executable, "#!/bin/sh\nexit 0\n");
      await chmod(executable, 0o755);
      await expect(resolveExecutableOnPath("codex", { path: directory })).resolves.toBe(executable);
      expect(await isExecutableFile(executable)).toBe(true);
      await chmod(executable, 0o644);
      expect(await isExecutableFile(executable)).toBe(false);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("resolves every optional CLI by name from an alternate PATH", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-alternate-path-"));
    try {
      for (const executable of ["agy", "cursor-agent", "kiro-cli", "opencode"]) {
        const executablePath = join(directory, executable);
        await writeFile(executablePath, "#!/bin/sh\nexit 0\n");
        await chmod(executablePath, 0o755);
        await expect(resolveAdapterExecutable({ executable, path: directory })).resolves.toBe(executablePath);
      }
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("mandatory-primary rejects a registry missing a primary adapter", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-mandatory-primary-"));
    try {
      const compatibilityPath = await writeRegistry(directory, {
        "claude-agent-sdk": adapter(true, process.execPath),
      });

      await expect(validateEnabledAdapterExecutables({
        compatibilityPath,
        mandatoryPrimary: true,
      })).rejects.toMatchObject({
        code: "ADAPTER_ARTIFACT_MISSING",
        message: expect.stringContaining("codex-app-server"),
      });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("mandatory-primary rejects a disabled primary adapter", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-disabled-primary-"));
    try {
      const compatibilityPath = await writeRegistry(directory, {
        "claude-agent-sdk": adapter(true, process.execPath),
        "codex-app-server": adapter(false, process.execPath),
      });

      await expect(validateEnabledAdapterExecutables({
        compatibilityPath,
        mandatoryPrimary: true,
      })).rejects.toMatchObject({
        code: "ADAPTER_ARTIFACT_MISSING",
        message: expect.stringContaining("codex-app-server"),
      });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("validates an explicitly selected disabled optional adapter at point of use", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-disabled-optional-"));
    try {
      const compatibilityPath = await writeRegistry(directory, {
        "pi-rpc": adapter(false, "missing-pi"),
      });

      const result = await validateEnabledAdapterExecutables({
        compatibilityPath,
        adapterIds: ["pi-rpc"],
      });

      expect(result.unavailableOptionalAdapters).toEqual([
        {
          adapterId: "pi-rpc",
          executable: "missing-pi",
          reasons: ["adapter pi-rpc is enabled but executable 'missing-pi' is not resolvable on PATH"],
        },
      ]);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("propagates malformed optional compatibility artifacts instead of degrading them", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-malformed-optional-"));
    try {
      const compatibilityPath = await writeRegistry(directory, {
        agy: adapter(false, "${USER_HOME}suffix"),
      });

      await expect(validateEnabledAdapterExecutables({
        compatibilityPath,
        adapterIds: ["agy"],
      })).rejects.toMatchObject({
        code: "ADAPTER_COMPATIBILITY_INVALID",
      });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });
});
