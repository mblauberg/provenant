import { mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import {
  looksLikeRepositoryCollection,
  projectConfigPathAtExactRoot,
  resolveProjectBoundary,
} from "../../src/cli/project-boundary.ts";

const temporaryDirectories: string[] = [];

async function fixture(prefix: string): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), `fabric-${prefix}-`));
  temporaryDirectories.push(root);
  return await realpath(root);
}

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map(async (path) => await rm(path, { recursive: true, force: true })));
});

describe("shared project boundary resolver", () => {
  it("selects the nearest nested Git root and preserves the requested directory", async () => {
    const root = await fixture("nested-git-boundary");
    const outer = join(root, "outer");
    const inner = join(outer, "inner");
    const requested = join(inner, "src");
    await mkdir(join(outer, ".git"), { recursive: true });
    await mkdir(join(inner, ".git"), { recursive: true });
    await mkdir(requested, { recursive: true });

    await expect(resolveProjectBoundary(requested)).resolves.toMatchObject({
      requestedDirectory: await realpath(requested),
      selectedProjectRoot: await realpath(inner),
      evidence: { kind: "git", root: await realpath(inner), linkedWorktree: false },
    });
  });

  it("selects a valid exact project marker for a non-Git project", async () => {
    const root = await fixture("project-marker-boundary");
    const project = join(root, "project");
    const requested = project;
    const marker = join(project, ".provenant", "agent-fabric.yaml");
    await mkdir(requested, { recursive: true });
    await mkdir(join(project, ".provenant"), { recursive: true });
    await writeFile(marker, "schemaVersion: 1\n");

    await expect(resolveProjectBoundary(requested)).resolves.toMatchObject({
      requestedDirectory: await realpath(requested),
      selectedProjectRoot: await realpath(project),
      evidence: {
        kind: "project-marker",
        root: await realpath(project),
        markerPath: await realpath(marker),
      },
    });
    expect(projectConfigPathAtExactRoot(await realpath(project))).toBe(await realpath(marker));
  });

  it("refuses a malformed or symlinked project marker without falling through", async () => {
    const root = await fixture("unsafe-project-marker");
    const outer = join(root, "outer");
    const inner = join(outer, "inner");
    const requested = inner;
    await mkdir(requested, { recursive: true });
    await mkdir(join(outer, ".provenant"), { recursive: true });
    await writeFile(join(outer, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\n");
    await mkdir(join(inner, ".provenant"), { recursive: true });
    await symlink(join(outer, ".provenant", "agent-fabric.yaml"), join(inner, ".provenant", "agent-fabric.yaml"));

    await expect(resolveProjectBoundary(requested)).resolves.toMatchObject({
      selectedProjectRoot: await realpath(inner),
      evidence: { kind: "refused", reason: "unsafe-project-marker" },
    });
  });

  it("does not inherit a parent project marker for a nested non-Git request", async () => {
    const root = await fixture("exact-project-marker-boundary");
    const project = join(root, "project");
    const requested = join(project, "src");
    await mkdir(requested, { recursive: true });
    await mkdir(join(project, ".provenant"), { recursive: true });
    await writeFile(join(project, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\n");

    await expect(resolveProjectBoundary(requested)).resolves.toMatchObject({
      requestedDirectory: await realpath(requested),
      selectedProjectRoot: await realpath(requested),
      evidence: { kind: "ambiguous", reason: "unmarked-non-git" },
    });
  });

  it("refuses malformed YAML at the exact marker root", async () => {
    const root = await fixture("malformed-project-marker");
    const project = join(root, "project");
    await mkdir(join(project, ".provenant"), { recursive: true });
    await writeFile(join(project, ".provenant", "agent-fabric.yaml"), "schemaVersion: [\n");

    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      evidence: { kind: "refused", reason: "unsafe-project-marker" },
    });
  });

  it("reports an unmarked non-Git collection as ambiguous", async () => {
    const root = await fixture("ambiguous-project-boundary");
    const project = join(root, "projects");
    await mkdir(join(project, "one", ".git"), { recursive: true });
    await mkdir(join(project, "two", ".git"), { recursive: true });

    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      requestedDirectory: await realpath(project),
      selectedProjectRoot: await realpath(project),
      evidence: { kind: "ambiguous", reason: "repository-collection" },
    });
  });

  it("does not treat a cloned .provenant repository as a project marker", async () => {
    const root = await fixture("cloned-project-marker");
    const project = join(root, "projects");
    await mkdir(join(project, "one", ".git"), { recursive: true });
    await mkdir(join(project, "two", ".git"), { recursive: true });
    await mkdir(join(project, ".provenant", ".git"), { recursive: true });
    await writeFile(join(project, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\n");

    await expect(looksLikeRepositoryCollection(await realpath(project))).resolves.toBe(true);
    expect(() => projectConfigPathAtExactRoot(project)).toThrow(/it is itself a repository root/iu);
    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      evidence: { kind: "refused", reason: "unsafe-project-marker" },
    });
  });
});
