import { execFileSync, spawnSync } from "node:child_process";
import {
  cpSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { identify, projectRoot } from "../src/identity.js";

const cliPath = fileURLToPath(new URL("../src/cli.ts", import.meta.url));
const serverPath = fileURLToPath(new URL("../src/server.ts", import.meta.url));
const tsxLoader = createRequire(import.meta.url).resolve("tsx");

let fixture: string;
let primary: string;
let linked: string;
let copied: string;
let separate: string;

function git(cwd: string, ...args: string[]): string {
  return execFileSync("git", args, {
    cwd,
    env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_NOSYSTEM: "1" },
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  }).trim();
}

function initialise(root: string, file: string): void {
  mkdirSync(root, { recursive: true });
  git(root, "init", "--quiet");
  git(root, "config", "user.email", "fabric@example.invalid");
  git(root, "config", "user.name", "Fabric test");
  writeFileSync(join(root, file), "fixture\n");
  git(root, "add", file);
  git(root, "commit", "--quiet", "-m", "fixture");
}

function runCli(cwd: string, state: string, label: string, ...args: string[]) {
  return spawnSync(process.execPath, ["--import", tsxLoader, cliPath, ...args], {
    cwd,
    encoding: "utf8",
    env: {
      ...process.env,
      AGENT_FABRIC_STATE_DIRECTORY: state,
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: label,
      NODE_NO_WARNINGS: "1",
    },
  });
}

function toolPayload(result: unknown): unknown {
  const content = (result as { content: Array<{ type: string; text?: string }> }).content;
  const block = content.find((item) => item.type === "text" && item.text !== undefined);
  if (block?.text === undefined) throw new Error("MCP result had no text payload");
  return JSON.parse(block.text);
}

beforeEach(() => {
  fixture = mkdtempSync(join(tmpdir(), "fabric-project-identity-"));
  primary = join(fixture, "primary");
  linked = join(primary, ".worktrees", "linked");
  copied = join(fixture, "copied");
  separate = join(fixture, "separate");

  initialise(primary, "README");
  mkdirSync(dirname(linked), { recursive: true });
  git(primary, "worktree", "add", "--quiet", "--detach", linked, "HEAD");
  cpSync(linked, copied, { recursive: true });
  initialise(separate, "OTHER");
});

afterEach(() => rmSync(fixture, { recursive: true, force: true }));

