// Drive the server over real MCP stdio, as a client would.
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

const state = resolve(tmpdir(), `fabric-mcp-smoke-${String(process.pid)}`);
rmSync(state, { recursive: true, force: true });

// Spawn the launcher the MCP registrations name, with the sparse environment an
// MCP client actually gives it. If this works, the registration works.
const launcher = resolve(import.meta.dirname, "bin/fabric-mcp");

const spawnAgent = async (seat) => {
  const transport = new StdioClientTransport({
    command: launcher,
    cwd: process.cwd(),
    env: {
      HOME: process.env.HOME,
      PATH: "/usr/bin:/bin",
      AGENT_FABRIC_SEAT: seat,
      AGENT_FABRIC_STATE_DIRECTORY: state,
    },
  });
  const client = new Client({ name: `test-${seat}`, version: "1" });
  await client.connect(transport);
  return client;
};

const text = (result) => JSON.parse(result.content[0].text);

const claude = await spawnAgent("claude");
const codex = await spawnAgent("codex");

const tools = await claude.listTools();
console.log("tools:", tools.tools.map((t) => t.name).join(", "));

console.log("whoami:", JSON.stringify(text(await claude.callTool({ name: "fabric_whoami", arguments: {} })).agentId));

const sent = text(await claude.callTool({
  name: "fabric_send",
  arguments: { to: "codex", body: "Does the parser handle empty input?", kind: "request" },
}));
console.log("sent to:", sent.recipients.join(", "));

const inbox = text(await codex.callTool({ name: "fabric_inbox", arguments: {} }));
console.log("codex received:", inbox.length, "->", inbox[0]?.body);

await codex.callTool({
  name: "fabric_send",
  arguments: { to: "claude", body: "No. It throws on a zero-length buffer.", reply_to: inbox[0].messageId },
});
const reply = text(await claude.callTool({ name: "fabric_inbox", arguments: {} }));
console.log("claude received reply:", reply[0]?.body);

console.log("activity:", text(await claude.callTool({ name: "fabric_activity", arguments: {} })).length, "entries");

await claude.close();
await codex.close();
rmSync(state, { recursive: true, force: true });
console.log("MCP round trip OK");
