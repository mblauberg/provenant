import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { beforeAll, describe, expect, it } from "vitest";

import { catalogueSnapshot } from "../src/catalogue.js";
import { attemptTaskId, faultLines, validateGraph } from "../src/graph/document.js";
import type { Identity } from "../src/identity.js";
import { scratchRoot } from "./helpers/temp-root.js";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const catalogue = catalogueSnapshot(repositoryRoot);
let repo: string, linked: string, identity: Identity;

const git = (cwd: string, ...args: string[]) => execFileSync("git", ["-C", cwd, ...args], { stdio: "ignore" });

beforeAll(() => {
  const root = scratchRoot("graph-document-");
  repo = join(root, "repo");
  mkdirSync(repo);
  git(repo, "init", "-q", "-b", "main");
  git(repo, "-c", "user.email=t@example.invalid", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "root");
  linked = join(root, "linked");
  git(repo, "worktree", "add", "-q", "-b", "work", linked);
  writeFileSync(join(repo, "brief.md"), "Read the code.\n");
  identity = { project: repo, cwd: repo, agentId: "codex-test", provider: "codex", registeredProjects: [repo] };
});

const reader = (id: string, extra: Record<string, unknown> = {}) => ({
  id, route: { adapter: "opencode", alias: "glm-free" }, brief: { prompt: `Survey ${id}.` }, ...extra,
});
const graph = (nodes: unknown[], extra: Record<string, unknown> = {}) => ({ schema_version: 1, graph_id: "g-0a1b2c", revision: 1, nodes, ...extra });
const errors = (document: unknown) => validateGraph(document, identity, catalogue).faults.map((fault) => `${fault.node ?? "-"}:${fault.error}`);

