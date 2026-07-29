import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import { resolveFabricPaths } from "../../src/cli/paths.ts";

const cleanup: string[] = [];

afterEach(async () => {
  vi.unstubAllEnvs();
  await Promise.all(cleanup.splice(0).map(async (path) => rm(path, { recursive: true, force: true })));
});

describe("fabric path resolution", () => {
  it("does not create absent directories by default", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-paths-"));
    cleanup.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    vi.stubEnv("AGENT_FABRIC_STATE_DIRECTORY", stateDirectory);
    vi.stubEnv("AGENT_FABRIC_RUNTIME_DIRECTORY", runtimeDirectory);

    expect(resolveFabricPaths()).toMatchObject({ stateDirectory, runtimeDirectory });

    await expect(stat(stateDirectory)).rejects.toMatchObject({ code: "ENOENT" });
    await expect(stat(runtimeDirectory)).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("creates private directories when requested", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-paths-"));
    cleanup.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    vi.stubEnv("AGENT_FABRIC_STATE_DIRECTORY", stateDirectory);
    vi.stubEnv("AGENT_FABRIC_RUNTIME_DIRECTORY", runtimeDirectory);

    expect(resolveFabricPaths({ createDirectories: true })).toMatchObject({
      stateDirectory,
      runtimeDirectory,
    });

    expect((await stat(stateDirectory)).mode & 0o777).toBe(0o700);
    expect((await stat(runtimeDirectory)).mode & 0o777).toBe(0o700);
  });
});
