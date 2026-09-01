// Assert the real MCP stdio contract through the same launcher clients register.
import assert from "node:assert/strict";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { chmodSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

const state = resolve(tmpdir(), `fabric-mcp-smoke-${String(process.pid)}`);
const executionRoot = resolve(tmpdir(), `fabric-mcp-execution-${String(process.pid)}`);
rmSync(state, { recursive: true, force: true });
rmSync(executionRoot, { recursive: true, force: true });

// Override this with the managed `provenant` shim to verify installed routing.
const command = process.env.AGENT_FABRIC_MCP_COMMAND ?? resolve(import.meta.dirname, "bin/fabric-mcp");
const tsxLoader = process.env.AGENT_FABRIC_TSX_LOADER ??
  createRequire(import.meta.url).resolve("tsx");
const clients = [];

const spawnAgent = async (seat, clientLabel, options = {}) => {
  const transport = new StdioClientTransport({
    command,
    cwd: options.cwd ?? process.cwd(),
    env: {
      HOME: process.env.HOME,
      PATH: process.env.AGENT_FABRIC_MCP_PATH ?? "/usr/bin:/bin",
      FABRIC_NODE: process.env.FABRIC_NODE ?? process.execPath,
      AGENT_FABRIC_SEAT: seat,
      AGENT_FABRIC_LABEL: clientLabel,
      AGENT_FABRIC_CLIENT_LABEL: clientLabel,
      AGENT_FABRIC_STATE_DIRECTORY: state,
      ...(process.env.AGENT_FABRIC_PRODUCT_ROOT === undefined ? {} : {
        AGENT_FABRIC_PRODUCT_ROOT: process.env.AGENT_FABRIC_PRODUCT_ROOT,
      }),
      ...(process.env.AGENT_FABRIC_INSTANCE_ROOT === undefined ? {} : {
        AGENT_FABRIC_INSTANCE_ROOT: process.env.AGENT_FABRIC_INSTANCE_ROOT,
      }),
      AGENT_FABRIC_TSX_LOADER: tsxLoader,
      ...(options.productRoot === undefined ? {} : {
        AGENT_FABRIC_PRODUCT_ROOT: options.productRoot,
      }),
    },
  });
  const client = new Client({ name: `test-${clientLabel}`, version: "1" });
  await client.connect(transport);
  clients.push(client);
  return client;
};

const payload = (result) => {
  assert.equal(result.isError, undefined, JSON.stringify(result));
  assert.equal(result.content[0]?.type, "text");
  return JSON.parse(result.content[0].text);
};

const expectToolError = async (promise) => {
  const result = await promise;
  assert.equal(result.isError, true, JSON.stringify(result));
};

const shellQuote = (value) => `'${value.replaceAll("'", `'\"'\"'`)}'`;

try {
  const claude = await spawnAgent("claude", "claude-client");
  const codex = await spawnAgent("codex", "codex-client");
  const agy = await spawnAgent("agy", "agy-client");

  assert.match(claude.getInstructions() ?? "", /project-scoped mailbox/);

  const listed = await claude.listTools();
  assert.deepEqual(listed.tools.map((tool) => tool.name).sort(), [
    "fabric_acknowledge",
    "fabric_activity",
    "fabric_batch",
    "fabric_dispatch",
    "fabric_inbox",
    "fabric_note",
    "fabric_send",
    "fabric_task_claim",
    "fabric_task_create",
    "fabric_task_update",
    "fabric_tasks",
    "fabric_team_create",
    "fabric_whoami",
  ]);
  assert.match(claude.getInstructions() ?? "", /configured-provider work/);
  const taskCreateTool = listed.tools.find((tool) => tool.name === "fabric_task_create");
  const taskClaimTool = listed.tools.find((tool) => tool.name === "fabric_task_claim");
  assert.match(taskCreateTool?.description ?? "", /owner-bound task is already assigned/);
  assert.match(taskClaimTool?.description ?? "", /Owner-bound tasks are already assigned/);

  for (const [client, seat, label] of [
    [claude, "claude", "claude-client"],
    [codex, "codex", "codex-client"],
    [agy, "agy", "agy-client"],
  ]) {
    const identity = payload(await client.callTool({ name: "fabric_whoami", arguments: {} }));
    assert.equal(identity.provider, seat);
    assert.equal(identity.agentId, label);
  }

  const sent = payload(await claude.callTool({
    name: "fabric_send",
    arguments: { to: "codex-client", body: "Does empty input fail?", kind: "request" },
  }));
  assert.deepEqual(sent.recipients, ["codex-client"]);

  const peeked = payload(await codex.callTool({
    name: "fabric_inbox",
    arguments: { peek: true },
  }));
  assert.equal(peeked[0].messageId, sent.messageId);
  assert.equal(peeked[0].claimId, null);

  const inbox = payload(await codex.callTool({ name: "fabric_inbox", arguments: {} }));
  assert.equal(inbox[0].kind, "request");
  assert.equal(inbox[0].conversationId, sent.messageId);
  assert.equal(typeof inbox[0].claimId, "string");
  assert.deepEqual(payload(await codex.callTool({ name: "fabric_inbox", arguments: {} })), []);

  // Persist and send the correlated response before acknowledging the request.
  const replySent = payload(await codex.callTool({
    name: "fabric_send",
    arguments: {
      to: "claude-client",
      body: "Yes, it fails.",
      kind: "response",
      reply_to: inbox[0].messageId,
    },
  }));
  assert.equal(payload(await codex.callTool({
    name: "fabric_acknowledge",
    arguments: { message_id: inbox[0].messageId, claim_id: inbox[0].claimId },
  })).alreadyAcknowledged, false);

  const reply = payload(await claude.callTool({ name: "fabric_inbox", arguments: {} }))[0];
  assert.equal(reply.messageId, replySent.messageId);
  assert.equal(reply.replyTo, sent.messageId);
  assert.equal(reply.conversationId, sent.messageId);
  payload(await claude.callTool({
    name: "fabric_acknowledge",
    arguments: { message_id: reply.messageId, claim_id: reply.claimId },
  }));

  const task = payload(await claude.callTool({
    name: "fabric_task_create",
    arguments: { task_id: "mcp-claim", objective: "Claim once" },
  }));
  assert.equal(task.owner, null);
  assert.equal(payload(await agy.callTool({
    name: "fabric_task_claim",
    arguments: { task_id: task.taskId },
  })).owner, "agy-client");
  await expectToolError(codex.callTool({
    name: "fabric_task_claim",
    arguments: { task_id: task.taskId },
  }));

  const targetedTask = payload(await claude.callTool({
    name: "fabric_task_create",
    arguments: { task_id: "mcp-targeted", objective: "Review once", owner: "agy-client" },
  }));
  assert.equal(targetedTask.owner, "agy-client");
  await expectToolError(codex.callTool({
    name: "fabric_task_claim",
    arguments: { task_id: targetedTask.taskId },
  }));

  payload(await claude.callTool({
    name: "fabric_team_create",
    arguments: { team_id: "reviewers", members: ["claude-client", "codex-client", "agy-client"] },
  }));
  assert.deepEqual(payload(await claude.callTool({
    name: "fabric_team_create",
    arguments: { team_id: "reviewers", members: ["claude-client", "agy-client"] },
  })).members, ["claude-client", "agy-client"]);
  assert.deepEqual(payload(await claude.callTool({
    name: "fabric_send",
    arguments: { to: "reviewers", body: "Only current members" },
  })).recipients, ["agy-client"]);

  const activity = payload(await claude.callTool({
    name: "fabric_activity",
    arguments: { after_seq: 0, limit: 200 },
  }));
  assert.ok(activity.length > 0);
  assert.ok(activity.every((entry, index) => index === 0 || entry.seq > activity[index - 1].seq));

  await expectToolError(claude.callTool({
    name: "fabric_send",
    arguments: { to: "missing-agent", body: "must fail" },
  }));

  const workspace = resolve(executionRoot, "workspace");
  const fakeProduct = resolve(executionRoot, "product");
  const ownerDirectory = resolve(fakeProduct, "skills/orchestrate/scripts");
  mkdirSync(workspace, { recursive: true });
  mkdirSync(ownerDirectory, { recursive: true });
  const fixtureOwner = resolve(import.meta.dirname, "tests/execution-owner-fixture.mjs");
  for (const name of ["run_dir_init.sh", "dispatch_run.py", "batch_run.py"]) {
    const owner = resolve(ownerDirectory, name);
    writeFileSync(owner,
      `#!/bin/sh\nPROVENANT_FIXTURE_OWNER=${shellQuote(name)} exec ${shellQuote(process.execPath)} ${shellQuote(fixtureOwner)} "$@"\n`,
    );
    chmodSync(owner, 0o755);
  }
  const executor = await spawnAgent("codex", "execution-client", {
    cwd: workspace,
    productRoot: fakeProduct,
  });
  const dispatched = payload(await executor.callTool({
    name: "fabric_dispatch",
    arguments: {
      prompt: "inspect the fixture",
      model: "gpt-fixture",
      wait_seconds: 5,
    },
  }));
  assert.equal(dispatched.status, "succeeded");
  assert.equal(dispatched.route.adapter, "codex");
  assert.equal(dispatched.route.resolved_model, "gpt-fixture");
  assert.match(readFileSync(dispatched.paths.result, "utf8"), /fixture result for: inspect the fixture/);
  assert.ok(dispatched.paths.run_dir.startsWith(resolve(realpathSync(workspace), ".agent-run")));
  assert.doesNotMatch(JSON.stringify(dispatched), /fixture result for: inspect the fixture/);
  const invocation = JSON.parse(readFileSync(
    resolve(dispatched.paths.run_dir, "dispatch_run.py.invocation.json"), "utf8",
  ));
  assert.equal(invocation.cwd, realpathSync(workspace));
  assert.ok(invocation.argv.includes("gpt-fixture"));

  const batched = payload(await executor.callTool({
    name: "fabric_batch",
    arguments: {
      concurrency: 2,
      wait_seconds: 5,
      tasks: [
        { id: "luna-one", prompt: "first", adapter: "codex", model: "gpt-5.6-luna" },
        { id: "luna-two", prompt: "second", adapter: "codex", model: "gpt-5.6-luna" },
      ],
    },
  }));
  assert.equal(batched.status, "completed");
  assert.equal(batched.task_count, 2);
  assert.equal(batched.concurrency, 2);
  assert.deepEqual(batched.counts, { succeeded: 2 });
  assert.deepEqual(batched.tasks.map((task) => task.route.resolved_model), [
    "gpt-5.6-luna",
    "gpt-5.6-luna",
  ]);
  assert.ok(batched.tasks.every((task) => task.message === undefined && task.stderr === undefined));
  assert.equal(JSON.parse(readFileSync(
    resolve(batched.paths.run_dir, "batch_run.py.invocation.json"), "utf8",
  )).cwd, realpathSync(workspace));

  const runCount = readdirSync(resolve(workspace, ".agent-run")).length;
  await expectToolError(executor.callTool({
    name: "fabric_batch",
    arguments: { tasks: [{ id: "invalid", adapter: "codex" }] },
  }));
  assert.equal(readdirSync(resolve(workspace, ".agent-run")).length, runCount);

  const running = payload(await executor.callTool({
    name: "fabric_dispatch",
    arguments: { prompt: "sleep until cancelled", adapter: "codex", wait_seconds: 0 },
  }));
  assert.equal(running.status, "running");
  assert.equal(running.route, null);
  assert.equal(running.route_status, "pending");
  const sleepingPid = resolve(running.paths.run_dir, "sleeping.pid");
  for (let attempts = 0; attempts < 100; attempts += 1) {
    try {
      readFileSync(sleepingPid);
      break;
    } catch {
      await new Promise((resolveWait) => setTimeout(resolveWait, 10));
    }
  }
  assert.match(readFileSync(sleepingPid, "utf8"), /^[0-9]+\n$/u);
  await executor.close();
  const cancelledMarker = resolve(running.paths.run_dir, "cancelled.marker");
  for (let attempts = 0; attempts < 100; attempts += 1) {
    try {
      readFileSync(cancelledMarker);
      break;
    } catch {
      await new Promise((resolveWait) => setTimeout(resolveWait, 10));
    }
  }
  assert.equal(readFileSync(cancelledMarker, "utf8"), "cancelled\n");
  await expectToolError(claude.callTool({
    name: "fabric_send",
    arguments: { to: "codex-client" },
  }));

  // Agy is a client seat. This smoke makes no claim about its selected model family.
  console.log("MCP contract assertions passed for claude, codex and agy client seats");
} finally {
  await Promise.allSettled(clients.map(async (client) => await client.close()));
  rmSync(state, { recursive: true, force: true });
  rmSync(executionRoot, { recursive: true, force: true });
}
