import { createHash } from "node:crypto";
import { chmod, lstat, mkdir, mkdtemp, readFile, realpath, rename, rm, symlink, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import {
  runWorkspaceTrust,
  trustedWorkspaceIdentity,
  trustedWorkspaceRoots,
} from "../../src/cli/workspace-trust.ts";

const temporaryDirectories: string[] = [];

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
      approvedBy: "local-operator",
      canonicalPath: await realpath(value.workspace),
      device: identity.entry.device,
      expiresAt: "2026-07-12T04:00:00.000Z",
      inode: identity.entry.inode,
    });
    expect(identity).toEqual({
      canonicalRoot: await realpath(value.workspace),
      trustRecordDigest: `sha256:${createHash("sha256").update(canonicalEntry).digest("hex")}`,
      entry: {
        canonicalPath: await realpath(value.workspace),
        approvedAt: now.toISOString(),
        approvedBy: "local-operator",
        device: identity.entry.device,
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
    expect(JSON.parse(await readFile(registryPath, "utf8"))).toMatchObject({ schemaVersion: 1 });
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

  it("trusts a non-Git directory beside repository neighbours", async () => {
    const value = await fixture();
    await gitRepository(value.root, "first-repo");
    await gitRepository(value.root, "second-repo");
    const project = join(value.root, "ordinary-project");
    await mkdir(project, { mode: 0o700 });

    await expect(runWorkspaceTrust(["trust", project], value.paths)).resolves.toMatchObject({ trusted: true });
  });

  it("refuses filesystem root before recording a grant", async () => {
    const value = await fixture();

    await expect(runWorkspaceTrust(["trust", "/"], value.paths)).rejects.toThrow(/filesystem root.*never be trusted/u);
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
  });

  it("refuses the real home directory before recording a grant", async () => {
    const value = await fixture();
    const home = await realpath(homedir());

    await expect(runWorkspaceTrust(["trust", home], value.paths)).rejects.toThrow(/home-wide.*never be trusted/u);
    await expect(runWorkspaceTrust(["list"], value.paths)).resolves.toMatchObject({ entries: [] });
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
      schemaVersion: 1,
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
