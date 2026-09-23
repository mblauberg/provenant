import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { createRequire } from "node:module";
import { expect, it } from "vitest";

it.each([false, true])("exposes exactly twelve default tools within budget (legacy=%s)", async (legacy) => {
  const state = mkdtempSync(join(tmpdir(), "fabric-surface-"));
  const client = new Client({ name: "surface", version: "1" });
  try {
    await client.connect(
      new StdioClientTransport({
        command: process.execPath,
        args: [
          "--import",
          createRequire(import.meta.url).resolve("tsx"),
          resolve(import.meta.dirname, "../src/server.ts"),
        ],
        cwd: state,
        env: {
          ...(process.env as Record<string, string>),
          AGENT_FABRIC_STATE_DIRECTORY: state,
          AGENT_FABRIC_LABEL: "surface",
          FABRIC_LEGACY_TOOLS: legacy ? "1" : "0",
        },
        stderr: "pipe",
      }),
    );
    const result = await client.listTools();
    expect(result.tools.map((t) => t.name).sort()).toEqual(
      [
        "acknowledge",
        "activity",
        "adapters",
        "cancel",
        "dispatch",
        "inbox",
        "note",
        "output",
        "send",
        "status",
        "task",
        "whoami",
        ...(legacy ? ["batch", "team_create", "task_create", "task_claim", "task_update", "tasks"] : []),
      ]
        .map((n) => "fabric_" + n)
        .sort(),
    );
    if (!legacy) expect(JSON.stringify(result).length).toBeLessThanOrEqual(9077 * 0.65);
    const invalid = await client.callTool({
      name: "fabric_inbox",
      arguments: { ids: Array.from({ length: 101 }, (_, i) => String(i)) },
    });
    expect(invalid.isError).toBe(true);
    const errorText = (invalid.content as Array<{ text: string }>)[0]!.text;
    expect(errorText).toContain("fix:");
    expect(errorText).not.toContain("\n");
  } finally {
    await client.close();
    rmSync(state, { recursive: true, force: true });
  }
});

it("runs the linked-worktree MCP flow with fixture owners only", async () => {
  const { mkdirSync, writeFileSync, chmodSync, copyFileSync, readFileSync, existsSync } = await import("node:fs");
  const { execFileSync } = await import("node:child_process");
  const root = mkdtempSync(join(tmpdir(), "fabric-v2-"));
  const primary = join(root, "primary"),
    linked = join(root, "linked"),
    product = join(root, "product");
  mkdirSync(primary);
  const git = (...args: string[]) => execFileSync("git", args, { cwd: primary, stdio: "pipe" });
  git("init", "-q");
  git(
    "-c",
    "user.name=fixture",
    "-c",
    "user.email=fixture@example.invalid",
    "commit",
    "--allow-empty",
    "-qm",
    "fixture",
  );
  git("worktree", "add", "-q", "--detach", linked);
  const owners = join(product, "skills/orchestrate/scripts");
  mkdirSync(owners, { recursive: true });
  mkdirSync(join(product, "scripts/lib"), { recursive: true });
  copyFileSync(
    resolve(import.meta.dirname, "../../../scripts/lib/harness-python.sh"),
    join(product, "scripts/lib/harness-python.sh"),
  );
  for (const name of ["run_dir_init.sh", "dispatch_run.py", "batch_run.py", "run_controls.py"]) {
    const path = join(owners, name),
      fixture = join(import.meta.dirname, "v2-owner-fixture.mjs");
    writeFileSync(
      path,
      name.endsWith(".sh")
        ? `#!/bin/sh\nPROVENANT_FIXTURE_OWNER=${name} exec '${process.execPath}' '${fixture}' "$@"\n`
        : `#!/usr/bin/env python3\nimport os,sys\nos.environ['PROVENANT_FIXTURE_OWNER']=${JSON.stringify(name)}\nos.execv(${JSON.stringify(process.execPath)},[${JSON.stringify(process.execPath)},${JSON.stringify(fixture)},*sys.argv[1:]])\n`,
    );
    chmodSync(path, 0o755);
  }
  const client = new Client({ name: "v2", version: "1" });
  const call = async (name: string, args: Record<string, unknown> = {}) => {
    const result = await client.callTool({ name: "fabric_" + name, arguments: args });
    expect(result.isError, JSON.stringify(result)).not.toBe(true);
    return result;
  };
  try {
    await client.connect(
      new StdioClientTransport({
        command: process.execPath,
        args: [
          "--import",
          createRequire(import.meta.url).resolve("tsx"),
          resolve(import.meta.dirname, "../src/server.ts"),
        ],
        cwd: linked,
        env: {
          ...(process.env as Record<string, string>),
          AGENT_FABRIC_STATE_DIRECTORY: join(root, "state"),
          AGENT_FABRIC_LABEL: "chair-seat",
          AGENT_FABRIC_SEAT: "codex",
          AGENT_FABRIC_PRODUCT_ROOT: product,
          HARNESS_PYTHON: execFileSync("python3", ["-c", "import sys;print(sys.executable)"], {
            encoding: "utf8",
          }).trim(),
        },
        stderr: "pipe",
      }),
    );
    const batch = await call("dispatch", {
      tasks: [
        { id: "one", prompt: "first" },
        { id: "two", prompt: "second" },
      ],
      wait_seconds: 5,
    });
    expect((batch.structuredContent as any).runs).toHaveLength(2);
    expect((batch.content as any[])[0].text).toMatch(/^batch mcp-.* 2 tasks: 2 ok/u);
    await call("status", { ids: [(batch.structuredContent as any).runs[0].run_id] });
    const nested = join(linked, "nested");
    mkdirSync(nested);
    const first = await call("dispatch", {
      prompt: "question",
      cwd: nested,
      timeout_seconds: 1234,
      wait_seconds: 5,
      sandbox: "read-only",
      network: false,
      effort: "high",
    });
    const row = first.structuredContent as Record<string, any>;
    expect(row, JSON.stringify(row)).toMatchObject({
      schema: "fabric.status.v1",
      status: "input_required",
      applied: { network: false },
      cwd: expect.stringContaining("/nested"),
    });
    expect((first.content as any[])[0].text).toBe(row.digest);
    expect(row.digest.length / 4).toBeLessThan(120);
    expect(row.run_dir).toContain("/primary/.agent-run/runs/");
    const notice = await call("inbox");
    expect((notice.structuredContent as any).messages.some((m: any) => m.kind === "run_terminal")).toBe(true);
    const status = await call("status", { ids: [row.run_id] });
    expect((status.structuredContent as any).runs[0].status).toBe("input_required");
    const resumed = await call("dispatch", { resume: row.run_id, prompt: "main", wait_seconds: 5 });
    expect(resumed.structuredContent).toMatchObject({
      status: "ok",
      attempt: 2,
      run_id: row.run_id,
      evidence: { timeout: 1234 },
    });
    const pending = await call("dispatch", { resume: row.run_id, prompt: "pause-before-attempt", wait_seconds: 0 });
    expect(pending.structuredContent).toMatchObject({ state: "queued", attempt: 3 });
    const duplicate = await call("dispatch", { resume: row.run_id, prompt: "duplicate", wait_seconds: 0 });
    expect(duplicate.structuredContent).toMatchObject({ status: "rejected" });
    await call("status", { ids: [row.run_id], wait_seconds: 5 });
    const interrupted = await call("dispatch", { resume: row.run_id, prompt: "crash-before-attempt", wait_seconds: 5 });
    expect(interrupted.structuredContent).toMatchObject({
      state: "terminal",
      status: "interrupted",
      attempt: 4,
      attempt_count: 3,
    });
    const retried = await call("dispatch", { resume: row.run_id, prompt: "retry", wait_seconds: 5 });
    expect(retried.structuredContent).toMatchObject({ state: "terminal", status: "ok", attempt: 4 });
    const output = await call("output", { id: row.run_id, max_bytes: 100 });
    expect(output.structuredContent).toMatchObject({ next_offset: 100, eof: false });
    expect((output.content as any[])[0].text).toHaveLength(100);
    await call("send", { to: "chair", body: "hello".repeat(1000), kind: "question" });
    const peek = await call("inbox");
    const messages = (peek.structuredContent as any).messages;
    const message = messages.find((m: any) => m.kind === "question");
    expect(message.preview).toHaveLength(80);
    expect(message.body).toBeUndefined();
    const claimed = await call("inbox", { ids: [message.id] });
    const body = (claimed.structuredContent as any).messages[0];
    expect(Buffer.byteLength(body.body)).toBeLessThanOrEqual(4096);
    expect(existsSync(body.body_path)).toBe(true);
    expect(body.claimId).toBeTruthy();
    await call("acknowledge", { message_id: body.messageId, claim_id: body.claimId });
    await call("status", { ids: [row.run_id] });
    const inbox = await call("inbox");
    expect((inbox.structuredContent as any).messages).toEqual([]);
    const slow = await call("dispatch", { prompt: "slow", wait_seconds: 0 });
    const active = slow.structuredContent as any;
    const cancelled = await call("cancel", { id: active.id });
    expect((cancelled.structuredContent as any).runs[0].status).toBe("cancelled");
    expect(readFileSync(join(primary, ".git/info/exclude"), "utf8")).toContain("/.agent-run/");
  } finally {
    await client.close();
    rmSync(root, { recursive: true, force: true });
  }
}, 30000);

