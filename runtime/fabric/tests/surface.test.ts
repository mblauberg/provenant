import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { createRequire } from "node:module";
import { expect, it } from "vitest";

it("keeps legacy route and result path in the brief digest", async () => {
  const { digest, runView } = await import("../src/surface.js");
  const brief = runView({
    status: "ok",
    run_id: "mcp-wave1",
    adapter: "codex",
    model: "gpt-6-sol",
    result_path: ".agent-run/old/result.md",
  });
  expect(digest(brief)).toContain("codex/gpt-6-sol");
  expect(digest(brief)).toContain("result .agent-run/old/result.md");
});

it("reads adapter cooldowns from the configured state root and explicit override", async () => {
  const { adapterView } = await import("../src/surface.js");
  const root = mkdtempSync(join(tmpdir(), "fabric-cooldowns-"));
  const oldRoot = process.env.AGENT_FABRIC_STATE_ROOT;
  const oldPath = process.env.FABRIC_COOLDOWNS_PATH;
  const snapshot = { adapters: [{ name: "codex", models: ["gpt-6-sol"], aliases: { workhorse: ["gpt-6-sol"] } }], endpoints: {} } as any;
  try {
    writeFileSync(join(root, "cooldowns.json"), JSON.stringify({ cooldowns: { one: {
      adapter: "codex", cooling_until: "2999-01-01T00:00:00Z",
    } } }));
    process.env.AGENT_FABRIC_STATE_ROOT = root;
    delete process.env.FABRIC_COOLDOWNS_PATH;
    expect(adapterView(snapshot).digest).toContain("cooling until 2999-01-01");
    const override = join(root, "override.json");
    writeFileSync(override, JSON.stringify({ cooldowns: { two: {
      adapter: "codex", cooling_until: "2998-01-01T00:00:00Z",
    } } }));
    process.env.FABRIC_COOLDOWNS_PATH = override;
    expect(adapterView(snapshot).digest).toContain("cooling until 2998-01-01");
  } finally {
    if (oldRoot === undefined) delete process.env.AGENT_FABRIC_STATE_ROOT;
    else process.env.AGENT_FABRIC_STATE_ROOT = oldRoot;
    if (oldPath === undefined) delete process.env.FABRIC_COOLDOWNS_PATH;
    else process.env.FABRIC_COOLDOWNS_PATH = oldPath;
    rmSync(root, { recursive: true, force: true });
  }
});

