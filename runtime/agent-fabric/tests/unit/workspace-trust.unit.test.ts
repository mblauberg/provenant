import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, lstat, mkdir, mkdtemp, readFile, realpath, rename, rm, symlink, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";

import { afterEach, describe, expect, it } from "vitest";

import {
  looksLikeRepositoryCollection,
  nearestGitWorkspace,
  runWorkspaceTrust,
  trustedWorkspaceIdentity,
  trustedWorkspaceRoots,
  WorkspaceTrustError,
} from "../../src/cli/workspace-trust.ts";

const temporaryDirectories: string[] = [];
const execFileAsync = promisify(execFile);

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), "fabric-workspace-trust-"));
  temporaryDirectories.push(root);
  const stateDirectory = join(root, "state");
  const runtimeDirectory = join(stateDirectory, "runtime");
  const workspace = join(root, "workspace");
  await mkdir(runtimeDirectory, { recursive: true, mode: 0o700 });
  await mkdir(workspace, { mode: 0o700 });
  await chmod(stateDirectory, 0o700);
  return {
    root,
    workspace,
    paths: {
      stateDirectory,
      runtimeDirectory,
      databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
      socketPath: join(runtimeDirectory, "fabric-v1.sock"),
    },
  };
}

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map(async (path) => await rm(path, { recursive: true, force: true })));
});

