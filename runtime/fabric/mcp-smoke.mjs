// Assert the real MCP stdio contract through the same launcher clients register.
import assert from "node:assert/strict";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

const state = resolve(tmpdir(), `fabric-mcp-smoke-${String(process.pid)}`);
rmSync(state, { recursive: true, force: true });

// Override this with the managed `provenant` shim to verify installed routing.
const command = process.env.AGENT_FABRIC_MCP_COMMAND ?? resolve(import.meta.dirname, "bin/fabric-mcp");
const tsxLoader = process.env.AGENT_FABRIC_TSX_LOADER ??
  createRequire(import.meta.url).resolve("tsx");
const clients = [];

const spawnAgent = async (seat, clientLabel) => {
  const transport = new StdioClientTransport({
    command,
    cwd: process.cwd(),
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

try {
  const claude = await spawnAgent("claude", "claude-client");
  const codex = await spawnAgent("codex", "codex-client");
  const agy = await spawnAgent("agy", "agy-client");

  assert.match(claude.getInstructions() ?? "", /project-scoped mailbox/);

  const listed = await claude.listTools();
  assert.deepEqual(listed.tools.map((tool) => tool.name).sort(), [
    "fabric_acknowledge",
    "fabric_activity",
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
  await expectToolError(claude.callTool({
    name: "fabric_send",
    arguments: { to: "codex-client" },
  }));

  // Agy is a client seat. This smoke makes no claim about its selected model family.
  console.log("MCP contract assertions passed for claude, codex and agy client seats");
} finally {
  await Promise.allSettled(clients.map(async (client) => await client.close()));
  rmSync(state, { recursive: true, force: true });
}