it("reports stale running server sources with a restart fix", async () => {
  const { serverBuild } = await import("../src/surface.js");
  const { mkdirSync, writeFileSync } = await import("node:fs");
  const root = mkdtempSync(join(tmpdir(), "fabric-build-"));
  try {
    mkdirSync(join(root, "src"));
    writeFileSync(join(root, "package.json"), JSON.stringify({ version: "test" }));
    writeFileSync(join(root, "src/server.ts"), "changed");
    expect(serverBuild(root, 0)).toMatchObject({
      server_version: "test",
      build_stale: true,
      fix: expect.stringContaining("Restart"),
    });
    expect(serverBuild(root, Date.now() + 1000)).toEqual({ server_version: "test", build_stale: false });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

it('keeps terminal status readable when the notice database cannot open', async () => {
 const {mkdirSync,writeFileSync,readFileSync}=await import('node:fs');
 const root=mkdtempSync(join(tmpdir(),'fabric-status-store-')),state=join(root,'not-a-directory');
 const path=join(root,'.agent-run/runs/20260923-1012-dispatch-fixture-a81f3c/tasks/task-1/attempt-001');mkdirSync(path,{recursive:true});
 writeFileSync(join(path,'attempt.json'),readFileSync(join(import.meta.dirname,'fixtures/attempt.json')));writeFileSync(state,'file');
 const client=new Client({name:'status-store',version:'1'});
 try {
  await client.connect(new StdioClientTransport({command:process.execPath,args:['--import',createRequire(import.meta.url).resolve('tsx'),resolve(import.meta.dirname,'../src/server.ts')],cwd:root,env:{...process.env as Record<string,string>,AGENT_FABRIC_STATE_DIRECTORY:state},stderr:'pipe'}));
  const result=await client.callTool({name:'fabric_status',arguments:{ids:['mcp-a81f3c']}});
  expect(result.isError).not.toBe(true);expect((result.structuredContent as any).runs[0].status).toBe('ok');
 } finally {await client.close();rmSync(root,{recursive:true,force:true});}
});
