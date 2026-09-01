// Assert the real MCP stdio contract through the same launcher clients register.
import assert from "node:assert/strict";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { execFileSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
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
      ...(options.env ?? {}),
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

const waitForFile = async (path, timeoutMs = 2_000) => {
  const deadline = Date.now() + timeoutMs;
  while (!existsSync(path) && Date.now() < deadline) {
    await new Promise((resolveWait) => setTimeout(resolveWait, 10));
  }
  assert.ok(existsSync(path), `timed out waiting for ${path}`);
};

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
  const helperDirectory = resolve(fakeProduct, "scripts/lib");
  mkdirSync(workspace, { recursive: true });
  mkdirSync(ownerDirectory, { recursive: true });
  mkdirSync(helperDirectory, { recursive: true });
  writeFileSync(resolve(helperDirectory, "harness-python.sh"), readFileSync(
    resolve(import.meta.dirname, "../../scripts/lib/harness-python.sh"), "utf8",
  ));
  const fixtureOwner = resolve(import.meta.dirname, "tests/execution-owner-fixture.mjs");
  for (const name of ["run_dir_init.sh", "dispatch_run.py", "batch_run.py", "run_controls.py"]) {
    const owner = resolve(ownerDirectory, name);
    writeFileSync(owner, name === "run_dir_init.sh"
      ? `#!/bin/sh\nPROVENANT_FIXTURE_OWNER=${shellQuote(name)} exec ${shellQuote(process.execPath)} ${shellQuote(fixtureOwner)} "$@"\n`
      : `#!/usr/bin/env python3\nimport os, sys\nos.environ["PROVENANT_FIXTURE_OWNER"] = ${JSON.stringify(name)}\nos.execv(${JSON.stringify(process.execPath)}, [${JSON.stringify(process.execPath)}, ${JSON.stringify(fixtureOwner)}, *sys.argv[1:]])\n`,
    );
    chmodSync(owner, 0o755);
  }
  const fixturePython = execFileSync("/usr/bin/env", [
    "python3", "-c", "import sys; print(sys.executable)",
  ], { encoding: "utf8" }).trim();
  const executor = await spawnAgent("codex", "execution-client", {
    cwd: workspace,
    productRoot: fakeProduct,
    env: { HARNESS_PYTHON: fixturePython },
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

  // The Python selector may borrow the primary checkout's environment for a
  // linked worktree, but inherited Git redirects must not steer that lookup to
  // another repository.
  execFileSync("git", ["init", "--quiet"], { cwd: fakeProduct });
  execFileSync("git", ["add", "."], { cwd: fakeProduct });
  execFileSync("git", ["-c", "user.name=Fabric smoke", "-c", "user.email=fabric@example.invalid",
    "commit", "--quiet", "-m", "fixture"], { cwd: fakeProduct });
  const linkedProduct = resolve(executionRoot, "linked-product");
  execFileSync("git", ["worktree", "add", "--quiet", "--detach", linkedProduct, "HEAD"], { cwd: fakeProduct });
  const primaryPython = resolve(fakeProduct, ".venv/bin/python");
  mkdirSync(resolve(fakeProduct, ".venv/bin"), { recursive: true });
  writeFileSync(primaryPython, `#!/bin/sh\nexec ${shellQuote(fixturePython)} "$@"\n`);
  chmodSync(primaryPython, 0o755);
  const foreignProduct = resolve(executionRoot, "foreign-product");
  mkdirSync(foreignProduct);
  execFileSync("git", ["init", "--quiet"], { cwd: foreignProduct });
  const foreignMarker = resolve(executionRoot, "foreign-python-ran");
  const foreignPython = resolve(foreignProduct, ".venv/bin/python");
  mkdirSync(resolve(foreignProduct, ".venv/bin"), { recursive: true });
  writeFileSync(foreignPython,
    `#!/bin/sh\ntouch ${shellQuote(foreignMarker)}\nexec ${shellQuote(fixturePython)} "$@"\n`);
  chmodSync(foreignPython, 0o755);
  const redirectedExecutor = await spawnAgent("codex", "redirected-execution-client", {
    cwd: workspace,
    productRoot: linkedProduct,
    env: { GIT_DIR: resolve(foreignProduct, ".git"), GIT_WORK_TREE: foreignProduct },
  });
  const redirectedDispatch = payload(await redirectedExecutor.callTool({
    name: "fabric_dispatch",
    arguments: { prompt: "ignore foreign Git redirects", model: "gpt-fixture", wait_seconds: 5 },
  }));
  assert.equal(redirectedDispatch.status, "succeeded");
  assert.equal(existsSync(foreignMarker), false);

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
  await expectToolError(executor.callTool({
    name: "fabric_dispatch",
    arguments: { prompt: "invalid route", alias: "workhorse", model: "duplicate" },
  }));
  assert.equal(readdirSync(resolve(workspace, ".agent-run")).length, runCount);

  const malformed = payload(await executor.callTool({
    name: "fabric_dispatch",
    arguments: { prompt: "emit malformed owner output", adapter: "codex", wait_seconds: 5 },
  }));
  assert.equal(malformed.status, "owner_output_invalid");
  const typedRunning = payload(await executor.callTool({
    name: "fabric_dispatch",
    arguments: { prompt: "emit typed running", adapter: "codex", wait_seconds: 5 },
  }));
  assert.equal(typedRunning.status, "owner_output_invalid");
  const untypedRunning = payload(await executor.callTool({
    name: "fabric_dispatch",
    arguments: { prompt: "emit untyped running", adapter: "codex", wait_seconds: 5 },
  }));
  assert.equal(untypedRunning.status, "owner_output_invalid");
  const untypedCancelled = payload(await executor.callTool({
    name: "fabric_dispatch",
    arguments: { prompt: "emit untyped cancelled", adapter: "codex", wait_seconds: 5 },
  }));
  assert.equal(untypedCancelled.status, "owner_output_invalid");
  const untypedPreflight = payload(await executor.callTool({
    name: "fabric_dispatch",
    arguments: { prompt: "emit untyped preflight error", adapter: "codex", wait_seconds: 5 },
  }));
  assert.equal(untypedPreflight.status, "adapter_unavailable");
  assert.equal(untypedPreflight.message, "provider adapter is unavailable");
  const untypedSuccess = payload(await executor.callTool({
    name: "fabric_dispatch",
    arguments: { prompt: "emit untyped success", adapter: "codex", wait_seconds: 5 },
  }));
  assert.equal(untypedSuccess.status, "owner_output_invalid");
  const incompleteSuccess = payload(await executor.callTool({
    name: "fabric_dispatch",
    arguments: { prompt: "emit incomplete success", adapter: "codex", wait_seconds: 5 },
  }));
  assert.equal(incompleteSuccess.status, "owner_output_invalid");
  for (const prompt of ["emit nonexistent success", "emit incomplete route success"]) {
    const invalidSuccess = payload(await executor.callTool({
      name: "fabric_dispatch",
      arguments: { prompt, adapter: "codex", wait_seconds: 5 },
    }));
    assert.equal(invalidSuccess.status, "owner_output_invalid");
  }
  const incompleteBatch = payload(await executor.callTool({
    name: "fabric_batch",
    arguments: {
      wait_seconds: 5,
      tasks: [{ id: "incomplete", prompt: "emit incomplete completed batch", adapter: "codex" }],
    },
  }));
  assert.equal(incompleteBatch.status, "owner_output_invalid");
  const typedRunningBatch = payload(await executor.callTool({
    name: "fabric_batch",
    arguments: {
      wait_seconds: 5,
      tasks: [{ id: "running", prompt: "emit typed running batch", adapter: "codex" }],
    },
  }));
  assert.equal(typedRunningBatch.status, "owner_output_invalid");
  const untypedRunningBatch = payload(await executor.callTool({
    name: "fabric_batch",
    arguments: {
      wait_seconds: 5,
      tasks: [{ id: "running-untyped", prompt: "emit untyped running batch", adapter: "codex" }],
    },
  }));
  assert.equal(untypedRunningBatch.status, "owner_output_invalid");
  const untypedCancelledBatch = payload(await executor.callTool({
    name: "fabric_batch",
    arguments: {
      wait_seconds: 5,
      tasks: [{ id: "cancelled-untyped", prompt: "emit untyped cancelled batch", adapter: "codex" }],
    },
  }));
  assert.equal(untypedCancelledBatch.status, "owner_output_invalid");
  const untypedBatchPreflight = payload(await executor.callTool({
    name: "fabric_batch",
    arguments: {
      wait_seconds: 5,
      tasks: [{ id: "preflight", prompt: "emit untyped batch preflight error", adapter: "codex" }],
    },
  }));
  assert.equal(untypedBatchPreflight.status, "invalid_manifest");
  assert.equal(untypedBatchPreflight.message, "task manifest is invalid");
  const nonexistentBatch = payload(await executor.callTool({
    name: "fabric_batch",
    arguments: {
      wait_seconds: 5,
      tasks: [{ id: "missing", prompt: "emit nonexistent completed batch", adapter: "codex" }],
    },
  }));
  assert.equal(nonexistentBatch.status, "owner_output_invalid");
  const conflictingBatch = payload(await executor.callTool({
    name: "fabric_batch",
    arguments: {
      wait_seconds: 5,
      tasks: [{ id: "conflicting", prompt: "claim batch success then fail", adapter: "codex" }],
    },
  }));
  assert.equal(conflictingBatch.status, "owner_completion_conflict");
  assert.equal(conflictingBatch.owner_status, "completed");
  assert.equal(conflictingBatch.owner_exit, 7);
  const conflicting = payload(await executor.callTool({
    name: "fabric_dispatch",
    arguments: { prompt: "claim success then fail", adapter: "codex", wait_seconds: 5 },
  }));
  assert.equal(conflicting.status, "owner_completion_conflict");
  assert.equal(conflicting.owner_status, "succeeded");
  assert.equal(conflicting.owner_exit, 1);

  const running = payload(await executor.callTool({
    name: "fabric_dispatch",
    arguments: {
      prompt: "sleep until cancelled",
      task_id: "sleeping-dispatch",
      adapter: "codex",
      wait_seconds: 0,
    },
  }));
  assert.equal(running.status, "running");
  assert.equal(running.task_id, "sleeping-dispatch");
  assert.equal(running.route, null);
  assert.equal(running.route_status, "pending");
  assert.equal(running.paths.attempt, resolve(
    running.paths.run_dir, "dispatch/tasks/sleeping-dispatch/attempt-001/attempt.json",
  ));
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
  const cancelledMarker = resolve(running.paths.run_dir, "cancelled.marker");
  await executor.close();
  for (let attempts = 0; attempts < 100; attempts += 1) {
    try {
      readFileSync(cancelledMarker);
      break;
    } catch {
      await new Promise((resolveWait) => setTimeout(resolveWait, 10));
    }
  }
  assert.equal(readFileSync(cancelledMarker, "utf8"), "cancelled\n");

  const batchExecutor = await spawnAgent("codex", "batch-execution-client", {
    cwd: workspace,
    productRoot: fakeProduct,
    env: { HARNESS_PYTHON: fixturePython },
  });
  const runningBatch = payload(await batchExecutor.callTool({
    name: "fabric_batch",
    arguments: {
      wait_seconds: 0,
      tasks: [{ id: "sleeping-batch", prompt: "sleep until cancelled batch", adapter: "codex" }],
    },
  }));
  assert.equal(runningBatch.status, "running");
  assert.equal(runningBatch.batch_id, "batch-001");
  assert.equal(runningBatch.paths.summary, resolve(
    runningBatch.paths.run_dir, "dispatch/batches/batch-001/summary.json",
  ));
  const batchSleepingPid = resolve(runningBatch.paths.run_dir, "sleeping.pid");
  for (let attempts = 0; attempts < 100; attempts += 1) {
    try {
      readFileSync(batchSleepingPid);
      break;
    } catch {
      await new Promise((resolveWait) => setTimeout(resolveWait, 10));
    }
  }
  assert.match(readFileSync(batchSleepingPid, "utf8"), /^[0-9]+\n$/u);
  await batchExecutor.close();
  const batchCancelledMarker = resolve(runningBatch.paths.run_dir, "cancelled.marker");
  for (let attempts = 0; attempts < 100; attempts += 1) {
    try {
      readFileSync(batchCancelledMarker);
      break;
    } catch {
      await new Promise((resolveWait) => setTimeout(resolveWait, 10));
    }
  }
  assert.equal(readFileSync(batchCancelledMarker, "utf8"), "cancelled\n");

  const delayedExecutor = await spawnAgent("codex", "delayed-execution-client", {
    cwd: workspace,
    productRoot: fakeProduct,
    env: { HARNESS_PYTHON: fixturePython },
  });
  for (const [kind, taskId, expectedStatus] of [
    ["success", "delayed-dispatch-success", "succeeded"],
    ["failure", "delayed-dispatch-failure", "failed"],
    ["timeout", "delayed-dispatch-timeout", "timed_out"],
  ]) {
    const response = payload(await delayedExecutor.callTool({
      name: "fabric_dispatch",
      arguments: {
        prompt: `delayed dispatch ${kind}`,
        task_id: taskId,
        adapter: "codex",
        model: "gpt-fixture",
        wait_seconds: 0,
      },
    }));
    assert.equal(response.status, "running");
    assert.equal(response.task_id, taskId);
    await waitForFile(resolve(response.paths.run_dir, "delayed-ready"));
    assert.equal(existsSync(response.paths.attempt), false);
    writeFileSync(resolve(response.paths.run_dir, "delayed-release"), "release\n");
    await waitForFile(response.paths.attempt);
    const attempt = JSON.parse(readFileSync(response.paths.attempt, "utf8"));
    assert.equal(attempt.task_id, taskId);
    assert.equal(attempt.status, expectedStatus);
    assert.equal(attempt.outcome, `delayed dispatch ${kind}`);
    if (kind === "success") {
      assert.equal(readFileSync(resolve(response.paths.attempt, "../result.md"), "utf8"),
        "delayed dispatch success content\n");
    }
  }
  for (const [kind, taskId, expectedTaskStatus, expectedBatchStatus] of [
    ["success", "delayed-batch-success", "succeeded", "completed"],
    ["failure", "delayed-batch-failure", "failed", "failed"],
    ["timeout", "delayed-batch-timeout", "timed_out", "failed"],
  ]) {
    const response = payload(await delayedExecutor.callTool({
      name: "fabric_batch",
      arguments: {
        wait_seconds: 0,
        tasks: [{ id: taskId, prompt: `delayed batch ${kind}`, adapter: "codex", model: "gpt-fixture" }],
      },
    }));
    assert.equal(response.status, "running");
    assert.equal(response.batch_id, "batch-001");
    await waitForFile(resolve(response.paths.run_dir, "delayed-ready"));
    assert.equal(existsSync(response.paths.summary), false);
    writeFileSync(resolve(response.paths.run_dir, "delayed-release"), "release\n");
    await waitForFile(response.paths.summary);
    const summary = JSON.parse(readFileSync(response.paths.summary, "utf8"));
    assert.equal(summary.batch_id, response.batch_id);
    assert.equal(summary.status, expectedBatchStatus);
    assert.equal(summary.tasks[0].task_id, taskId);
    assert.equal(summary.tasks[0].status, expectedTaskStatus);
    assert.equal(summary.tasks[0].outcome, `delayed batch ${kind}`);
    if (kind === "success") {
      assert.equal(readFileSync(resolve(response.paths.run_dir, "dispatch/tasks", taskId, "attempt-001/result.md"), "utf8"),
        "delayed batch success content\n");
    }
  }
  await delayedExecutor.close();

  // Exercise the real orchestration owners without starting a provider. An
  // unsupported adapter fails inside cf_dispatch.sh after the real owners have
  // written and validated their attempt and batch evidence.
  const realWorkspace = resolve(executionRoot, "real-workspace");
  mkdirSync(realWorkspace, { recursive: true });
  writeFileSync(resolve(realWorkspace, "README.md"), "real workspace\n");
  execFileSync("git", ["init", "--quiet"], { cwd: realWorkspace });
  execFileSync("git", ["add", "README.md"], { cwd: realWorkspace });
  execFileSync("git", ["-c", "user.name=Fabric smoke", "-c", "user.email=fabric@example.invalid",
    "commit", "--quiet", "-m", "real"], { cwd: realWorkspace });
  const realHead = execFileSync("git", ["rev-parse", "HEAD"], { cwd: realWorkspace, encoding: "utf8" }).trim();
  writeFileSync(resolve(foreignProduct, "README.md"), "foreign workspace\n");
  execFileSync("git", ["add", "README.md"], { cwd: foreignProduct });
  execFileSync("git", ["-c", "user.name=Fabric smoke", "-c", "user.email=fabric@example.invalid",
    "commit", "--quiet", "-m", "foreign"], { cwd: foreignProduct });
  const realProviderBin = resolve(executionRoot, "real-provider-bin");
  mkdirSync(realProviderBin);
  const slowClaude = resolve(realProviderBin, "claude");
  const providerMarker = resolve(executionRoot, "real-provider.pid");
  writeFileSync(slowClaude,
    "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$FABRIC_SMOKE_PROVIDER_MARKER\"\n" +
    "trap 'exit 143' TERM INT HUP\nwhile :; do /bin/sleep 1; done\n");
  chmodSync(slowClaude, 0o755);
  const realExecutor = await spawnAgent("codex", "real-execution-client", {
    cwd: realWorkspace,
    productRoot: resolve(import.meta.dirname, "../.."),
    env: {
      PATH: `${realProviderBin}:/usr/bin:/bin`,
      ...(process.env.HARNESS_PYTHON === undefined ? {} : {
        HARNESS_PYTHON: process.env.HARNESS_PYTHON,
      }),
      GIT_DIR: resolve(foreignProduct, ".git"),
      GIT_WORK_TREE: foreignProduct,
      FABRIC_SMOKE_PROVIDER_MARKER: providerMarker,
    },
  });
  const realDispatch = payload(await realExecutor.callTool({
    name: "fabric_dispatch",
    arguments: {
      prompt: "exercise the real dispatch owner",
      adapter: "unsupported-fixture",
      model: "fixture-model",
      wait_seconds: 10,
    },
  }));
  assert.equal(realDispatch.status, "failed", JSON.stringify(realDispatch));
  const realAttempt = JSON.parse(readFileSync(realDispatch.paths.attempt, "utf8"));
  assert.equal(realAttempt.record_type, "dispatch-attempt");
  assert.equal(realAttempt.workspace.root, realpathSync(realWorkspace));
  assert.equal(realAttempt.workspace.base_revision, realHead);
  const realBatch = payload(await realExecutor.callTool({
    name: "fabric_batch",
    arguments: {
      concurrency: 2,
      wait_seconds: 10,
      tasks: [
        { id: "real-one", prompt: "first", adapter: "unsupported-fixture", model: "fixture-model" },
        { id: "real-two", prompt: "second", adapter: "unsupported-fixture", model: "fixture-model" },
      ],
    },
  }));
  assert.equal(realBatch.status, "completed");
  assert.deepEqual(realBatch.counts, { failed: 2 });
  assert.equal(realBatch.owner_exit, 1);
  assert.equal(JSON.parse(readFileSync(realBatch.paths.summary, "utf8")).record_type, "dispatch-batch");
  assert.ok(!readdirSync(realDispatch.paths.run_dir).some((name) => name.startsWith("owner.") || name === "prompt.md"));
  assert.ok(!readdirSync(realBatch.paths.run_dir).some((name) => name.startsWith("owner.") || name === "task-manifest.json"));

  const timedOut = payload(await realExecutor.callTool({
    name: "fabric_dispatch",
    arguments: {
      prompt: "force a timeout before result",
      adapter: "claude",
      model: "claude-opus-4-6",
      timeout_seconds: 1,
      wait_seconds: 10,
    },
  }));
  assert.equal(timedOut.status, "timed_out", JSON.stringify(timedOut));
  assert.equal(timedOut.paths.result, null);
  const finalizer = resolve(import.meta.dirname, "../../skills/orchestrate/scripts/run_dir_finalize.py");
  for (const runDir of [realDispatch.paths.run_dir, realBatch.paths.run_dir]) {
    assert.match(execFileSync(fixturePython, [
      finalizer, runDir, "--status", "failed", "--reason", "MCP smoke",
    ], { cwd: realWorkspace, encoding: "utf8" }), /PASS: run terminalised as failed/u);
  }

  const realCancelled = payload(await realExecutor.callTool({
    name: "fabric_dispatch",
    arguments: {
      prompt: "cancel the real owner during startup",
      task_id: "real-cancel",
      adapter: "claude",
      model: "claude-opus-4-6",
      wait_seconds: 0,
    },
  }));
  for (let attempts = 0; attempts < 500 && !existsSync(providerMarker); attempts += 1) {
    await new Promise((resolveWait) => setTimeout(resolveWait, 10));
  }
  if (!existsSync(providerMarker)) {
    const attemptPath = resolve(
      realCancelled.paths.run_dir, "dispatch/tasks/real-cancel/attempt-001/attempt.json",
    );
    assert.fail(JSON.stringify({
      running: realCancelled,
      attempt: existsSync(attemptPath) ? JSON.parse(readFileSync(attemptPath, "utf8")) : null,
      owner_stdout: readFileSync(realCancelled.paths.owner_stdout, "utf8"),
      owner_stderr: readFileSync(realCancelled.paths.owner_stderr, "utf8"),
    }));
  }
  const providerPid = Number(readFileSync(providerMarker, "utf8").trim());
  assert.ok(Number.isInteger(providerPid) && providerPid > 0);
  await Promise.race([
    realExecutor.close(),
    new Promise((_, reject) => {
      const timeout = setTimeout(() => reject(new Error("MCP transport close timed out")), 10_000);
      timeout.unref();
    }),
  ]);
  const realCancelledAttempt = resolve(
    realCancelled.paths.run_dir, "dispatch/tasks/real-cancel/attempt-001/attempt.json",
  );
  for (let attempts = 0; attempts < 500 && !existsSync(realCancelledAttempt); attempts += 1) {
    await new Promise((resolveWait) => setTimeout(resolveWait, 10));
  }
  const cancelledRecord = JSON.parse(readFileSync(realCancelledAttempt, "utf8"));
  assert.equal(cancelledRecord.status, "cancelled");
  assert.equal(cancelledRecord.failure_code, "cancelled");
  assert.equal(cancelledRecord.process.observed_exit, true);
  for (let attempts = 0; attempts < 500; attempts += 1) {
    try {
      process.kill(providerPid, 0);
      await new Promise((resolveWait) => setTimeout(resolveWait, 10));
    } catch {
      break;
    }
  }
  assert.throws(() => process.kill(providerPid, 0));
  assert.match(execFileSync(fixturePython, [
    finalizer, realCancelled.paths.run_dir, "--status", "cancelled", "--reason", "MCP close",
  ], { cwd: realWorkspace, encoding: "utf8" }), /PASS: run terminalised as cancelled/u);
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
