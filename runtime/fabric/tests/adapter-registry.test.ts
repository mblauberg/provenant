import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { parse as parseYaml } from "yaml";

import { DISPATCH_ADAPTERS, dispatchConfiguredBatch, dispatchConfiguredProvider } from "../src/execution.js";
import { catalogueSnapshot } from "../src/catalogue.js";
import { normaliseRoute } from "../src/execution-input.js";
import type { Identity } from "../src/identity.js";
import { scratchRoot } from "./helpers/temp-root.js";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const catalogue = JSON.parse(
  readFileSync(join(repositoryRoot, "config", "model-routing.json"), "utf8"),
) as { adapters: Record<string, { endpoint_provider?: string; fixed_model_family?: string | null }> };
const compatibility = parseYaml(
  readFileSync(join(repositoryRoot, "config", "adapter-compatibility.yaml"), "utf8"),
) as {
  dispatch_registry?: Record<string, { dispatch?: string; write_modes?: string[] }>;
};
function registryAdapters(state: string): string[] {
  // herdr is a policy-only entry: it observes and steers but is never a
  // routing adapter, so it is excluded from the routable sets (it still
  // counts as unrunnable below, which is exactly the point).
  return Object.entries(compatibility.dispatch_registry ?? {})
    .filter(([name, entry]) => name !== "herdr" && entry.dispatch === state)
    .map(([name]) => name)
    .sort();
}

/**
 * Dispatch state lives in exactly one place: the product-owned
 * `dispatch_registry` in adapter-compatibility.yaml. The instance-owned
 * routing catalogue (`config/model-routing.json`, ADR 0019) carries no
 * dispatch field and may lag behind product policy, so no test here may read
 * adapter state from it. These tests read the registry and the Fabric schema, and
 * the generated vocabulary is checked for freshness. Unsupported adapters are
 * absent from the catalogue, not stubbed in it: the catalogue lists exactly
 * the implemented set.
 */
describe("adapter registry", () => {
  it("declares a known dispatch state in the registry for every catalogued adapter", () => {
    for (const name of Object.keys(catalogue.adapters)) {
      expect(["implemented", "dormant", "unsupported"], `registry ${name}`)
        .toContain(compatibility.dispatch_registry?.[name]?.dispatch);
    }
  });

  it("carries no dispatch mirror in the catalogue", () => {
    for (const [name, entry] of Object.entries(catalogue.adapters)) {
      expect(entry, `catalogue ${name}`).not.toHaveProperty("dispatch");
    }
  });

  it("keeps the product dispatch registry and the catalogue aligned", () => {
    // herdr is a policy-only entry: it observes and steers but is never a
    // routing adapter, so the instance-owned catalogue does not list it.
    const policyOnly = new Set(["herdr"]);
    const registry = Object.entries(compatibility.dispatch_registry ?? {})
      .filter(([name]) => !policyOnly.has(name));
    expect(registry.map(([name]) => name).sort())
      .toStrictEqual(Object.keys(catalogue.adapters).sort());
  });

  it("lists exactly the implemented adapters in the catalogue (no stubs)", () => {
    expect(Object.keys(catalogue.adapters).sort())
      .toStrictEqual(registryAdapters("implemented"));
  });

  it("keeps the generated dispatch vocabulary fresh against the registry", () => {
    execFileSync("python3", ["scripts/gen_dispatch_vocabulary.py", "--check"], { cwd: repositoryRoot });
    expect([...DISPATCH_ADAPTERS]).toStrictEqual(registryAdapters("implemented"));
  });

  it("keeps adapters the dispatcher cannot execute out of the schema", () => {
    const unrunnable = Object.entries(compatibility.dispatch_registry ?? {})
      .filter(([, entry]) => entry.dispatch !== "implemented")
      .map(([name]) => name);
    expect(unrunnable.length).toBeGreaterThan(0);
    for (const adapter of unrunnable) {
      expect(DISPATCH_ADAPTERS as readonly string[]).not.toContain(adapter);
    }
  });

  it("gives every implemented adapter an executable Python profile", () => {
    const profiles = JSON.parse(execFileSync("python3", ["-c", [
      "import json, adapters",
      "print(json.dumps({name: bool(adapters.profile(name).CLI) and callable(adapters.profile(name).argv) for name in adapters.NAMES}))",
    ].join("\n")], { cwd: join(repositoryRoot, "skills/orchestrate/scripts"), encoding: "utf8" }));
    expect(Object.keys(profiles).sort()).toEqual([...DISPATCH_ADAPTERS].sort());
    expect(Object.values(profiles).every(Boolean)).toBe(true);
  });
});

