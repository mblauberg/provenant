import { mkdir, mkdtemp, readFile, realpath, rename, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

const race = vi.hoisted(() => ({
  requestedPath: null as string | null,
  swap: null as (() => Promise<void>) | null,
}));

vi.mock("node:fs/promises", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:fs/promises")>();
  return {
    ...actual,
    realpath: async (path: Parameters<typeof actual.realpath>[0]) => {
      if (typeof path === "string" && path === race.requestedPath && race.swap !== null) {
        const swap = race.swap;
        race.swap = null;
        await swap();
      }
      return await actual.realpath(path);
    },
  };
});

import { ensureAutomaticBootstrapTrust } from "../../src/cli/workspace-trust.ts";

const temporaryDirectories: string[] = [];

afterEach(async () => {
  race.requestedPath = null;
  race.swap = null;
  await Promise.all(temporaryDirectories.splice(0).map(async (path) => await rm(path, { recursive: true, force: true })));
});

describe("requested project path replacement boundary", () => {
  it("refuses automatic enrolment when the requested directory becomes a symlink during realpath", async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), "fabric-boundary-race-")));
    temporaryDirectories.push(root);
    const project = join(root, "project");
    const original = join(root, "project-original");
    const replacement = join(root, "project-replacement");
    const stateDirectory = join(root, "state");
    await mkdir(project);
    await mkdir(replacement);
    await mkdir(stateDirectory, { mode: 0o700 });

    race.requestedPath = project;
    race.swap = async () => {
      await rename(project, original);
      await symlink(replacement, project);
    };

    await expect(ensureAutomaticBootstrapTrust({
      stateDirectory,
      bootstrapAttemptId: "attempt-requested-path-swap",
      cwd: project,
    })).rejects.toThrow(/changed while resolving|symbolic-link/iu);
    await expect(readFile(join(stateDirectory, "trusted-workspaces.json"))).rejects.toMatchObject({ code: "ENOENT" });
  });
});
