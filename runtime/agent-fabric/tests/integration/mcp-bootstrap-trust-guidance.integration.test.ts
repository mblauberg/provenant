import { execFile } from "node:child_process";
import { mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join, parse } from "node:path";
import { promisify } from "node:util";

import { afterEach, describe, expect, it } from "vitest";

import { bootstrapMcpSeat } from "../../src/cli/mcp-bootstrap.ts";

const temporaryDirectories: string[] = [];
const execFileAsync = promisify(execFile);

function paths(root: string) {
  return {
    stateDirectory: join(root, "state"),
    runtimeDirectory: join(root, "runtime"),
    databasePath: join(root, "state", "fabric-v1.sqlite3"),
    socketPath: join(root, "runtime", "fabric-v1.sock"),
  };
}

async function bootstrapFailure(cwd: string, root: string): Promise<Error & { code?: string }> {
  try {
    await bootstrapMcpSeat({
      environment: {
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_PRODUCT_ROOT: join(root, "product"),
      },
      cwd,
      paths: paths(root),
    });
    throw new Error("expected bootstrap to reject an untrusted workspace");
  } catch (error: unknown) {
    return error as Error & { code?: string };
  }
}

afterEach(async () => {
  await Promise.all(temporaryDirectories.splice(0).map(
    async (path) => await rm(path, { recursive: true, force: true }),
  ));
});

describe("MCP bootstrap workspace-trust guidance", () => {
  it("auto-enrols the repository root when bootstrap starts in a subdirectory", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-repository-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const root = await realpath(temporaryRoot);
    const project = join(root, "packages", "agent");
    await mkdir(join(root, ".git"));
    await mkdir(project, { recursive: true });

    await expect(bootstrapMcpSeat({
      environment: {
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_PRODUCT_ROOT: join(root, "product"),
      },
      cwd: project,
      paths: paths(root),
    })).rejects.toMatchObject({
      code: "BOOTSTRAP_SPAWN_FAILED",
    });
  });

  it("withholds a trust command for a linked worktree", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-worktree-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const mainRepository = join(temporaryRoot, "main");
    const worktree = join(temporaryRoot, "linked");
    await mkdir(mainRepository);
    await execFileAsync("git", ["-C", mainRepository, "init", "--quiet"]);
    await execFileAsync("git", ["-C", mainRepository, "config", "user.email", "fabric-tests@example.invalid"]);
    await execFileAsync("git", ["-C", mainRepository, "config", "user.name", "Fabric Tests"]);
    await writeFile(join(mainRepository, "README.md"), "linked worktree fixture\n");
    await execFileAsync("git", ["-C", mainRepository, "add", "README.md"]);
    await execFileAsync("git", ["-C", mainRepository, "commit", "--quiet", "-m", "fixture"]);
    await execFileAsync("git", ["-C", mainRepository, "worktree", "add", "--detach", "--quiet", worktree]);

    const failure = await bootstrapFailure(await realpath(worktree), temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/linked worktree.*user-only/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });

  it("withholds a trust command for a collection of repositories", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-collection-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const collection = join(temporaryRoot, "projects");
    await mkdir(join(collection, "one", ".git"), { recursive: true });
    await mkdir(join(collection, "two", ".git"), { recursive: true });

    const failure = await bootstrapFailure(await realpath(collection), temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/repository collection.*one.*two.*project activate/isu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });

  it("withholds a trust command for a collection of bare repositories", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-bare-collection-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const collection = join(temporaryRoot, "projects");
    const first = join(collection, "one");
    const second = join(collection, "two");
    await execFileAsync("git", ["init", "--bare", "--quiet", first]);
    await execFileAsync("git", ["init", "--bare", "--quiet", second]);

    const failure = await bootstrapFailure(await realpath(collection), temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/repository collection.*one.*two.*inspect and repair/isu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });

  it("refuses a standalone bare repository as an automatic project boundary", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-standalone-bare-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const bare = join(temporaryRoot, "bare");
    await execFileAsync("git", ["init", "--bare", "--quiet", bare]);

    const failure = await bootstrapFailure(await realpath(bare), temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/standalone bare Git repository.*no automatic trust/isu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });

  it("refuses a symlinked plain non-Git root without widening trust", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-symlinked-non-git-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const target = join(temporaryRoot, "target");
    const linked = join(temporaryRoot, "linked");
    await mkdir(target);
    await symlink(target, linked);

    const failure = await bootstrapFailure(linked, temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/symbolic-link|symlink/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });

  it("refuses a plain non-Git root reached through a symlinked ancestor", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-symlinked-ancestor-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const targetParent = join(temporaryRoot, "target-parent");
    const linkedParent = join(temporaryRoot, "linked-parent");
    const project = join(targetParent, "project");
    await mkdir(project, { recursive: true });
    await symlink(targetParent, linkedParent);

    const failure = await bootstrapFailure(join(linkedParent, "project"), temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/symbolic-link|symlink/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });

  it("gives malformed collection children repair guidance without activation commands", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-malformed-collection-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const collection = join(temporaryRoot, "projects");
    const valid = join(collection, "valid");
    const malformed = join(collection, "malformed");
    await mkdir(join(valid, ".git"), { recursive: true });
    await mkdir(malformed, { recursive: true });
    await writeFile(join(malformed, ".git"), "not a gitdir marker\n");

    const failure = await bootstrapFailure(await realpath(collection), temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringContaining(`provenant project activate '${await realpath(valid)}'`),
    });
    expect(failure.message).toMatch(/inspect and repair.*malformed/isu);
    expect(failure.message).not.toContain(`provenant project activate '${await realpath(malformed)}'`);
  });

  it("explains that home-wide trust is forbidden", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-home-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);

    const failure = await bootstrapFailure(await realpath(homedir()), temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/exact path can never be trusted.*home-wide authority.*forbidden by policy/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });

  it("withholds a trust command when the repository root is home", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-home-repository-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const home = join(temporaryRoot, "home");
    const project = join(home, "project");
    await mkdir(join(home, ".git"), { recursive: true });
    await mkdir(project);
    const canonicalHome = await realpath(home);
    const canonicalProject = await realpath(project);
    const previousHome = process.env.HOME;
    process.env.HOME = canonicalHome;
    try {
      const failure = await bootstrapFailure(canonicalProject, temporaryRoot);

      expect(failure).toMatchObject({
        code: "WORKSPACE_NOT_TRUSTED",
        message: expect.stringMatching(/exact path can never be trusted.*home-wide trust.*forbidden by policy/iu),
      });
      expect(failure.message).not.toContain("workspace trust");
    } finally {
      if (previousHome === undefined) delete process.env.HOME;
      else process.env.HOME = previousHome;
    }
  });

  it("withholds a trust command for the filesystem root", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-root-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);

    const failure = await bootstrapFailure(
      await realpath(parse(homedir()).root),
      temporaryRoot,
    );

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/filesystem root.*can never be trusted.*forbidden by policy/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });

  it("withholds a trust command when the repository boundary cannot be inspected", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-unreadable-git-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const project = join(temporaryRoot, "project");
    await mkdir(project);
    await symlink("missing-gitdir", join(project, ".git"));

    const failure = await bootstrapFailure(project, temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/inspect and repair.*boundary evidence/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });

  it("withholds a trust command when a gitfile target is missing", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-missing-gitdir-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const project = join(temporaryRoot, "project");
    await mkdir(project);
    await writeFile(join(project, ".git"), "gitdir: missing-gitdir\n");

    const failure = await bootstrapFailure(project, temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/inspect and repair.*boundary evidence/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });
});