describe("project identity", () => {
  it("shares persistent MCP and CLI coordination across registered worktrees", async () => {
    const chair = identify({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "chair",
    }, primary);
    const worker = identify({
      AGENT_FABRIC_SEAT: "codex",
      AGENT_FABRIC_LABEL: "worker",
    }, linked);

    expect(chair.project).toBe(realpathSync(primary));
    expect(worker.project).toBe(chair.project);
    expect(worker.cwd).toBe(realpathSync(linked));
    const nested = join(linked, "nested", "source");
    mkdirSync(nested, { recursive: true });
    expect(projectRoot(nested)).toBe(chair.project);

    const state = join(fixture, "state");
    const announced = runCli(linked, state, "worker", "whoami");
    expect(announced.status, announced.stderr).toBe(0);
    expect(JSON.parse(announced.stdout)).toMatchObject({
      project: realpathSync(primary),
      cwd: realpathSync(linked),
    });

    const transport = new StdioClientTransport({
      command: process.execPath,
      args: ["--import", tsxLoader, serverPath],
      cwd: primary,
      stderr: "pipe",
      env: {
        ...process.env,
        AGENT_FABRIC_STATE_DIRECTORY: state,
        AGENT_FABRIC_SEAT: "codex",
        AGENT_FABRIC_LABEL: "chair",
        NODE_NO_WARNINGS: "1",
      },
    });
    const client = new Client({ name: "worktree-identity-test", version: "1" });
    try {
      await client.connect(transport);
      await client.callTool({
        name: "fabric_task_create",
        arguments: { objective: "review the linked-worktree change", task_id: "worktree-review" },
      });
      await client.callTool({
        name: "fabric_team_create",
        arguments: { team_id: "reviewers", members: ["worker"] },
      });
      const sent = await client.callTool({
        name: "fabric_send",
        arguments: { to: "reviewers", body: "continue in the linked worktree" },
      });
      const sentMessage = toolPayload(sent) as { messageId: string };

      const claimed = runCli(linked, state, "worker", "claim", "worktree-review");
      expect(claimed.status, claimed.stderr).toBe(0);
      expect(JSON.parse(claimed.stdout)).toMatchObject({ owner: "worker", state: "open" });
      const inbox = runCli(linked, state, "worker", "inbox");
      expect(inbox.status, inbox.stderr).toBe(0);
      const deliveries = JSON.parse(inbox.stdout) as Array<{
        messageId: string;
        claimId: string;
        body: string;
      }>;
      expect(deliveries).toContainEqual(expect.objectContaining({
        body: "continue in the linked worktree",
      }));
      const delivery = deliveries.find((item) => item.messageId === sentMessage.messageId)!;
      const acknowledged = runCli(
        linked, state, "worker", "ack", delivery.messageId, delivery.claimId,
      );
      expect(acknowledged.status, acknowledged.stderr).toBe(0);
      const replied = runCli(
        linked, state, "worker", "send", "chair", "review complete",
        "--reply-to", sentMessage.messageId,
      );
      expect(replied.status, replied.stderr).toBe(0);
      const chairInbox = toolPayload(await client.callTool({
        name: "fabric_inbox",
        arguments: {},
      })) as Array<{ body: string; claimId: string; messageId: string; replyTo: string }>;
      expect(chairInbox).toContainEqual(expect.objectContaining({
        body: "review complete",
        replyTo: sentMessage.messageId,
      }));
      const response = chairInbox.find((item) => item.body === "review complete")!;
      await client.callTool({
        name: "fabric_acknowledge",
        arguments: { message_id: response.messageId, claim_id: response.claimId },
      });
      const completed = runCli(linked, state, "worker", "done", "worktree-review");
      expect(completed.status, completed.stderr).toBe(0);
      expect(JSON.parse(completed.stdout)).toMatchObject({ state: "done" });
      const activity = toolPayload(await client.callTool({
        name: "fabric_activity",
        arguments: { limit: 50 },
      })) as Array<{ agentId: string }>;
      expect(new Set(activity.map((entry) => entry.agentId))).toEqual(new Set(["chair", "worker"]));
    } finally {
      await client.close().catch(() => undefined);
    }
  }, 20_000);

  it("keeps separate repositories and copied worktree metadata isolated", () => {
    const primaryProject = projectRoot(primary);
    expect(projectRoot(separate)).not.toBe(primaryProject);
    expect(projectRoot(copied)).not.toBe(primaryProject);

    const staleRegistered = join(fixture, "stale-registered");
    git(primary, "worktree", "add", "--quiet", "--detach", staleRegistered, "HEAD");
    writeFileSync(join(staleRegistered, ".git"), readFileSync(join(linked, ".git")));
    expect(projectRoot(staleRegistered)).not.toBe(primaryProject);

    const plain = join(fixture, "plain");
    mkdirSync(plain);
    expect(projectRoot(plain)).toBe(plain);

    const malformed = join(fixture, "malformed");
    mkdirSync(malformed);
    writeFileSync(join(malformed, ".git"), "gitdir: missing\0metadata\n");
    expect(projectRoot(malformed)).toBe(malformed);

    const separateWorktree = join(fixture, "separate-git-dir");
    const separateMetadata = join(fixture, "separate-git-metadata");
    const separateLinked = join(fixture, "separate-git-linked");
    mkdirSync(separateWorktree);
    git(separateWorktree, "init", "--quiet", "--separate-git-dir", separateMetadata);
    git(separateWorktree, "config", "user.email", "fabric@example.invalid");
    git(separateWorktree, "config", "user.name", "Fabric test");
    writeFileSync(join(separateWorktree, "SEPARATE"), "fixture\n");
    git(separateWorktree, "add", "SEPARATE");
    git(separateWorktree, "commit", "--quiet", "-m", "fixture");
    git(separateWorktree, "worktree", "add", "--quiet", "--detach", separateLinked, "HEAD");
    expect(projectRoot(separateWorktree)).toBe(realpathSync(separateWorktree));
    expect(projectRoot(separateLinked)).not.toBe(projectRoot(separateWorktree));
    const copiedSeparateMain = join(fixture, "copied-separate-git-main");
    cpSync(separateWorktree, copiedSeparateMain, { recursive: true });
    expect(projectRoot(copiedSeparateMain)).not.toBe(projectRoot(separateWorktree));
    const copiedSeparateLinked = join(fixture, "copied-separate-git-linked");
    cpSync(separateLinked, copiedSeparateLinked, { recursive: true });
    expect(projectRoot(copiedSeparateLinked)).not.toBe(projectRoot(separateWorktree));

    git(primary, "-c", "protocol.file.allow=always", "submodule", "add", "--quiet", separate,
      "vendor/separate");
    const submodule = join(primary, "vendor", "separate");
    const submoduleLinked = join(fixture, "submodule-linked");
    git(submodule, "worktree", "add", "--quiet", "--detach", submoduleLinked, "HEAD");
    expect(projectRoot(submoduleLinked)).not.toBe(projectRoot(submodule));
    expect(projectRoot(submodule)).not.toBe(primaryProject);

    const newlinePrimary = join(fixture, "primary\nline");
    const newlineLinked = join(fixture, "linked\nline");
    initialise(newlinePrimary, "NEWLINE");
    git(newlinePrimary, "worktree", "add", "--quiet", "--detach", newlineLinked, "HEAD");
    expect(projectRoot(newlineLinked)).toBe(projectRoot(newlinePrimary));

    if (process.platform !== "win32") {
      const carriageReturnLinked = join(fixture, "linked\r");
      git(primary, "worktree", "add", "--quiet", "--detach", carriageReturnLinked, "HEAD");
      expect(projectRoot(carriageReturnLinked)).toBe(primaryProject);
    }
  });

  it("ignores inherited Git redirects when deriving the project", () => {
    expect(projectRoot(linked, {
      GIT_DIR: join(separate, ".git"),
      GIT_WORK_TREE: separate,
      GIT_OBJECT_DIRECTORY: join(separate, "missing-objects"),
    })).toBe(realpathSync(primary));
  });
});