it("shows the requested route and pending result before the first attempt", async () => {
  const { digest, runView } = await import("../src/surface.js");
  const brief = runView({
    status: "queued",
    state: "queued",
    run_id: "mcp-pending",
    provenance: { requested: { adapter: "claude", model: "opus" } },
  });
  expect(digest(brief)).toContain("claude/opus");
  expect(digest(brief)).toContain("result pending");
});

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
  const { mkdirSync, writeFileSync, chmodSync, copyFileSync, readFileSync, existsSync, realpathSync } = await import("node:fs");
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
  const projectRoot = realpathSync(primary), linkedRoot = realpathSync(linked);
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
  const peer = new Client({ name: "v2-peer", version: "1" });
  const call = async (name: string, args: Record<string, unknown> = {}) => {
    const result = await client.callTool({ name: "fabric_" + name, arguments: args });
    expect(result.isError, JSON.stringify(result)).not.toBe(true);
    return result;
  };
  try {
    await client.connect(
      new StdioClientTransport({
        command: resolve(import.meta.dirname, "../bin/fabric-mcp"),
        args: [],
        cwd: linked,
        env: {
          ...(process.env as Record<string, string>),
          FABRIC_NODE: process.execPath,
          AGENT_FABRIC_TSX_LOADER: createRequire(import.meta.url).resolve("tsx"),
          GIT_DIR: join(root, "bogus-git-dir"),
          GIT_WORK_TREE: primary,
          AGENT_FABRIC_STATE_DIRECTORY: join(root, "state"),
          AGENT_FABRIC_LABEL: "chair-seat",
          PROVENANT_CHAIR: "chair-seat",
          AGENT_FABRIC_SEAT: "codex",
          AGENT_FABRIC_PRODUCT_ROOT: product,
          HARNESS_PYTHON: execFileSync("python3", ["-c", "import sys;print(sys.executable)"], {
            encoding: "utf8",
          }).trim(),
        },
        stderr: "pipe",
      }),
    );
    const self = await call("whoami");
    expect((self.structuredContent as any)).toMatchObject({ project: projectRoot, cwd: linkedRoot, agentId: "chair-seat" });
    await peer.connect(
      new StdioClientTransport({
        command: resolve(import.meta.dirname, "../bin/fabric-mcp"),
        args: [],
        cwd: primary,
        env: {
          ...(process.env as Record<string, string>),
          FABRIC_NODE: process.execPath,
          AGENT_FABRIC_TSX_LOADER: createRequire(import.meta.url).resolve("tsx"),
          GIT_DIR: join(root, "bogus-git-dir"),
          GIT_WORK_TREE: linked,
          AGENT_FABRIC_STATE_DIRECTORY: join(root, "state"),
          AGENT_FABRIC_LABEL: "worker-seat",
          AGENT_FABRIC_SEAT: "claude",
          PROVENANT_CHAIR: "chair-seat",
          AGENT_FABRIC_PRODUCT_ROOT: product,
          HARNESS_PYTHON: execFileSync("python3", ["-c", "import sys;print(sys.executable)"], {
            encoding: "utf8",
          }).trim(),
        },
        stderr: "pipe",
      }),
    );
    const peerCall = async (name: string, args: Record<string, unknown> = {}) => {
      const result = await peer.callTool({ name: "fabric_" + name, arguments: args });
      expect(result.isError, JSON.stringify(result)).not.toBe(true);
      return result;
    };
    expect((await peerCall("whoami")).structuredContent).toMatchObject({
      project: projectRoot, cwd: projectRoot, agentId: "worker-seat",
    });
    const workerRun = await peerCall("dispatch", { prompt: "worker context", wait_seconds: 5 });
    const workerRow = workerRun.structuredContent as any;
    expect(workerRow, JSON.stringify(workerRow)).toHaveProperty("run_dir");
    expect(JSON.parse(readFileSync(join(workerRow.run_dir, "_owner", `${workerRow.task_id}-env-1.json`), "utf8")))
      .toMatchObject({ chair: "chair-seat" });
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
    const batchRow = (batch.structuredContent as any).runs[0];
    const unnamed = await call("dispatch", { resume: batchRow.run_id, prompt: "again", wait_seconds: 5 });
    expect(unnamed.structuredContent).toMatchObject({ status: "rejected", error: "resume_task_required" });
    const resumedTask = await call("dispatch", {
      resume: batchRow.run_id, task_id: "two", prompt: "again", context_ceiling: 250000, wait_seconds: 5,
    });
    expect(resumedTask.structuredContent).toMatchObject({ status: "ok", run_id: batchRow.run_id, task_id: "two", attempt: 2 });
    expect(JSON.parse(readFileSync(join(batchRow.run_dir, "_owner", "two-args-2.json"), "utf8")))
      .toEqual(expect.arrayContaining(["--task-id", "two", "--context-ceiling", "250000"]));
    const byOwnId = await call("dispatch", { resume: "one", prompt: "again", wait_seconds: 5 });
    expect(byOwnId.structuredContent).toMatchObject({ status: "ok", run_id: batchRow.run_id, task_id: "one", attempt: 2 });
    const batchStatus = (await call("status", { ids: [batchRow.run_id] })).structuredContent as any;
    expect(batchStatus.runs.map((row: any) => [row.task_id, row.attempt, row.batch_id]).sort())
      .toEqual([["one", 2, batchRow.batch_id], ["two", 2, batchRow.batch_id]]);
    const handed = (await call("dispatch", {
      handoff: batchRow.run_id, task_id: "two", prompt: "carry on", context_ceiling: 150000, wait_seconds: 5,
    })).structuredContent as any;
    expect(handed).toMatchObject({ status: "ok", attempt: 1 });
    expect(handed.run_id).not.toBe(batchRow.run_id);
    const handedArgs = JSON.parse(readFileSync(join(handed.run_dir, "_owner", `${handed.task_id}-args-1.json`), "utf8"));
    expect(handedArgs).toEqual(expect.arrayContaining(["--adapter", "codex", "--context-ceiling", "150000"]));
    const brief = readFileSync(join(handed.run_dir, "_owner", `${handed.task_id}-prompt-1.md`), "utf8");
    expect(brief.startsWith(`Fresh session handed off from Fabric run ${batchRow.run_id} task two (codex/fixture@high`)).toBe(true);
    expect(brief.endsWith("x".repeat(100) + "\n>>>\n\ncarry on")).toBe(true);
    expect(brief.length).toBeLessThan(8400);
    expect((await call("dispatch", { handoff: batchRow.run_id, prompt: "x", wait_seconds: 0 })).structuredContent)
      .toMatchObject({ status: "rejected", error: "handoff_task_required" });
    mkdirSync(join(linked, "nested-batch"));
    writeFileSync(join(linked, "batch.md"), "first");
    const routed = await call("dispatch", {
      adapter: "claude", alias: "workhorse", model: "unknown-model", effort: "high", timeout_seconds: 4321,
      tasks: [{ id: "default", prompt_file: "batch.md", cwd: "nested-batch" },
        { id: "override", prompt: "second", adapter: "codex", model: "sol", timeout_seconds: 123 }],
      wait_seconds: 5, detail: "full",
    });
    const routedRows = (routed.structuredContent as any).runs?.sort((a: any, b: any) => a.task_id.localeCompare(b.task_id));
    expect(routedRows?.[0].provenance.requested).toMatchObject({ adapter: "claude", model: "unknown-model", effort: "high" });
    expect(routedRows?.[0].notes).toContain("alias and model both supplied; model won");
    expect(routedRows?.[0].evidence.timeout).toBe(4321);
    expect(routedRows?.[0].cwd).toContain("/nested-batch");
    expect(routedRows?.[1].provenance.requested).toMatchObject({ adapter: "codex", model: "sol" });
    expect(routedRows?.[1].evidence.timeout).toBe(123);
    const writer = await call("dispatch", {
      prompt: "writer", mode: "worktree_write", worktree: linked, wait_seconds: 5,
    });
    const writerRow = writer.structuredContent as any;
    expect(writerRow.status).toBe("ok");
    const writerArgs = JSON.parse(
      readFileSync(join(writerRow.run_dir, "_owner", `${writerRow.task_id}-args-1.json`), "utf8"),
    ) as string[];
    expect(writerArgs.slice(writerArgs.indexOf("--access-mode"), writerArgs.indexOf("--access-mode") + 2)).toEqual([
      "--access-mode", "worktree_write",
    ]);
    expect(writerArgs.slice(writerArgs.indexOf("--worktree"), writerArgs.indexOf("--worktree") + 2)).toEqual([
      "--worktree", linked,
    ]);
    expect(JSON.parse(readFileSync(join(writerRow.run_dir, "_owner", `${writerRow.task_id}-env-1.json`), "utf8")))
      .toMatchObject({ chair: "chair-seat" });
    const resumedWriter = await call("dispatch", { resume: writerRow.run_id, prompt: "continue writer", wait_seconds: 5 });
    expect(resumedWriter.structuredContent).toMatchObject({ status: "ok", attempt: 2, run_id: writerRow.run_id });
    const nested = join(linked, "nested");
    mkdirSync(nested);
    writeFileSync(join(linked, "question.md"), "question");
    const first = await call("dispatch", {
      prompt_file: "question.md",
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
    expect((notice.structuredContent as any).messages.some((m: any) => m.kind === "run_terminal")).toBe(false);
    expect(row.attempts).toBeUndefined();
    expect(row.evidence).toBeUndefined();
    const full = await call("status", { ids: [row.run_id], detail: "full" });
    expect((full.structuredContent as any).runs[0].attempts).toHaveLength(1);
    expect((full.structuredContent as any).runs[0].evidence).toBeDefined();
    expect((full.structuredContent as any).runs[0].evidence.owner_cwd).toBe(linked.replace("/var/folders/", "/private/var/folders/"));
    const missing = await call("dispatch", { resume: row.run_id, prompt_file: "absent.md", wait_seconds: 5 });
    expect(missing.structuredContent).toMatchObject({ status: "rejected", error: "prompt_unavailable" });
    const status = await call("status", { ids: [row.run_id] });
    expect((status.structuredContent as any).runs[0].status).toBe("input_required");
    writeFileSync(join(row.run_dir, "dispatch-owner.json"), JSON.stringify({
      schema_version: 1, kind: "dispatch", run_dir: row.run_dir, workspace: linked,
      run_token: "recycled", owner_pid: process.pid, owner_pgid: process.pid,
      owner_started_at: "a different process", host_pid: process.pid, host_started_at: null,
      started_at: new Date().toISOString(), owner_stdout: "", owner_stderr: "",
    }));
    const resumed = await call("dispatch", { resume: row.run_id, prompt_file: "question.md", wait_seconds: 5, detail: "full" });
    expect(resumed.structuredContent).toMatchObject({
      status: "input_required",
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
    expect(JSON.parse(readFileSync(join(row.run_dir, "dispatch-status.json"), "utf8")).fix).toBeUndefined();
    const rejectedResume = await call("dispatch", {resume:row.run_id,prompt:"reject-before-attempt",wait_seconds:5});
    expect(rejectedResume.structuredContent).toMatchObject({state:"terminal",status:"rejected",fix:"dispatch a new run",attempt:5});
    expect((rejectedResume.structuredContent as any).attempts).toBeUndefined();
    expect(((await call("inbox")).structuredContent as any).messages).toEqual([]);
    await call("dispatch", {resume:row.run_id,prompt:"main",wait_seconds:5});
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
    const owned = await call("task", {
      action: "create", task_id: "owned-work", objective: "Reply with evidence", owner: "worker-seat",
    });
    expect(owned.structuredContent).toMatchObject({ taskId: "owned-work", owner: "worker-seat", state: "open" });
    expect(((await peerCall("task", { action: "list" })).structuredContent as any).tasks)
      .toContainEqual(expect.objectContaining({ taskId: "owned-work", owner: "worker-seat" }));
    const claimByChair = await client.callTool({ name: "fabric_task", arguments: { action: "claim", task_id: "owned-work" } });
    expect(claimByChair.isError).toBe(true);
    const sent = await call("send", {
      to: "worker-seat", body: "Please reply", kind: "question", task_id: "owned-work",
    });
    const parentId = (sent.structuredContent as any).messageId as string;
    expect(((await peerCall("inbox", { task_id: "owned-work" })).structuredContent as any).messages)
      .toContainEqual(expect.objectContaining({ id: parentId, from: "chair-seat" }));
    const question = ((await peerCall("inbox", { ids: [parentId] })).structuredContent as any).messages[0];
    expect(question).toMatchObject({ messageId: parentId, taskId: "owned-work", conversationId: parentId });
    await peerCall("acknowledge", { message_id: question.messageId, claim_id: question.claimId });
    const answer = await peerCall("send", {
      to: "chair-seat", body: "Evidence recorded", kind: "answer", reply_to: parentId, task_id: "owned-work",
    });
    const answerId = (answer.structuredContent as any).messageId as string;
    const reply = ((await call("inbox", { ids: [answerId] })).structuredContent as any).messages[0];
    expect(reply).toMatchObject({
      messageId: answerId, from: "worker-seat", taskId: "owned-work", replyTo: parentId, conversationId: parentId,
    });
    await call("acknowledge", { message_id: answerId, claim_id: reply.claimId });
    await peerCall("task", { action: "update", task_id: "owned-work", state: "done" });
    expect(((await call("task", { action: "list", state: "done" })).structuredContent as any).tasks)
      .toContainEqual(expect.objectContaining({ taskId: "owned-work", owner: "worker-seat", state: "done" }));
    await call("status", { ids: [row.run_id] });
    const inbox = await call("inbox");
    expect((inbox.structuredContent as any).messages).toEqual([]);
    const noControls = await call("dispatch", { prompt: "no-controls", wait_seconds: 5 });
    const resumedNoControls = await call("dispatch", { resume: (noControls.structuredContent as any).run_id, prompt: "main", wait_seconds: 5 });
    expect(resumedNoControls.structuredContent).toMatchObject({status:"ok",attempt:2});
    const slow = await call("dispatch", { prompt: "slow", wait_seconds: 0 });
    const active = slow.structuredContent as any;
    const cancelled = await call("cancel", { id: active.id });
    expect((cancelled.structuredContent as any).runs[0].status).toBe("cancelled");
    expect((cancelled.structuredContent as any).runs[0].attempts).toBeUndefined();
    // A cancel that has to SIGKILL an owner before it writes a result stays a cancel.
    const stubborn = (await call("dispatch", { prompt: "stubborn", wait_seconds: 0 })).structuredContent as any;
    await call("cancel", { id: stubborn.id });
    const closed = await call("status", { ids: [stubborn.id], wait_seconds: 10 });
    expect((closed.structuredContent as any).status ?? (closed.structuredContent as any).runs?.[0]?.status).toBe("cancelled");
    expect(((await call("inbox")).structuredContent as any).messages).toEqual([]);
    expect(readFileSync(join(primary, ".git/info/exclude"), "utf8")).toContain("/.agent-run/");
  } finally {
    await peer.close();
    await client.close();
    rmSync(root, { recursive: true, force: true });
  }
}, 45000);

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
