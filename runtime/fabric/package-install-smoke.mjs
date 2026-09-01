// Prove the packed package installs and launches without a product checkout or loader override.
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const packageRoot = import.meta.dirname;
const temporaryDirectory = mkdtempSync(join(tmpdir(), "fabric-package-install-"));
const packDirectory = join(temporaryDirectory, "pack");
const installDirectory = join(temporaryDirectory, "install");
const stateDirectory = join(temporaryDirectory, "state");
const npmCacheDirectory = join(temporaryDirectory, "npm-cache");
mkdirSync(packDirectory);
mkdirSync(installDirectory);
writeFileSync(join(installDirectory, "package.json"), '{"name":"fabric-install-smoke","private":true}\n');

const npm = (args) => {
  const npmCli = process.env.npm_execpath;
  const options = {
    encoding: "utf8",
    env: { ...process.env, npm_config_cache: npmCacheDirectory },
  };
  return npmCli === undefined
    ? execFileSync("npm", args, options)
    : execFileSync(process.execPath, [npmCli, ...args], options);
};

let client;
try {
  const packed = JSON.parse(npm([
    "pack", packageRoot, "--pack-destination", packDirectory, "--json",
  ]));
  const tarball = join(packDirectory, packed[0].filename);
  npm([
    "install", "--prefix", installDirectory, "--package-lock=false", "--omit=dev",
    "--no-audit", "--no-fund", tarball,
  ]);
  const installedRequire = createRequire(join(installDirectory, "package.json"));
  const { Client } = await import(pathToFileURL(
    installedRequire.resolve("@modelcontextprotocol/sdk/client/index.js"),
  ).href);
  const { StdioClientTransport } = await import(pathToFileURL(
    installedRequire.resolve("@modelcontextprotocol/sdk/client/stdio.js"),
  ).href);

  const binDirectory = join(installDirectory, "node_modules/.bin");
  const env = {
    HOME: process.env.HOME,
    PATH: "/usr/bin:/bin",
    FABRIC_NODE: process.execPath,
    AGENT_FABRIC_PRODUCT_ROOT: join(temporaryDirectory, "missing-product-root"),
    AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
    AGENT_FABRIC_SEAT: "package-smoke",
    AGENT_FABRIC_LABEL: "package-smoke",
    NODE_NO_WARNINGS: "1",
  };
  const cli = spawnSync(join(binDirectory, "fabric"), ["--help"], {
    cwd: temporaryDirectory,
    encoding: "utf8",
    env,
  });
  assert.equal(cli.status, 0, cli.stderr);
  assert.match(cli.stdout, /fabric <command>/);
  assert.doesNotMatch(cli.stderr, /tsx loader not found/);

  const unsupportedNode = join(temporaryDirectory, "node-26");
  writeFileSync(unsupportedNode, `#!${process.execPath}
if (process.argv[2] !== "-e" || typeof process.argv[3] !== "string") process.exit(99);
Object.defineProperty(process.versions, "node", { value: "26.0.0" });
(0, eval)(process.argv[3]);
`);
  chmodSync(unsupportedNode, 0o755);
  for (const binary of ["fabric", "fabric-mcp"]) {
    const unsupported = spawnSync(join(binDirectory, binary), ["--help"], {
      cwd: temporaryDirectory,
      encoding: "utf8",
      env: { ...env, FABRIC_NODE: unsupportedNode },
    });
    assert.equal(unsupported.status, 127, `${binary}: ${unsupported.stderr}`);
    assert.match(unsupported.stderr, /requires Node >=24\.15\.0 and <25/);
  }

  const transport = new StdioClientTransport({
    command: join(binDirectory, "fabric-mcp"),
    cwd: temporaryDirectory,
    env,
    stderr: "pipe",
  });
  let stderr = "";
  transport.stderr?.on("data", (chunk) => { stderr += chunk.toString(); });
  client = new Client({ name: "package-install-smoke", version: "1" });
  await client.connect(transport);
  const tools = await client.listTools();
  assert.equal(tools.tools.length, 13);
  assert.ok(tools.tools.some((tool) => tool.name === "fabric_whoami"));
  assert.ok(tools.tools.some((tool) => tool.name === "fabric_dispatch"));
  assert.ok(tools.tools.some((tool) => tool.name === "fabric_batch"));
  const identity = await client.callTool({ name: "fabric_whoami", arguments: {} });
  assert.equal(identity.isError, undefined, JSON.stringify(identity));
  assert.match(identity.content[0].text, /"provider": "package-smoke"/);
  assert.doesNotMatch(stderr, /tsx loader not found|ERR_MODULE_NOT_FOUND/);

  console.log("Packed package CLI and MCP launcher assertions passed");
} finally {
  await client?.close().catch(() => undefined);
  rmSync(temporaryDirectory, { recursive: true, force: true });
}