describe("graph document validation", () => {
  it("plans a fan-in graph with resolved routes, pools and spec hashes", () => {
    const result = validateGraph(graph([
      reader("a"), reader("b", { route: { adapter: "opencode", model: "deepseek-free" } }),
      reader("synth", { depends_on: ["a", "b"], inputs: [{ from: "a", part: "result" }, { from: "b", part: "result" }] }),
    ]), identity, catalogue);
    expect(result.faults).toEqual([]);
    const nodes = Object.fromEntries(result.plan!.nodes.map((node) => [node.id, node]));
    expect(nodes.a!.pool).toEqual({ adapter: "opencode", model: "opencode/glm-5.3-flash-free", account: "default" });
    expect(nodes.b!.pool.model).toBe("opencode/deepseek-v4.1-flash-free");
    expect(nodes.a!.spec_hash).toMatch(/^[0-9a-f]{64}$/u);
    expect(nodes.a!.spec_hash).not.toBe(nodes.synth!.spec_hash);
    expect(result.plan!.height).toEqual({ a: 2, b: 2, synth: 1 });
  });

  it("rejects a confidential parent feeding a non-confidential synthesis node", () => {
    const document = graph([
      reader("secret", { authority: { confidential: true } }),
      reader("synth", { depends_on: ["secret"], inputs: [{ from: "secret", part: "result" }], authority: { confidential: false } }),
    ]);
    expect(errors(document)).toEqual(["synth:confidentiality_downgrade"]);
    expect(faultLines(validateGraph(document, identity, catalogue).faults))
      .toBe("node synth: confidentiality_downgrade · fix: Remove confidential false: it consumes confidential secret output.");
  });

  it("reserves tier aliases sharing a resolved model against the same pool", () => {
    const model = "opencode/glm-5.3-flash-free";
    const shared = { ...catalogue, adapters: catalogue.adapters.map((adapter) => adapter.name !== "opencode" ? adapter
      : { ...adapter, aliases: { ...adapter.aliases, junior: [model], senior: [model] } }) };
    const result = validateGraph(graph([
      reader("a", { route: { adapter: "opencode", alias: "junior" } }),
      reader("b", { route: { adapter: "opencode", alias: "senior" } }),
      reader("c", { route: { adapter: "opencode", model } }),
    ]), identity, shared);
    expect(result.faults).toEqual([]);
    expect(result.plan!.nodes.map((node) => node.pool)).toEqual(Array(3).fill({ adapter: "opencode", model, account: "default" }));
    expect(result.plan!.nodes.map((node) => node.route.model)).toEqual([model, model, model]);
  });

  it("raises a consumer of confidential output to confidential when it does not say otherwise", () => {
    const result = validateGraph(graph([
      reader("secret", { authority: { confidential: true } }), reader("mid", { depends_on: ["secret"] }), reader("leaf", { depends_on: ["mid"] }),
    ]), identity, catalogue);
    expect(result.plan!.nodes.map((node) => [node.id, node.authority.confidential])).toEqual([["secret", true], ["mid", true], ["leaf", true]]);
  });

  it("reports cycles, unknown dependencies, inputs outside depends_on and duplicate ids", () => {
    expect(errors(graph([reader("a", { depends_on: ["b"] }), reader("b", { depends_on: ["a"] })]))).toEqual(["-:cycle"]);
    expect(errors(graph([reader("a", { depends_on: ["ghost"] })]))).toEqual(["a:dependency_unknown"]);
    expect(errors(graph([reader("a"), reader("b", { inputs: [{ from: "a", part: "result" }] })]))).toEqual(["b:input_not_dependency"]);
    expect(errors(graph([reader("a"), reader("a")]))).toContain("a:node_id_duplicate");
  });

  it("accepts only allowlisted gate argv and refuses later-slice fields", () => {
    const gate = { fast: [["true"]] };
    expect(errors(graph([reader("a", { gate })]))).toEqual(["a:gate_not_allowed"]);
    expect(errors(graph([reader("a", { gate })], { limits: { gate_allowlist: [["true"]] } }))).toEqual([]);
    expect(errors(graph([reader("a", { gate: { fast: [["true", "x"]] } })], { limits: { gate_allowlist: [["true"]] } })))
      .toEqual(["a:gate_not_allowed"]);
    expect(errors(graph([reader("a", { on_fail: "repair", kind: "review", gate: { slow: [["true"]] } })]))).toEqual([
      "a:slice_unsupported", "a:slice_unsupported", "a:slice_unsupported"]);
    expect(errors(graph([reader("a", { surprise: 1 })]))).toEqual(["a:field_unknown"]);
  });

  it("refuses unknown catalogue routes and widened authority", () => {
    expect(errors(graph([reader("a", { route: { adapter: "opencode", model: "not-a-model" } })]))).toEqual(["a:route_unknown"]);
    expect(errors(graph([reader("a", { route: { adapter: "opencode" } })]))).toEqual(["a:route_required"]);
    expect(errors(graph([reader("a", { authority: { network: true, allow_secrets: true } })])))
      .toEqual(["a:authority_widened", "a:authority_widened"]);
  });

  it.each(["credentials.json", ".ssh/config", ".config/codex/auth.json", "unrelated"])(
    "rejects a prompt_file from %s before reading or staging it", (name) => {
      const path = name === "unrelated" ? join(repositoryRoot, "README.md") : join(repo, name);
      if (name !== "unrelated") {
        mkdirSync(dirname(path), { recursive: true });
        writeFileSync(path, "fixture content, not a secret\n");
      }
      const result = validateGraph(graph([reader("a", { brief: { prompt_file: path } })]), identity, catalogue);
      expect(result.plan).toBeUndefined();
      expect(result.faults).toContainEqual(expect.objectContaining({ node: "a",
        error: name === "unrelated" ? "prompt_path_forbidden" : "credential_or_auth_store_denied" }));
    });

  it("isolates prompt validation from project Python modules and honours the Python owner selector", () => {
    const marker = join(repo, "python-imported");
    const shadow = join(repo, "base64.py");
    writeFileSync(shadow, `from pathlib import Path\nPath(${JSON.stringify(marker)}).touch()\nraise RuntimeError("project module executed")\n`);
    const python = execFileSync("/usr/bin/env", ["python3", "-I", "-c", "import sys; print(sys.executable)"], { encoding: "utf8" }).trim();
    const script = `import { readGraphPromptFile } from ${JSON.stringify(new URL("../src/graph/document.ts", import.meta.url).href)};
      process.stdout.write(readGraphPromptFile("brief.md", ${JSON.stringify(identity)}).toString());`;
    try {
      const result = spawnSync(process.execPath, ["--import", createRequire(import.meta.url).resolve("tsx"), "--input-type=module", "-e", script],
        { cwd: repo, env: { ...process.env, HARNESS_PYTHON: python, AGENT_FABRIC_PRODUCT_ROOT: repositoryRoot }, encoding: "utf8" });
      expect(existsSync(marker), result.stderr).toBe(false);
      expect(result.status, result.stderr).toBe(0);
      expect(result.stdout).toBe("Read the code.\n");
      const rejected = spawnSync(process.execPath, ["--import", createRequire(import.meta.url).resolve("tsx"), "--input-type=module", "-e", script],
        { cwd: repo, env: { ...process.env, HARNESS_PYTHON: "relative-python", AGENT_FABRIC_PRODUCT_ROOT: repositoryRoot }, encoding: "utf8" });
      expect(rejected.status).not.toBe(0);
      expect(rejected.stderr).toContain("Python execution owner is unavailable");
    } finally { rmSync(shadow); }
  });

  it("requires a writer to use a linked worktree, never the primary checkout, and orders writers sharing one", () => {
    const writer = (id: string, extra: Record<string, unknown> = {}) => reader(id, {
      route: { adapter: "cursor", model: "composer-2.5" }, authority: { mode: "worktree_write" }, worktree: "w", ...extra });
    const root = { authority: { mode: "worktree_write" } };
    expect(errors(graph([writer("a")], { ...root, worktrees: { w: { path: repo } } }))).toEqual(["-:worktree_primary"]);
    expect(errors(graph([writer("a"), writer("b")], { ...root, worktrees: { w: { path: linked } } }))).toEqual(["b:writers_unordered"]);
    const ordered = validateGraph(graph([writer("a"), writer("b", { depends_on: ["a"] })], { ...root, worktrees: { w: { path: linked } } }), identity, catalogue);
    expect(ordered.faults).toEqual([]);
    expect(ordered.plan!.nodes[0]!.worktree!.path).toBe(linked);
    expect(errors(graph([writer("a")], { worktrees: { w: { path: linked } } }))).toEqual(["a:authority_widened"]);
  });

  it("derives a short dispatch task id from the full attempt identity", () => {
    const key = { project: repo, graph_id: "g-0123456789abcdef", node_id: "lead/a-very-long-node-name", revision: 1, attempt: 1, restart: 0 };
    const id = attemptTaskId(key);
    expect(id).toMatch(/^g-0123456789abcdef\.[0-9a-f]{24}$/u);
    expect(id.length).toBeLessThanOrEqual(43);
    expect(id).toMatch(/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/u);
    expect(attemptTaskId({ ...key, restart: 1 })).not.toBe(id);
    expect(attemptTaskId({ ...key })).toBe(id);
  });

  it("keeps the live free-route acceptance graph valid against the catalogue", () => {
    const document = JSON.parse(readFileSync(join(repositoryRoot, "runtime/fabric/tests/fixtures/graph-live-free.json"), "utf8"));
    const result = validateGraph({ ...document, graph_id: "g-0a1b2c", revision: 1 }, identity, catalogue);
    expect(result.faults).toEqual([]);
    expect(result.plan!.nodes.map((node) => [node.id, node.pool.adapter, node.authority.mode])).toEqual([
      ["registry", "opencode", "read_only"], ["lease", "opencode", "read_only"], ["synth", "opencode", "read_only"]]);
  });
});
