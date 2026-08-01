import { mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join, parse } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { bootstrapMcpSeat } from "../../src/cli/mcp-bootstrap.ts";

const temporaryDirectories: string[] = [];

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
  it("recommends the repository root when bootstrap starts in a subdirectory", async () => {
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
      code: "WORKSPACE_NOT_TRUSTED",
      message: `Fabric bootstrap requires the exact current project root to be trusted; run '${join(root, "product", "scripts", "agent-fabric")}' workspace trust '${root}'; then retry fabric_bootstrap from '${root}'`,
    });
  });

  it("withholds a trust command for a linked worktree", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-worktree-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);
    const mainRepository = join(temporaryRoot, "main");
    const worktree = join(temporaryRoot, "linked");
    const worktreeGitDirectory = join(mainRepository, ".git", "worktrees", "linked");
    await mkdir(worktreeGitDirectory, { recursive: true });
    await mkdir(worktree);
    await writeFile(join(worktree, ".git"), `gitdir: ${worktreeGitDirectory}\n`);

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
      message: expect.stringMatching(/parent collection.*several repositories.*policy.*specific project/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });

  it("explains that home-wide trust is forbidden", async () => {
    const temporaryRoot = await mkdtemp(join(tmpdir(), "fabric-home-bootstrap-"));
    temporaryDirectories.push(temporaryRoot);

    const failure = await bootstrapFailure(await realpath(homedir()), temporaryRoot);

    expect(failure).toMatchObject({
      code: "WORKSPACE_NOT_TRUSTED",
      message: expect.stringMatching(/exact path can never be trusted.*home-wide trust.*forbidden by policy/iu),
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
    await writeFile(join(home, "AGENTS.md"), "# home marker must not widen trust\n");
    const previousHome = process.env.HOME;
    process.env.HOME = home;
    try {
      const failure = await bootstrapFailure(project, temporaryRoot);

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
      message: expect.stringMatching(/cannot safely determine.*trust boundary.*no trust command/iu),
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
      message: expect.stringMatching(/cannot safely determine.*trust boundary.*no trust command/iu),
    });
    expect(failure.message).not.toContain("workspace trust");
  });
});
