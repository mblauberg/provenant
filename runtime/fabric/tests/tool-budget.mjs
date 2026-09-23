import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
const state = mkdtempSync(join(tmpdir(), "fabric-budget-"));
const client = new Client({ name: "budget", version: "1" });
try {
  await client.connect(
    new StdioClientTransport({
      command: process.execPath,
      args: ["--import", "tsx", resolve(import.meta.dirname, "../src/server.ts")],
      cwd: resolve(import.meta.dirname, ".."),
      env: { ...process.env, AGENT_FABRIC_STATE_DIRECTORY: state, AGENT_FABRIC_LABEL: "budget" },
      stderr: "pipe",
    }),
  );
  const result = await client.listTools();
  const chars = JSON.stringify(result).length;
  console.log(
    JSON.stringify({ tools: result.tools.length, chars, tokens: chars / 4, names: result.tools.map((t) => t.name) }),
  );
} finally {
  await client.close();
  rmSync(state, { recursive: true, force: true });
}
