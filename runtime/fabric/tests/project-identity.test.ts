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
  const payload=JSON.parse(block.text);
  return payload.messages ?? payload.activity ?? payload;
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
        FABRIC_LEGACY_TOOLS: "1",
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
        FABRIC_LEGACY_TOOLS: "1",
        NODE_NO_WARNINGS: "1",
      },
    });
    const client = new Client({ name: "worktree-identity-test", version: "1" });
    try {
      await client.connect(transport);
      await client.callTool({
        name: "fabric_task",
        arguments: { action:"create", objective: "review the linked-worktree change", task_id: "worktree-review" },
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
        arguments: {claim:true},
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

it('stores linked and symlinked cwd runs in the primary run root', async () => {
 const identity = await import('../src/identity.js');
 expect('runRoot' in identity).toBe(true);
 if (!('runRoot' in identity)) return;
 const root = identity.runRoot as (cwd:string)=>string;
 expect(root(linked)).toBe(join(realpathSync(primary),'.agent-run'));
 expect(root(primary)).toBe(join(realpathSync(primary),'.agent-run'));
 expect(root(fixture)).toBe(join(realpathSync(fixture),'.agent-run'));
});

 it('passes the shared layout contract cases and a symlinked cwd', async () => {
  const {runRoot}=await import('../src/identity.js');
  const {symlinkSync}=await import('node:fs');
  const shared=join(import.meta.dirname,'../../../tests/fixtures/fabric-v1/layout-cases.json');
  const cases=JSON.parse(readFileSync(shared,'utf8'));
  const alias=join(fixture,'alias');mkdirSync(alias);
  const substitute=(path:string)=>path.replace('/repo/.worktrees/lane',linked).replace('/repo',primary).replace('/workspace',fixture).replace('/alias',alias);
  for(const item of cases) {
   if(item.resolved_cwd) symlinkSync(substitute(item.resolved_cwd),substitute(item.cwd));
   const expected=item.agent_run_dir ? substitute(item.agent_run_dir) : join(substitute(item.run_root),'.agent-run');
   expect(runRoot(substitute(item.cwd)),item.name).toBe(join(realpathSync(dirname(expected)),'.agent-run'));
  }
 });

it('finds and cancels retained cwd-local legacy runs from a linked worktree', async () => {
 const {statusRows,findRecordedRun}=await import('../src/run-registry.js');
 const {cancelConfiguredRun}=await import('../src/execution.js');
 const dir=join(realpathSync(linked),'.agent-run/mcp-legacy');mkdirSync(dir,{recursive:true});
 writeFileSync(join(dir,'dispatch-status.json'),JSON.stringify({id:'mcp-legacy',status:'running',started_at:new Date().toISOString()}));
 writeFileSync(join(dir,'dispatch-owner.json'),JSON.stringify({schema_version:1,kind:'dispatch',run_dir:dir,workspace:linked,run_token:'gone',owner_pid:999991,owner_pgid:999991,owner_started_at:null,host_pid:999992,host_started_at:null,started_at:new Date().toISOString(),owner_stdout:'',owner_stderr:''}));
 expect(findRecordedRun(linked,'mcp-legacy')?.run_dir).toBe(dir);
 expect((await statusRows(linked,['mcp-legacy'])).runs?.[0]).toMatchObject({run_dir:dir,status:'interrupted'});
 const who={project:primary,cwd:linked,provider:'codex',agentId:'worker'};
 expect((await cancelConfiguredRun('mcp-legacy',who)).runs).toBeDefined();
});

it('computes the ledger once per selected worktree and skips terminal brief rows', async () => {
 const {statusRows}=await import('../src/run-registry.js');
 const {chmodSync,existsSync}=await import('node:fs');
 const executable=execFileSync('/usr/bin/which',['git'],{encoding:'utf8'}).trim();
 const bin=join(fixture,'bin'),log=join(fixture,'git-calls');mkdirSync(bin);
 const quote=(value:string)=>"'"+value.replaceAll("'", "'\"'\"'")+"'";
 writeFileSync(join(bin,'git'),`#!/bin/sh\nif [ "$1" = '-C' ]; then echo "$2" >> ${quote(log)}; fi\nexec ${quote(executable)} "$@"\n`);chmodSync(join(bin,'git'),0o755);
 for(const suffix of ['aaaaaa','bbbbbb','cccccc']) {
  const dir=join(primary,`.agent-run/runs/20260923-1012-dispatch-test-${suffix}`);
  const path=join(dir,'tasks/task-1/attempt-001');mkdirSync(path,{recursive:true});
  writeFileSync(join(path,'attempt.json'),JSON.stringify({schema:'fabric.attempt.v1',run_id:`mcp-${suffix}`,task_id:'task-1',attempt:1,state:'terminal',status:'ok',worktree:suffix==='cccccc'?separate:linked,started_at:new Date().toISOString(),paths:{}}));
 }
 const old=process.env.PATH;process.env.PATH=`${bin}:${old}`;
 try {
  const brief=await statusRows(primary,['mcp-aaaaaa']);
  expect(brief.runs?.[0]).not.toHaveProperty('branch_tip');
  expect(existsSync(log)).toBe(false);
  const full=await statusRows(primary,['mcp-aaaaaa','mcp-bbbbbb'],0,'all',undefined,'full');
  expect(full.runs?.every(row=>typeof row.branch_tip==='string')).toBe(true);
  expect(readFileSync(log,'utf8').trim().split('\n')).toEqual([linked,linked]);
 } finally {if(old===undefined) delete process.env.PATH;else process.env.PATH=old;}
});
