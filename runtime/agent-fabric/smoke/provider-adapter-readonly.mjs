#!/usr/bin/env node

import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { lstat, mkdir, mkdtemp, readFile, readdir, readlink, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { parse } from "yaml";

const execFileAsync = promisify(execFile);

import { AdapterProcessTransport } from "../dist/adapters/process.js";
import { providerConformanceEvidence, verifyProviderConformance } from "../dist/adapters/provider-conformance.js";

function option(name, required = true) {
  const index = process.argv.indexOf(name);
  const value = index === -1 ? undefined : process.argv[index + 1];
  if (required && (value === undefined || value.startsWith("--"))) throw new Error(`${name} is required`);
  return value;
}

const adapterId = option("--adapter");
const model = option("--model");
const modelFamily = option("--model-family");
const effort = option("--effort", false);
const providerExecutable = option("--provider-executable");
const provider = option("--provider", false);
const wrapper = {
  "claude-agent-sdk": "adapters/providers/claude-agent-sdk.ts",
  "codex-app-server": "adapters/providers/codex-app-server.ts",
  "pi-rpc": "adapters/providers/optional/pi-rpc.ts",
  agy: "adapters/providers/optional/agy.ts",
  "cursor-agent": "adapters/providers/optional/cursor-agent.ts",
  "kiro-acp": "adapters/providers/optional/kiro-acp.ts",
  "opencode-acp": "adapters/providers/optional/opencode-acp.ts",
}[adapterId];
if (wrapper === undefined) throw new Error(`unsupported adapter ${adapterId}`);
const agentsRoot = resolve(new URL("../../../", import.meta.url).pathname);
const compatibility = parse(await readFile(join(agentsRoot, "config/adapter-compatibility.yaml"), "utf8"));
const compatibilityEntry = compatibility?.adapters?.[adapterId];
if (
  typeof compatibilityEntry !== "object" || compatibilityEntry === null ||
  compatibilityEntry.enabled !== true
) {
  throw new Error(`adapter ${adapterId} is not enabled`);
}
const implementation = compatibilityEntry.implementation;
const expandPath = (value) => value
  .replaceAll("${USER_HOME}", process.env.HOME ?? "")
  .replaceAll("${AGENTS_HOME}", agentsRoot);
const configuredExecutable = expandPath(implementation.executable);
if (await realpath(providerExecutable) !== await realpath(configuredExecutable)) {
  throw new Error("provider executable does not match the compatibility path");
}
const providerConfigRoot = adapterId === "opencode-acp"
  ? join(process.env.XDG_CONFIG_HOME ?? join(process.env.HOME ?? "", ".config"), "opencode")
  : undefined;
const providerConfigBefore = providerConfigRoot === undefined ? undefined : await optionalTreeDigest(providerConfigRoot);
const providerConformance = await verifyProviderConformance({
  adapterId,
  executable: configuredExecutable,
  ...(implementation.cursor_install_root === undefined ? {} : {
    cursorInstallRoot: expandPath(implementation.cursor_install_root),
  }),
  ...(implementation.provider_install_root === undefined ? {} : {
    providerInstallRoot: expandPath(implementation.provider_install_root),
  }),
});
const wrapperPath = resolve(new URL("../src", import.meta.url).pathname, wrapper);
if (await realpath(wrapperPath) !== await realpath(join(agentsRoot, implementation.wrapper_entrypoint))) {
  throw new Error("wrapper entrypoint does not match the compatibility path");
}
// Repository-owned wrapper code carries Git provenance: the repository commit
// plus the tracked wrapper path. Provider version and digest observations are
// runtime evidence, never compatibility gates. All GIT_* environment variables
// are stripped so repository discovery cannot be redirected.
/** @type {Record<string, string>} */
const gitEnvironment = Object.fromEntries(
  Object.entries(process.env).filter(([key, value]) => value !== undefined && !key.startsWith("GIT_")),
);
gitEnvironment.GIT_CONFIG_GLOBAL = "/dev/null";
gitEnvironment.GIT_CONFIG_SYSTEM = "/dev/null";
const git = async (...args) =>
  (await execFileAsync("git", ["-C", agentsRoot, ...args], { env: gitEnvironment })).stdout.trim();
const wrapperRepositoryCommit = await git("rev-parse", "HEAD");
if (!/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/u.test(wrapperRepositoryCommit)) throw new Error("wrapper repository commit is unavailable");
await git("cat-file", "-e", `HEAD:${implementation.wrapper_entrypoint}`).catch(() => {
  throw new Error("wrapper entrypoint is not tracked at the repository HEAD");
});
await git("diff", "--quiet", "HEAD", "--", implementation.wrapper_entrypoint).catch(() => {
  throw new Error("wrapper entrypoint differs from its committed content");
});

const directory = await mkdtemp(join(tmpdir(), `agent-fabric-${adapterId}-smoke-`));
const workspace = join(directory, "workspace");
await mkdir(workspace);
await writeFile(join(workspace, "READ_ONLY_SENTINEL.txt"), "agent-fabric provider smoke\n", { mode: 0o600 });

async function workspaceDigest(root) {
  const entries = [];
  async function visit(directoryPath, relativePrefix = "") {
    for (const name of (await readdir(directoryPath)).sort()) {
      const path = join(directoryPath, name);
      const relativePath = join(relativePrefix, name);
      const metadata = await lstat(path);
      if (metadata.isDirectory()) {
        entries.push([relativePath, "directory"]);
        await visit(path, relativePath);
      } else if (metadata.isSymbolicLink()) {
        entries.push([relativePath, "symlink", await readlink(path)]);
      } else if (metadata.isFile()) {
        entries.push([relativePath, "file", metadata.mode & 0o777, createHash("sha256").update(await readFile(path)).digest("hex")]);
      } else {
        entries.push([relativePath, "other"]);
      }
    }
  }
  await visit(root);
  return createHash("sha256").update(JSON.stringify(entries)).digest("hex");
}

async function optionalTreeDigest(root) {
  try {
    return await workspaceDigest(root);
  } catch (error) {
    if (error?.code === "ENOENT") return "missing";
    throw error;
  }
}

const beforeDigest = await workspaceDigest(workspace);
const args = [
  "--import", join(agentsRoot, "node_modules/tsx/dist/loader.mjs"),
  "--conditions=source",
  wrapperPath,
  "--journal", join(directory, "journal.sqlite3"),
  "--provider-executable", configuredExecutable,
  ...(implementation.provider_identity === undefined ? [] : ["--provider-identity-policy", implementation.provider_identity]),
  ...((implementation.provider_install_root ?? implementation.cursor_install_root) === undefined
    ? []
    : ["--provider-install-root", expandPath(implementation.provider_install_root ?? implementation.cursor_install_root)]),
  ...(provider === undefined ? [] : ["--allowed-provider", provider]),
];
const transport = new AdapterProcessTransport({ command: [process.execPath, ...args], environment: {}, responseTimeoutMs: 120_000 });
let phase = "capabilities";
try {
  const capabilities = await transport.request("capabilities", {});
  if (capabilities?.adapterId !== adapterId || !Array.isArray(capabilities?.operations)) {
    throw new Error("adapter capabilities do not identify the requested adapter");
  }
  for (const operation of ["spawn", "send_turn", "release"]) {
    if (!capabilities.operations.includes(operation)) throw new Error(`adapter does not advertise ${operation}`);
  }
  const common = { cwd: workspace, model, modelFamily, ...(effort === undefined ? {} : { effort }) };
  phase = "spawn";
  const spawned = await transport.request("spawn", {
    actionId: `${adapterId}:smoke:spawn`,
    payload: {
      ...common,
      ...(provider === undefined ? {} : { provider }),
      prompt: "Return only FABRIC_SMOKE_OK. Do not use tools or modify files.",
      maxTurns: 1,
    },
  });
  if (typeof spawned !== "object" || spawned === null || typeof spawned.resumeReference !== "string") {
    throw new Error("adapter smoke spawn returned no resume reference");
  }
  phase = "send_turn";
  const turn = await transport.request("dispatch", {
    actionId: `${adapterId}:smoke:turn`,
    operation: "send_turn",
    payload: {
      ...common,
      ...(provider === undefined ? {} : { provider }),
      resumeReference: spawned.resumeReference,
      prompt: "Return only FABRIC_SMOKE_TURN_OK. Do not use tools or modify files.",
    },
  });
  if (typeof turn !== "object" || turn === null || turn.status !== "terminal") throw new Error("adapter smoke turn was not terminal");
  const providerResult = turn.result;
  const outputField = adapterId === "kiro-acp" || adapterId === "opencode-acp" || adapterId === "pi-rpc" ? "text" : "result";
  const output = typeof providerResult === "object" && providerResult !== null ? providerResult[outputField] : undefined;
  if (typeof output !== "string" || output.trim() !== "FABRIC_SMOKE_TURN_OK") {
    throw new Error("adapter smoke turn did not return the exact sentinel");
  }
  phase = "release";
  await transport.request("release", {
    actionId: `${adapterId}:smoke:release`,
    payload: { resumeReference: spawned.resumeReference },
  });
  phase = "read-only-verification";
  const afterDigest = await workspaceDigest(workspace);
  if (afterDigest !== beforeDigest) throw new Error("provider smoke modified the isolated workspace");
  if (providerConfigRoot !== undefined && await optionalTreeDigest(providerConfigRoot) !== providerConfigBefore) {
    throw new Error("provider smoke modified provider configuration");
  }
  process.stdout.write(`${JSON.stringify({
    status: "pass",
    adapterId,
    requestedModel: model,
    modelFamily,
    providerConformance: providerConformanceEvidence(providerConformance),
    wrapper: { path: implementation.wrapper_entrypoint, repositoryCommit: wrapperRepositoryCommit },
    output: "exact-sentinel",
    workspace: "unchanged",
    ...(providerConfigRoot === undefined ? {} : { providerConfig: "unchanged" }),
    credentialInput: "subscription-session",
    fabricCapability: "not-provided",
    session: "spawn-turn-release",
  })}\n`);
} catch (error) {
  process.stderr.write(`${JSON.stringify({ status: "fail", adapterId, phase, code: error?.code, message: error?.message, details: error?.details })}\n`);
  throw error;
} finally {
  await transport.close();
  await rm(directory, { recursive: true, force: true });
}
