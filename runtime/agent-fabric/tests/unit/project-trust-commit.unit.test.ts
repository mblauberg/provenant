import { mkdir, mkdtemp, readFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

const fault = vi.hoisted(() => ({ failRegistryChmod: false }));

vi.mock("node:fs/promises", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:fs/promises")>();
  return {
    ...actual,
    chmod: async (
      path: Parameters<typeof actual.chmod>[0],
      mode: Parameters<typeof actual.chmod>[1],
    ) => {
      if (fault.failRegistryChmod && String(path).endsWith("trusted-workspaces.json")) {
        throw Object.assign(new Error("simulated post-rename chmod failure"), { code: "EACCES" });
      }
      return await actual.chmod(path, mode);
    },
  };
});

import { runProjectActivate } from "../../src/cli/project.ts";

const temporaryDirectories: string[] = [];

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), "fabric-project-trust-commit-"));
  temporaryDirectories.push(root);
  const stateDirectory = join(root, "state");
  const runtimeDirectory = join(stateDirectory, "runtime");
  const project = join(root, "project");
  await mkdir(runtimeDirectory, { recursive: true, mode: 0o700 });
  await mkdir(project, { mode: 0o700 });
  return {
    project,
    paths: {
      stateDirectory,
      runtimeDirectory,
      databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
      socketPath: join(runtimeDirectory, "fabric-v1.sock"),
    },
  };
}

afterEach(async () => {
  fault.failRegistryChmod = false;
  await Promise.all(temporaryDirectories.splice(0).map(async (path) => await rm(path, { recursive: true, force: true })));
});

describe("project activation trust commit point", () => {
  it("reports success and preserves the grant when post-rename chmod fails", async () => {
    const value = await fixture();
    fault.failRegistryChmod = true;

    await expect(runProjectActivate(value.project, value.paths)).resolves.toMatchObject({
      action: "trusted",
      trusted: true,
      trustedRoot: await realpath(value.project),
    });

    const registry = JSON.parse(await readFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), "utf8")) as {
      entries: { canonicalPath: string }[];
    };
    expect(registry.entries).toEqual([
      expect.objectContaining({ canonicalPath: await realpath(value.project) }),
    ]);
  });
});
