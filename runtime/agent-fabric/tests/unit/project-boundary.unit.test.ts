import { execFile } from "node:child_process";
import { mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";

import { afterEach, describe, expect, it } from "vitest";

import {
  looksLikeRepositoryCollection,
  projectConfigPathAtExactRoot,
  resolveProjectBoundary,
} from "../../src/cli/project-boundary.ts";

const temporaryDirectories: string[] = [];
const execFileAsync = promisify(execFile);

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

  it("accepts a stable ancestor alias but refuses an explicitly symlinked directory", async () => {
    const root = await fixture("ancestor-alias-boundary");
    const actualParent = join(root, "actual-parent");
    const project = join(actualParent, "project");
    const ancestorAlias = join(root, "ancestor-alias");
    const requestedAlias = join(root, "requested-alias");
    await mkdir(project, { recursive: true });
    await symlink(actualParent, ancestorAlias);
    await symlink(project, requestedAlias);

    await expect(resolveProjectBoundary(join(ancestorAlias, "project"))).resolves.toMatchObject({
      requestedDirectory: await realpath(project),
      selectedProjectRoot: await realpath(project),
      evidence: { kind: "ambiguous", reason: "unmarked-non-git" },
    });
    await expect(resolveProjectBoundary(requestedAlias)).resolves.toMatchObject({
      requestedDirectory: await realpath(project),
      selectedProjectRoot: await realpath(project),
      evidence: { kind: "refused", reason: "symbolic-link" },
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

  it("reports one direct ordinary Git repository as a repository collection", async () => {
    const root = await fixture("single-repository-collection");
    const project = join(root, "projects");
    const repository = join(project, "one");
    await mkdir(join(repository, ".git"), { recursive: true });

    await expect(looksLikeRepositoryCollection(await realpath(project))).resolves.toBe(true);
    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      requestedDirectory: await realpath(project),
      selectedProjectRoot: await realpath(project),
      gitProbe: "not-repository",
      evidence: {
        kind: "ambiguous",
        reason: "repository-collection",
        repositories: [await realpath(repository)],
      },
    });
  });

  it("reports multiple direct bare Git repositories as a repository collection", async () => {
    const root = await fixture("bare-repository-collection");
    const project = join(root, "projects");
    const one = join(project, "one");
    await execFileAsync("git", ["init", "--bare", "--quiet", one]);
    await execFileAsync("git", ["init", "--bare", "--quiet", join(project, "two")]);
    await rm(join(one, "description"));

    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      requestedDirectory: await realpath(project),
      selectedProjectRoot: await realpath(project),
      gitProbe: "not-repository",
      evidence: {
        kind: "ambiguous",
        reason: "repository-collection",
        repositories: [await realpath(one), await realpath(join(project, "two"))],
      },
    });
  });

  it("refuses a standalone bare Git root instead of treating it as plain non-Git", async () => {
    const root = await fixture("standalone-bare-repository");
    const project = join(root, "bare");
    await execFileAsync("git", ["init", "--bare", "--quiet", project]);
    await rm(join(project, "description"));

    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      requestedDirectory: await realpath(project),
      selectedProjectRoot: await realpath(project),
      gitProbe: "repository",
      evidence: { kind: "refused", reason: "bare-repository" },
    });
  });

  it("reports one direct bare Git repository without description as a repository collection", async () => {
    const root = await fixture("single-bare-repository-collection");
    const project = join(root, "projects");
    const bare = join(project, "bare");
    await execFileAsync("git", ["init", "--bare", "--quiet", bare]);
    await rm(join(bare, "description"));

    await expect(looksLikeRepositoryCollection(await realpath(project))).resolves.toBe(true);
    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      requestedDirectory: await realpath(project),
      selectedProjectRoot: await realpath(project),
      gitProbe: "not-repository",
      evidence: {
        kind: "ambiguous",
        reason: "repository-collection",
        repositories: [await realpath(bare)],
      },
    });
  });

  it("lets an exact project marker override a direct repository child", async () => {
    const root = await fixture("marked-single-repository-collection");
    const project = join(root, "projects");
    await mkdir(join(project, "one", ".git"), { recursive: true });
    await mkdir(join(project, ".provenant"), { recursive: true });
    await writeFile(join(project, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\n");

    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      selectedProjectRoot: await realpath(project),
      gitProbe: "not-repository",
      evidence: { kind: "project-marker", root: await realpath(project) },
    });
  });

  it("keeps a normal HEAD/objects/refs folder out of bare-repository collection evidence", async () => {
    const root = await fixture("bare-shape-false-positive");
    const project = join(root, "projects");
    const ordinary = join(project, "ordinary");
    const bare = join(project, "bare");
    await mkdir(join(ordinary, "objects"), { recursive: true });
    await mkdir(join(ordinary, "refs"), { recursive: true });
    await writeFile(join(ordinary, "HEAD"), "not a Git ref\n");
    await execFileAsync("git", ["init", "--bare", "--quiet", bare]);

    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      gitProbe: "not-repository",
      evidence: {
        kind: "ambiguous",
        reason: "repository-collection",
        repositories: [await realpath(bare)],
      },
    });
  });

  it("treats mixed worktree and bare direct children as one repository collection", async () => {
    const root = await fixture("mixed-repository-collection");
    const project = join(root, "projects");
    const worktree = join(project, "worktree");
    const bare = join(project, "bare");
    await mkdir(join(worktree, ".git"), { recursive: true });
    await execFileAsync("git", ["init", "--bare", "--quiet", bare]);

    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      evidence: {
        kind: "ambiguous",
        reason: "repository-collection",
        repositories: expect.arrayContaining([await realpath(worktree), await realpath(bare)]),
      },
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

  it("rejects a Gitfile whose target is an arbitrary existing directory", async () => {
    const root = await fixture("arbitrary-gitfile-target");
    const project = join(root, "project");
    const target = join(root, "ordinary-directory");
    await mkdir(project, { recursive: true });
    await mkdir(target, { recursive: true });
    await writeFile(join(project, ".git"), `gitdir: ${target}\n`);

    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      selectedProjectRoot: await realpath(project),
      evidence: { kind: "refused", reason: "malformed-git-marker" },
    });
  });

  it("rejects a Gitfile targeting a foreign Git repository", async () => {
    const root = await fixture("foreign-gitfile-target");
    const project = join(root, "project");
    const foreign = join(root, "foreign");
    await mkdir(project, { recursive: true });
    await mkdir(foreign, { recursive: true });
    await execFileAsync("git", ["-C", foreign, "init", "--quiet"]);
    await writeFile(join(project, ".git"), `gitdir: ${join(foreign, ".git")}\n`);

    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      selectedProjectRoot: await realpath(project),
      evidence: { kind: "refused", reason: "malformed-git-marker" },
    });
  });

  it("rejects a Gitfile whose valid target resolves outside the selected root", async () => {
    const root = await fixture("out-of-root-gitfile-target");
    const project = join(root, "project");
    const foreign = join(root, "outside");
    await mkdir(project, { recursive: true });
    await mkdir(foreign, { recursive: true });
    await execFileAsync("git", ["-C", foreign, "init", "--quiet"]);
    await writeFile(join(project, ".git"), "gitdir: ../outside/.git\n");

    await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
      selectedProjectRoot: await realpath(project),
      evidence: { kind: "refused", reason: "malformed-git-marker" },
    });
  });

  it("refuses a Gitfile whose marker is swapped during the Git probe", async () => {
    const root = await fixture("gitfile-marker-swap");
    const project = join(root, "project");
    const target = join(project, "git-target");
    const marker = join(project, ".git");
    const replacement = join(root, "replacement-marker");
    const shimDirectory = join(root, "bin");
    await mkdir(project, { recursive: true });
    await mkdir(join(target, ".git"), { recursive: true });
    await mkdir(shimDirectory, { recursive: true });
    await writeFile(marker, "gitdir: git-target/.git\n");
    await writeFile(replacement, "not a gitdir marker\n");
    const gitShim = join(shimDirectory, "git");
    await writeFile(gitShim, "#!/bin/sh\nmv " + replacement + " " + marker + "\nprintf '%s\\n' " + project + "\n");
    await import("node:fs/promises").then(async ({ chmod }) => await chmod(gitShim, 0o700));

    const previousPath = process.env.PATH;
    process.env.PATH = shimDirectory + ":" + (previousPath ?? "");
    try {
      await expect(resolveProjectBoundary(project)).resolves.toMatchObject({
        evidence: { kind: "refused", reason: "malformed-git-marker" },
      });
    } finally {
      if (previousPath === undefined) delete process.env.PATH;
      else process.env.PATH = previousPath;
    }
  });
});