describe("machine-local workspace trust", () => {
  async function gitRepository(parent: string, name: string): Promise<string> {
    const repository = join(parent, name);
    await mkdir(join(repository, ".git"), { recursive: true, mode: 0o700 });
    return repository;
  }

  it("exports the exact live normalized entry with a deterministic sha256 binding", async () => {
    const value = await fixture();
    const now = new Date("2026-07-11T04:00:00.000Z");
    await runWorkspaceTrust([
      "trust", value.workspace, "--profiles", "observed,headless", "--expires-at", "2026-07-12T04:00:00.000Z",
    ], value.paths, now);

    const identity = await trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.workspace,
      now,
    });
    const canonicalEntry = JSON.stringify({
      allowedProfiles: ["headless", "observed"],
      approvedAt: now.toISOString(),
      canonicalPath: await realpath(value.workspace),
      device: identity.entry.device,
      expiresAt: "2026-07-12T04:00:00.000Z",
      inode: identity.entry.inode,
      establishmentKind: "local-operator",
    });
    expect(identity).toEqual({
      canonicalRoot: await realpath(value.workspace),
      trustRecordDigest: `sha256:${createHash("sha256").update(canonicalEntry).digest("hex")}`,
      entry: {
        canonicalPath: await realpath(value.workspace),
        approvedAt: now.toISOString(),
        device: identity.entry.device,
        establishmentKind: "local-operator",
        inode: identity.entry.inode,
        expiresAt: "2026-07-12T04:00:00.000Z",
        allowedProfiles: ["headless", "observed"],
      },
    });
    await expect(trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.workspace,
      executionProfile: "paired-visible",
      now,
    })).rejects.toThrow(/profile/u);
  });

  it("keeps a legacy digest on read-only and status paths until the registry is migrated", async () => {
    const value = await fixture();
    await runWorkspaceTrust(["trust", value.workspace], value.paths);
    const registryPath = join(value.paths.stateDirectory, "trusted-workspaces.json");
    const canonical = JSON.parse(await readFile(registryPath, "utf8")) as {
      schemaVersion: number;
      entries: Record<string, unknown>[];
    };
    const entry = canonical.entries[0];
    if (entry === undefined) throw new Error("canonical trust entry is missing");
    canonical.schemaVersion = 1;
    entry.approvedBy = "local-operator";
    delete entry.establishmentKind;
    await writeFile(registryPath, `${JSON.stringify(canonical, null, 2)}\n`, { mode: 0o600 });
    const legacyBytes = await readFile(registryPath, "utf8");
    const legacyDigest = `sha256:${createHash("sha256").update(JSON.stringify({
      allowedProfiles: entry.allowedProfiles,
      approvedAt: entry.approvedAt,
      approvedBy: "local-operator",
      canonicalPath: entry.canonicalPath,
      device: entry.device,
      inode: entry.inode,
    })).digest("hex")}`;

    await expect(trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.workspace,
    })).resolves.toMatchObject({ trustRecordDigest: legacyDigest });
    await expect(runWorkspaceTrust(["status", value.workspace], value.paths)).resolves.toMatchObject({
      schemaVersion: 2,
      trusted: true,
      entry: { establishmentKind: "local-operator" },
    });
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({
      entries: [{ canonicalPath: await realpath(value.workspace), trusted: true }],
    });
    await expect(readFile(registryPath, "utf8")).resolves.toBe(legacyBytes);
  });

  it("fails closed on an unknown trust establishment kind", async () => {
    const value = await fixture();
    const canonicalPath = await realpath(value.workspace);
    const identity = await lstat(canonicalPath);
    await writeFile(join(value.paths.stateDirectory, "trusted-workspaces.json"), `${JSON.stringify({
      schemaVersion: 1,
      entries: [{
        canonicalPath,
        approvedAt: "2026-07-11T04:00:00.000Z",
        approvedBy: "local-operator",
        device: identity.dev,
        inode: identity.ino,
        allowedProfiles: ["headless"],
        establishmentKind: "unknown-source",
      }],
    })}\n`, { mode: 0o600 });

    await expect(trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.workspace,
    })).rejects.toThrow(/establishment kind is invalid/u);
  });

  it("rejects a trusted root after it becomes an unmarked collection and reuses its digest after marking it", async () => {
    const value = await fixture();
    await runWorkspaceTrust(["trust", value.workspace], value.paths);
    const canonicalPath = await realpath(value.workspace);
    const original = await trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.workspace,
    });
    const registryPath = join(value.paths.stateDirectory, "trusted-workspaces.json");
    const before = await readFile(registryPath, "utf8");

    await gitRepository(value.workspace, "first-repo");
    await gitRepository(value.workspace, "second-repo");

    await expect(runWorkspaceTrust(["trust", value.workspace], value.paths))
      .rejects.toThrow(/repository collection/u);

    await expect(trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.workspace,
    })).rejects.toSatisfy((error: unknown) =>
      error instanceof WorkspaceTrustError &&
      error.code === "WORKSPACE_NOT_TRUSTED" &&
      error.message.includes("repository collection"));
    await expect(trustedWorkspaceRoots({ stateDirectory: value.paths.stateDirectory }))
      .resolves.toEqual([]);
    await expect(runWorkspaceTrust(["inspect", value.workspace], value.paths))
      .resolves.toMatchObject({ canonicalPath, trusted: false });

    await writeFile(join(value.workspace, "AGENTS.md"), "# composed project\n");

    await expect(trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.workspace,
    })).resolves.toMatchObject({
      canonicalRoot: canonicalPath,
      trustRecordDigest: original.trustRecordDigest,
    });
    await expect(trustedWorkspaceRoots({ stateDirectory: value.paths.stateDirectory }))
      .resolves.toEqual([canonicalPath]);
    await expect(readFile(registryPath, "utf8")).resolves.toBe(before);
  });

  it("atomically records exact roots and filters them by profile and expiry", async () => {
    const value = await fixture();
    const now = new Date("2026-07-11T04:00:00.000Z");
    await expect(runWorkspaceTrust([
      "trust", value.workspace, "--profiles", "headless,observed", "--expires-at", "2026-07-12T04:00:00.000Z",
    ], value.paths, now)).resolves.toMatchObject({ trusted: true });

    await expect(trustedWorkspaceRoots({ stateDirectory: value.paths.stateDirectory, executionProfile: "headless", now }))
      .resolves.toEqual([await realpath(value.workspace)]);
    await expect(trustedWorkspaceRoots({ stateDirectory: value.paths.stateDirectory, executionProfile: "paired-visible", now }))
      .resolves.toEqual([]);
    await expect(trustedWorkspaceRoots({ stateDirectory: value.paths.stateDirectory, executionProfile: "headless", now: new Date("2026-07-13T00:00:00.000Z") }))
      .resolves.toEqual([]);
    await expect(runWorkspaceTrust(["inspect", value.workspace], value.paths, new Date("2026-07-13T00:00:00.000Z")))
      .resolves.toMatchObject({ trusted: false, expired: true, entry: expect.objectContaining({ canonicalPath: await realpath(value.workspace) }) });

    const registryPath = join(value.paths.stateDirectory, "trusted-workspaces.json");
    expect((await lstat(registryPath)).mode & 0o077).toBe(0);
    expect(JSON.parse(await readFile(registryPath, "utf8"))).toMatchObject({ schemaVersion: 2 });
  });

  it("trusts a first-use project exactly without trusting its parent", async () => {
    const value = await fixture();

    await expect(runWorkspaceTrust(["inspect", value.workspace], value.paths))
      .resolves.toMatchObject({ canonicalPath: await realpath(value.workspace), trusted: false });
    await expect(runWorkspaceTrust(["trust", value.workspace], value.paths))
      .resolves.toMatchObject({ trusted: true });

    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({
      entries: [expect.objectContaining({ canonicalPath: await realpath(value.workspace) })],
    });
    await expect(runWorkspaceTrust(["inspect", value.root], value.paths))
      .resolves.toMatchObject({ canonicalPath: await realpath(value.root), trusted: false });
  });

  it("trusts a Git repository root that contains submodule-like children", async () => {
    const value = await fixture();
    await mkdir(join(value.workspace, ".git"), { mode: 0o700 });
    for (const name of ["first-submodule", "second-submodule"]) {
      const child = join(value.workspace, name);
      await mkdir(child, { mode: 0o700 });
      await writeFile(join(child, ".git"), "gitdir: ../.git/modules/child\n");
    }

    await expect(runWorkspaceTrust(["trust", value.workspace], value.paths))
      .resolves.toMatchObject({ trusted: true });
  });

  it("rejects a direct-child repository collection and names exact commands for its children", async () => {
    const value = await fixture();
    const collection = join(value.root, "projects");
    await mkdir(collection, { mode: 0o700 });
    const first = await gitRepository(collection, "first-repo");
    const second = await gitRepository(collection, "second-repo");

    await expect(runWorkspaceTrust(["trust", collection], value.paths)).rejects.toThrow(
      new RegExp(`repository collection.*${first}.*${second}.*fabric workspace trust`, "u"),
    );
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
  });

  it("rejects a parent with exactly one direct repository child", async () => {
    const value = await fixture();
    const collection = join(value.root, "single-repository-collection");
    await mkdir(collection, { mode: 0o700 });
    const repository = await gitRepository(collection, "only-repo");

    await expect(looksLikeRepositoryCollection(await realpath(collection))).resolves.toBe(true);
    await expect(runWorkspaceTrust(["trust", collection], value.paths)).rejects.toThrow(
      new RegExp(`repository collection.*${repository}.*fabric workspace trust`, "u"),
    );
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
  });

  it("treats two symlink aliases to one repository as one collection child and refuses the parent", async () => {
    const value = await fixture();
    const collection = join(value.root, "alias-only-collection");
    const repositories = join(value.root, "repositories");
    await mkdir(collection, { mode: 0o700 });
    await mkdir(repositories, { mode: 0o700 });
    const repository = await gitRepository(repositories, "shared-repo");
    await symlink(repository, join(collection, "first-alias"));
    await symlink(repository, join(collection, "second-alias"));

    const canonicalCollection = await realpath(collection);
    await expect(looksLikeRepositoryCollection(canonicalCollection)).resolves.toBe(true);
    await expect(runWorkspaceTrust(["trust", collection], value.paths)).rejects.toThrow(/repository collection/u);
  });

  it("treats a real repository child and its symlink alias as one collection child and refuses the parent", async () => {
    const value = await fixture();
    const collection = join(value.root, "real-and-alias-collection");
    await mkdir(collection, { mode: 0o700 });
    const repository = await gitRepository(collection, "shared-repo");
    await symlink(repository, join(collection, "shared-alias"));

    const canonicalCollection = await realpath(collection);
    await expect(looksLikeRepositoryCollection(canonicalCollection)).resolves.toBe(true);
    await expect(runWorkspaceTrust(["trust", collection], value.paths)).rejects.toThrow(/repository collection/u);
  });

  it("shell-quotes every canonical repository path in collection trust commands", async () => {
    const value = await fixture();
    const collection = join(value.root, "projects with spaces");
    await mkdir(collection, { mode: 0o700 });
    const names = [
      "repo with spaces",
      "repo'with'quotes",
      "repo;echo shell-metacharacter",
      "repo$(echo injected) `backtick`",
    ];
    const repositories: string[] = [];
    for (const name of names) repositories.push(await gitRepository(collection, name));

    const result = await runWorkspaceTrust(["trust", collection], value.paths).catch((error: unknown) => error);
    expect(result).toBeInstanceOf(Error);
    const message = (result as Error).message;
    const shellQuote = (path: string) => `'${path.replaceAll("'", `\'"\'"\'`)}'`;
    const canonicalRepositories = await Promise.all(repositories.map(async (repository) => await realpath(repository)));
    const commandsStart = message.indexOf("Run ");
    expect(commandsStart).toBeGreaterThanOrEqual(0);
    const commands = message.slice(commandsStart + "Run ".length);

    for (const repository of canonicalRepositories) {
      expect(commands).toContain(`provenant fabric workspace trust ${shellQuote(repository)}`);
    }
    const shell = await execFileAsync("sh", [
      "-c",
      `provenant() { test "$1" = fabric && test "$2" = workspace && test "$3" = trust || exit 91; printf '%s\\n' "$4"; }\n${commands}`,
    ]);
    expect(shell.stdout.trim().split("\n").sort()).toEqual([...canonicalRepositories].sort());
  });

  it("trusts a marker-bearing multi-repository project and keeps it outside Git discovery", async () => {
    const value = await fixture();
    const project = join(value.root, "composed-project");
    await mkdir(project, { mode: 0o700 });
    await gitRepository(project, "first-repo");
    await gitRepository(project, "second-repo");
    await mkdir(join(project, ".provenant"), { mode: 0o700 });
    await writeFile(join(project, ".provenant", "agent-fabric.yaml"), "schemaVersion: 1\n");
    const canonicalProject = await realpath(project);

    await expect(nearestGitWorkspace(canonicalProject)).resolves.toBeNull();
    await expect(looksLikeRepositoryCollection(canonicalProject)).resolves.toBe(false);
    await expect(runWorkspaceTrust(["trust", project], value.paths)).resolves.toMatchObject({ trusted: true });
  });

  it("refuses a collection whose .claude entry is itself a cloned repository", async () => {
    const value = await fixture();
    const collection = join(value.root, "cloned-marker-collection");
    await mkdir(collection, { mode: 0o700 });
    await gitRepository(collection, "first-repo");
    await gitRepository(collection, "second-repo");
    // Anyone who can drop a repository beside the others could otherwise name
    // it .claude and switch the collection guard off for every sibling.
    await gitRepository(collection, ".claude");
    const canonicalCollection = await realpath(collection);

    await expect(looksLikeRepositoryCollection(canonicalCollection)).resolves.toBe(true);
    await expect(runWorkspaceTrust(["trust", collection], value.paths)).rejects.toThrow(/collection/u);
  });

  it("refuses a collection whose .claude marker resolves to the candidate root", async () => {
    const value = await fixture();
    const collection = join(value.root, "root-marker-collection");
    await mkdir(collection, { mode: 0o700 });
    await gitRepository(collection, "first-repo");
    await gitRepository(collection, "second-repo");
    await symlink(".", join(collection, ".claude"));
    const canonicalCollection = await realpath(collection);

    await expect(looksLikeRepositoryCollection(canonicalCollection)).resolves.toBe(true);
    await expect(runWorkspaceTrust(["trust", collection], value.paths)).rejects.toThrow(/collection/u);
  });

  it("refuses a collection whose .claude marker resolves outside the candidate root", async () => {
    const value = await fixture();
    const collection = join(value.root, "outside-marker-collection");
    const outsideMarker = join(value.root, "outside-marker");
    await mkdir(collection, { mode: 0o700 });
    await mkdir(outsideMarker, { mode: 0o700 });
    await gitRepository(collection, "first-repo");
    await gitRepository(collection, "second-repo");
    await symlink(outsideMarker, join(collection, ".claude"));
    const canonicalCollection = await realpath(collection);

    await expect(looksLikeRepositoryCollection(canonicalCollection)).resolves.toBe(true);
    await expect(runWorkspaceTrust(["trust", collection], value.paths)).rejects.toThrow(/collection/u);
  });

  it("refuses a collection whose repository children are reached through directory symlinks", async () => {
    const value = await fixture();
    const collection = join(value.root, "symlinked-repository-collection");
    const repositories = join(value.root, "repositories");
    await mkdir(collection, { mode: 0o700 });
    await mkdir(repositories, { mode: 0o700 });
    const first = await gitRepository(repositories, "first-repo");
    const second = await gitRepository(repositories, "second-repo");
    await symlink(first, join(collection, "first-repo"));
    await symlink(second, join(collection, "second-repo"));
    const canonicalCollection = await realpath(collection);

    await expect(looksLikeRepositoryCollection(canonicalCollection)).resolves.toBe(true);
    await expect(runWorkspaceTrust(["trust", collection], value.paths)).rejects.toThrow(/collection/u);
  });

  it("accepts a marker reached through a symlink into a child repository", async () => {
    const value = await fixture();
    const project = join(value.root, "symlinked-marker-project");
    await mkdir(project, { mode: 0o700 });
    const repository = await gitRepository(project, "repository");
    await gitRepository(project, "second-repo");
    const target = join(repository, "CLAUDE.md");
    await writeFile(target, "# child project marker\n");
    await symlink(target, join(project, "CLAUDE.md"));
    const canonicalProject = await realpath(project);

    await expect(nearestGitWorkspace(canonicalProject)).resolves.toBeNull();
    await expect(looksLikeRepositoryCollection(canonicalProject)).resolves.toBe(false);
    await expect(runWorkspaceTrust(["trust", project], value.paths)).resolves.toMatchObject({ trusted: true });
  });

  it.each([
    "AGENTS.md",
    "CLAUDE.md",
    ".claude",
    "workspace.code-workspace",
    "package.json",
    "pyproject.toml",
    "Cargo.toml",
    "go.mod",
  ])("honours the project marker %s at the candidate root", async (marker) => {
    const value = await fixture();
    const project = join(value.root, `marked-${marker.replaceAll(/[^a-z0-9]+/giu, "-")}`);
    await mkdir(project, { mode: 0o700 });
    await gitRepository(project, "first-repo");
    await gitRepository(project, "second-repo");
    if (marker === ".claude") await mkdir(join(project, marker), { mode: 0o700 });
    else await writeFile(join(project, marker), "# project marker\n");

    await expect(looksLikeRepositoryCollection(await realpath(project))).resolves.toBe(false);
  });

  it("rejects a non-Git directory with one direct-child repository and no marker", async () => {
    const value = await fixture();
    const project = join(value.root, "ordinary-project");
    await mkdir(project, { mode: 0o700 });
    await gitRepository(project, "repository");

    await expect(runWorkspaceTrust(["trust", project], value.paths)).rejects.toThrow(/repository collection/u);
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
  });

  it("rejects explicit trust when a root-level Git semantic probe is unavailable", async () => {
    const value = await fixture();
    const bare = join(value.root, "unavailable-bare");
    await execFileAsync("git", ["init", "--bare", "--quiet", bare]);
    const shimDirectory = join(value.root, "unavailable-git-shim");
    await mkdir(shimDirectory, { mode: 0o700 });
    const shim = join(shimDirectory, "git");
    await writeFile(shim, "#!/bin/sh\ncase \"$*\" in *--show-toplevel*) printf '%s\\n' 'fatal: this operation must be run in a work tree' >&2; exit 128;; *--is-bare-repository*) printf '%s\\n' 'fatal: bare semantic probe unavailable' >&2; exit 1;; esac\nexit 1\n");
    await chmod(shim, 0o700);
    const previousPath = process.env.PATH;
    process.env.PATH = shimDirectory;
    try {
      await expect(runWorkspaceTrust(["trust", bare], value.paths)).rejects.toThrow(/Git repository probe unavailable|repository collection/iu);
      await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
    } finally {
      if (previousPath === undefined) delete process.env.PATH;
      else process.env.PATH = previousPath;
    }
  });

  it("refuses filesystem root before recording a grant", async () => {
    const value = await fixture();

    await expect(runWorkspaceTrust(["trust", "/"], value.paths)).rejects.toThrow(/filesystem root.*never be trusted/u);
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
  });

  it("refuses the real home directory before recording a grant", async () => {
    const value = await fixture();
    const home = await realpath(homedir());

    await expect(runWorkspaceTrust(["trust", home], value.paths)).rejects.toThrow(/never be trusted.*home-wide/u);
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
  });

  it("refuses a marked resolved home directory before recording a grant", async () => {
    const value = await fixture();
    const home = join(value.root, "home");
    await mkdir(join(home, ".git"), { recursive: true, mode: 0o700 });
    await writeFile(join(home, "AGENTS.md"), "# home marker must not widen trust\n");
    const previousHome = process.env.HOME;
    process.env.HOME = home;
    try {
      expect(await realpath(homedir())).toBe(await realpath(home));
      await expect(runWorkspaceTrust(["trust", home], value.paths)).rejects.toThrow(
        /never be trusted.*home-wide/u,
      );
      await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
    } finally {
      if (previousHome === undefined) delete process.env.HOME;
      else process.env.HOME = previousHome;
    }
  });

  it("reports ancestor broadening when an exact repository was trusted before its parent collection", async () => {
    const value = await fixture();
    const collection = join(value.root, "projects");
    await mkdir(collection, { mode: 0o700 });
    const first = await gitRepository(collection, "first-repo");
    await gitRepository(collection, "second-repo");

    await runWorkspaceTrust(["trust", first], value.paths);
    await expect(runWorkspaceTrust(["trust", collection], value.paths))
      .rejects.toThrow(new RegExp(`ancestor broadening over ${await realpath(first)}`, "u"));
  });

  it("treats workspace status as an inspect alias", async () => {
    const value = await fixture();
    await runWorkspaceTrust(["trust", value.workspace], value.paths);

    await expect(runWorkspaceTrust(["status", value.workspace], value.paths))
      .resolves.toEqual(await runWorkspaceTrust(["inspect", value.workspace], value.paths));
  });

  it("rejects symbolic-link roots and supports inspect/revoke without widening", async () => {
    const value = await fixture();
    const linked = join(value.root, "linked");
    await symlink(value.workspace, linked);
    await expect(runWorkspaceTrust(["trust", linked], value.paths)).rejects.toThrow(/symbolic-link/u);

    await runWorkspaceTrust(["trust", value.workspace], value.paths, new Date("2026-07-11T04:00:00.000Z"));
    await expect(runWorkspaceTrust(["inspect", value.workspace], value.paths)).resolves.toMatchObject({ trusted: true });
    await expect(runWorkspaceTrust(["revoke", value.workspace], value.paths)).resolves.toMatchObject({ revoked: true });
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
  });

  it("fails closed for a non-private or symlinked registry", async () => {
    const value = await fixture();
    const registryPath = join(value.paths.stateDirectory, "trusted-workspaces.json");
    await symlink(join(value.root, "missing"), registryPath);
    await expect(runWorkspaceTrust(["list"], value.paths)).rejects.toThrow(/private regular file/u);
  });

  it("serialises concurrent grants and a following revoke without lost updates", async () => {
    const value = await fixture();
    const second = join(value.root, "workspace-two");
    await mkdir(second, { mode: 0o700 });
    await Promise.all([
      runWorkspaceTrust(["trust", value.workspace], value.paths),
      runWorkspaceTrust(["trust", second], value.paths),
    ]);
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({
      entries: expect.arrayContaining([
        expect.objectContaining({ canonicalPath: await realpath(value.workspace) }),
        expect.objectContaining({ canonicalPath: await realpath(second) }),
      ]),
    });
    await Promise.all([
      runWorkspaceTrust(["trust", value.workspace], value.paths),
      runWorkspaceTrust(["revoke", value.workspace], value.paths),
    ]);
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({
      entries: expect.arrayContaining([expect.objectContaining({ canonicalPath: await realpath(second) })]),
    });
    expect((await lstat(join(value.paths.stateDirectory, "trusted-workspaces.lock.sqlite3"))).mode & 0o077).toBe(0);
  });

  it("rejects lexical and registered-root ancestor broadening", async () => {
    const value = await fixture();
    await expect(runWorkspaceTrust(["trust", `${value.workspace}/..`], value.paths)).rejects.toThrow(/ancestor broadening/u);
    const nested = join(value.workspace, "nested");
    await mkdir(nested);
    await runWorkspaceTrust(["trust", nested], value.paths);
    await expect(runWorkspaceTrust(["trust", value.workspace], value.paths)).rejects.toThrow(/ancestor broadening/u);
  });

  it("reports an exact-root retrust as already trusted before checking descendants", async () => {
    const value = await fixture();
    const first = await runWorkspaceTrust(
      ["trust", value.workspace],
      value.paths,
      new Date("2026-07-11T04:00:00.000Z"),
    );
    const nested = join(value.workspace, "nested");
    await mkdir(nested);
    await runWorkspaceTrust(["trust", nested], value.paths);

    await expect(runWorkspaceTrust(
      ["trust", value.workspace],
      value.paths,
      new Date("2026-07-11T05:00:00.000Z"),
    )).resolves.toEqual({
      schemaVersion: 2,
      trusted: true,
      alreadyTrusted: true,
      entry: first.entry,
    });
  });

  it("rejects ancestor broadening when the exact-root record has stale identity", async () => {
    const value = await fixture();
    await runWorkspaceTrust(["trust", value.workspace], value.paths);
    const nested = join(value.workspace, "nested");
    await mkdir(nested);
    await runWorkspaceTrust(["trust", nested], value.paths);

    await rename(value.workspace, join(value.root, "workspace-original"));
    await mkdir(value.workspace);
    await mkdir(nested);

    await expect(runWorkspaceTrust(["trust", value.workspace], value.paths))
      .rejects.toThrow(/ancestor broadening/u);
  });

  async function driftRecordedDevice(paths: { stateDirectory: string }, canonicalPath: string): Promise<number> {
    const registryPath = join(paths.stateDirectory, "trusted-workspaces.json");
    const registry = JSON.parse(await readFile(registryPath, "utf8")) as {
      entries: { canonicalPath: string; device: number }[];
    };
    const entry = registry.entries.find((item) => item.canonicalPath === canonicalPath);
    if (entry === undefined) throw new Error("expected a record to drift");
    const stale = entry.device + 3;
    entry.device = stale;
    await writeFile(registryPath, `${JSON.stringify(registry, null, 2)}\n`);
    return stale;
  }

  it("honours a record whose device drifted while its inode still matches", async () => {
    const value = await fixture();
    await runWorkspaceTrust(["trust", value.workspace], value.paths);
    const canonicalPath = await realpath(value.workspace);
    await driftRecordedDevice(value.paths, canonicalPath);

    await expect(runWorkspaceTrust(["inspect", value.workspace], value.paths))
      .resolves.toMatchObject({ trusted: true });
    await expect(trustedWorkspaceRoots({ stateDirectory: value.paths.stateDirectory }))
      .resolves.toContain(canonicalPath);
    await expect(trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.workspace,
    })).resolves.toMatchObject({ canonicalRoot: canonicalPath });
  });

  it("heals a drifted device on retrust instead of refusing it as ancestor broadening", async () => {
    const value = await fixture();
    await runWorkspaceTrust(["trust", value.workspace], value.paths);
    const nested = join(value.workspace, "nested");
    await mkdir(nested);
    await runWorkspaceTrust(["trust", nested], value.paths);
    const canonicalPath = await realpath(value.workspace);
    const stale = await driftRecordedDevice(value.paths, canonicalPath);
    const live = (await lstat(canonicalPath)).dev;

    const result = await runWorkspaceTrust(["trust", value.workspace], value.paths) as {
      entry: { device: number };
    };
    expect(result.entry.device).toBe(live);
    expect(result.entry.device).not.toBe(stale);
  });

  it("still refuses a root whose inode changed, which a device drift must not mask", async () => {
    const value = await fixture();
    await runWorkspaceTrust(["trust", value.workspace], value.paths);
    const canonicalPath = await realpath(value.workspace);
    await rename(value.workspace, join(value.root, "workspace-original"));
    await mkdir(value.workspace);

    await expect(runWorkspaceTrust(["inspect", value.workspace], value.paths))
      .resolves.toMatchObject({ trusted: false });
    await expect(trustedWorkspaceRoots({ stateDirectory: value.paths.stateDirectory }))
      .resolves.not.toContain(canonicalPath);
  });

  it("revokes a record whose directory has been removed", async () => {
    const value = await fixture();
    const nested = join(value.workspace, "nested");
    await mkdir(nested);
    await runWorkspaceTrust(["trust", nested], value.paths);
    const canonicalNested = await realpath(nested);
    await rm(nested, { recursive: true });

    await expect(runWorkspaceTrust(["revoke", nested], value.paths))
      .resolves.toMatchObject({ revoked: true, canonicalPath: canonicalNested });
    await expect(runWorkspaceTrust(["list"], value.paths))
      .resolves.toMatchObject({ entries: [] });
  });

  it("reports per-entry liveness from list so a dead record is visible", async () => {
    const value = await fixture();
    const nested = join(value.workspace, "nested");
    await mkdir(nested);
    await runWorkspaceTrust(["trust", value.workspace], value.paths);
    await runWorkspaceTrust(["trust", nested], value.paths);
    const canonicalNested = await realpath(nested);
    await rm(nested, { recursive: true });

    const listed = await runWorkspaceTrust(["list"], value.paths) as {
      entries: { canonicalPath: string; trusted: boolean; identity: string }[];
    };
    const dead = listed.entries.find((entry) => entry.canonicalPath === canonicalNested);
    expect(dead).toMatchObject({ trusted: false, identity: "mismatch" });
    const canonicalWorkspacePath = await realpath(value.workspace);
    const live = listed.entries.find((entry) => entry.canonicalPath === canonicalWorkspacePath);
    expect(live).toMatchObject({ trusted: true, identity: "match" });
  });

  it("applies explicit profile and expiry changes to an already trusted exact root", async () => {
    const value = await fixture();
    await runWorkspaceTrust(["trust", value.workspace], value.paths);
    const nested = join(value.workspace, "nested");
    await mkdir(nested);
    await runWorkspaceTrust(["trust", nested], value.paths);
    const now = new Date("2026-07-11T04:00:00.000Z");

    await expect(runWorkspaceTrust([
      "trust", value.workspace, "--profiles", "observed", "--expires-at", "2026-07-12T04:00:00.000Z",
    ], value.paths, now)).resolves.toMatchObject({
      trusted: true,
      entry: {
        approvedAt: now.toISOString(),
        allowedProfiles: ["observed"],
        expiresAt: "2026-07-12T04:00:00.000Z",
      },
    });
  });

  it("recovers an expired exact-root record with the plain trust command", async () => {
    const value = await fixture();
    const approved = new Date("2026-07-11T04:00:00.000Z");
    await runWorkspaceTrust([
      "trust", value.workspace, "--profiles", "observed", "--expires-at", "2026-07-12T04:00:00.000Z",
    ], value.paths, approved);
    const nested = join(value.workspace, "nested");
    await mkdir(nested);
    await runWorkspaceTrust(["trust", nested], value.paths, approved);
    const retrustedAt = new Date("2026-07-13T04:00:00.000Z");

    const result = await runWorkspaceTrust(["trust", value.workspace], value.paths, retrustedAt);
    expect(result).toMatchObject({
      trusted: true,
      entry: {
        canonicalPath: await realpath(value.workspace),
        approvedAt: retrustedAt.toISOString(),
        allowedProfiles: ["observed"],
      },
    });
    expect(result.entry).not.toHaveProperty("expiresAt");
    await expect(trustedWorkspaceIdentity({
      stateDirectory: value.paths.stateDirectory,
      canonicalRoot: value.workspace,
      now: retrustedAt,
    })).resolves.toMatchObject({ canonicalRoot: await realpath(value.workspace) });
  });

  it("does not transfer trust when the path identity is replaced or becomes a symlink", async () => {
    const value = await fixture();
    await runWorkspaceTrust(["trust", value.workspace], value.paths);
    const original = join(value.root, "workspace-original");
    await rename(value.workspace, original);
    await mkdir(value.workspace);
    await expect(trustedWorkspaceRoots({ stateDirectory: value.paths.stateDirectory, executionProfile: "headless" })).resolves.toEqual([]);
    await expect(runWorkspaceTrust(["inspect", value.workspace], value.paths)).resolves.toMatchObject({ trusted: false });
    await rm(value.workspace, { recursive: true });
    await symlink(original, value.workspace);
    await expect(trustedWorkspaceRoots({ stateDirectory: value.paths.stateDirectory, executionProfile: "headless" })).resolves.toEqual([]);
  });
});