describe("adapter rejection", () => {
  let workspace: string;
  let identity: Identity;

  beforeEach(() => {
    workspace = scratchRoot("fabric-adapter-");
    identity = { project: workspace, cwd: workspace, agentId: "test-agent", provider: "claude" };
  });

  afterEach(() => {
    rmSync(workspace, { recursive: true, force: true });
  });

  for (const adapter of ["not-an-adapter"]) {
    it(`refuses ${adapter} before a run directory exists`, async () => {
      await expect(dispatchConfiguredProvider(
        { adapter, prompt: "hello" },
        identity,
        AbortSignal.abort(),
      )).resolves.toMatchObject({ status: "rejected", error: "adapter_invalid" });
      expect(existsSync(join(workspace, ".agent-run"))).toBe(false);
    });

    it(`refuses ${adapter} in a batch task before a run directory exists`, async () => {
      await expect(dispatchConfiguredBatch(
        { tasks: [{ adapter, prompt: "hello" }] },
        identity,
        AbortSignal.abort(),
      )).resolves.toMatchObject({ status: "rejected", error: "adapter_invalid" });
      expect(existsSync(join(workspace, ".agent-run"))).toBe(false);
    });
  }

  it("passes unknown model and alias selectors to the named adapter with a note", () => {
    const snapshot = catalogueSnapshot(repositoryRoot);
    for (const selector of [{ model: "missing-model" }, { alias: "missing-alias" }]) {
      const name = Object.values(selector)[0]!;
      const route = normaliseRoute({ adapter: "codex", ...selector }, identity, snapshot);
      expect(route).toMatchObject({ adapter: "codex", model: name });
      expect(route.warnings).toContain(`${name} is not in the codex catalogue; passing it as given`);
    }
  });

  it("returns each invalid task as a rejected row and creates no run when none is valid", async () => {
    const result = await dispatchConfiguredBatch({ tasks: [
      { id: "bad-prompt", adapter: "codex", prompt_file: "absent.md" },
      { id: "bad-prompt-too", adapter: "codex", prompt_file: "also-absent.md" },
    ], wait_seconds: 0 }, identity, new AbortController().signal,
    { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: repositoryRoot, AGENT_FABRIC_INSTANCE_ROOT: repositoryRoot });
    expect(result.status).toBe("rejected");
    const rows = result.tasks as Record<string, unknown>[];
    expect(rows.map((row) => row.task_id).sort()).toEqual(["bad-prompt", "bad-prompt-too"]);
    expect(rows.every((row) => row.status === "rejected" && row.error === "prompt_unavailable" && typeof row.fix === "string")).toBe(true);
    expect(existsSync(join(workspace, ".agent-run"))).toBe(false);
  });

  it("does not block the default alias for an adapter the routing catalogue carries no alias table for", async () => {
    // cursor, copilot, kiro and opencode pick their model per call rather than
    // through a fixed family, so config/model-routing.json's family-keyed
    // alias tables do not cover them; catalogueSnapshot resolves an empty
    // alias set for each. Enforcing against that empty set would reject the
    // default alias on every dispatch to these adapters, which is not the
    // caller mistake the front-door check exists to catch.
    const catalogueOnlyProduct = join(workspace, "catalogue-only-product");
    mkdirSync(join(catalogueOnlyProduct, "config"), { recursive: true });
    cpSync(
      join(repositoryRoot, "config", "model-routing.json"),
      join(catalogueOnlyProduct, "config", "model-routing.json"),
    );
    cpSync(
      join(repositoryRoot, "config", "adapter-compatibility.yaml"),
      join(catalogueOnlyProduct, "config", "adapter-compatibility.yaml"),
    );
    for (const adapter of ["cursor", "copilot", "kiro", "opencode"]) {
      // No skills/orchestrate/scripts directory exists under this synthetic
      // product root, so a dispatch that gets past alias validation fails
      // fast on the missing execution owner instead of spawning a real
      // provider: this isolates the alias check from launch mechanics.
      await expect(dispatchConfiguredProvider(
        { adapter, prompt: "hello" },
        identity,
        AbortSignal.abort(),
        { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: catalogueOnlyProduct },
      )).rejects.toBeInstanceOf(Error);
    }
  });

});

describe("instance catalogue", () => {
  it("deep merges live instance aliases and preserves the product on malformed overlay", () => {
    const root = scratchRoot("fabric-catalogue-");
    try {
      mkdirSync(join(root, "config"));
      const path = join(root, "config", "model-routing.json");
      const routing = JSON.parse(readFileSync(join(repositoryRoot, "config", "model-routing.json"), "utf8"));
      const expectedCodexModel = routing.families.openai.aliases.senior[0];
      routing.families.openai.aliases.senior = ["custom-luna"];
      const expectedOpenCodeModel = routing.adapters.opencode.models[0].id;
      writeFileSync(path, JSON.stringify(routing));
      const env = { AGENT_FABRIC_INSTANCE_ROOT: root };
      expect(catalogueSnapshot(repositoryRoot, env).adapters.find((adapter) => adapter.name === "codex")?.models).toContain("custom-luna");
      expect(catalogueSnapshot(repositoryRoot, env).adapters.find((adapter) => adapter.name === "opencode")?.models).toContain(expectedOpenCodeModel);
      writeFileSync(path, "invalid");
      expect(catalogueSnapshot(repositoryRoot, env).adapters.find((adapter) => adapter.name === "codex")?.models).toContain(expectedCodexModel);
      expect(catalogueSnapshot(repositoryRoot, env).drift).not.toEqual([]);
      rmSync(path);
      expect(catalogueSnapshot(repositoryRoot, env).adapters.find((adapter) => adapter.name === "codex")?.models).toContain(expectedCodexModel);
    } finally { rmSync(root, { recursive: true, force: true }); }
  });
});
