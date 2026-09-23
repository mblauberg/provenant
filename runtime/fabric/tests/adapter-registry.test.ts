import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { parse as parseYaml } from "yaml";

import { DISPATCH_ADAPTERS, dispatchConfiguredBatch, dispatchConfiguredProvider } from "../src/execution.js";
import { catalogueSnapshot } from "../src/catalogue.js";
import type { Identity } from "../src/identity.js";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const catalogue = JSON.parse(
  readFileSync(join(repositoryRoot, "config", "model-routing.json"), "utf8"),
) as { adapters: Record<string, { endpoint_provider?: string; fixed_model_family?: string | null }> };
const compatibility = parseYaml(
  readFileSync(join(repositoryRoot, "config", "adapter-compatibility.yaml"), "utf8"),
) as {
  dispatch_registry?: Record<string, { dispatch?: string; write_modes?: string[] }>;
};
const dispatcher = readFileSync(
  join(repositoryRoot, "skills", "orchestrate", "scripts", "cf_dispatch.sh"),
  "utf8",
);

function shellList(name: string): string[] {
  const match = new RegExp(`^${name}="([^"]*)"$`, "mu").exec(dispatcher);
  if (match === null) throw new Error(`cf_dispatch.sh declares no ${name}`);
  return match[1]!.split(/\s+/u).filter((entry) => entry.length > 0).sort();
}

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
 * adapter state from it. These tests read the registry, the Fabric schema and
 * the dispatcher, and fail on any disagreement. Unsupported adapters are
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

  it("agrees on the implemented adapters across the schema, the registry and the dispatcher", () => {
    const schema = [...DISPATCH_ADAPTERS].sort();
    expect(schema).toStrictEqual(registryAdapters("implemented"));
    expect(schema).toStrictEqual(shellList("DISPATCH_IMPLEMENTED_ADAPTERS"));
  });

  it("keeps adapters the registry marks dormant out of the dispatcher", () => {
    // No dormant adapters exist today; the loop is the guard for the day one
    // is added: dormant means refused (absent from the implemented list),
    // never silently runnable. The shell keeps no dormant list by design; the
    // registry is the only place dormant state is declared.
    const implemented = new Set(shellList("DISPATCH_IMPLEMENTED_ADAPTERS"));
    for (const adapter of registryAdapters("dormant")) {
      expect(DISPATCH_ADAPTERS as readonly string[]).not.toContain(adapter);
      expect(implemented.has(adapter), `dispatcher runs dormant ${adapter}`).toBe(false);
    }
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
    workspace = mkdtempSync(join(tmpdir(), "fabric-adapter-"));
    identity = { project: workspace, cwd: workspace, agentId: "test-agent", provider: "claude" };
  });

  afterEach(() => {
    rmSync(workspace, { recursive: true, force: true });
  });

  for (const adapter of ["pi", "not-an-adapter"]) {
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

  it("names the adapters it does accept", async () => {
    await expect(dispatchConfiguredProvider(
      { adapter: "pi", prompt: "hello" },
      identity,
      AbortSignal.abort(),
    )).resolves.toMatchObject({ status: "rejected", fix: expect.stringMatching(/agy, claude, codex, copilot, cursor, kiro, opencode/u) });
  });

  it("passes unknown model aliases to the owner, respecting cancellation", async () => {
    await expect(dispatchConfiguredProvider(
      { adapter: "codex", alias: "missing-model", prompt: "hello" },
      identity,
      AbortSignal.abort(),
      { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: repositoryRoot },
    )).rejects.toThrow(/aborted/u);
    expect(existsSync(join(workspace, ".agent-run"))).toBe(false);
  });

  it("uses the real router and returns every invalid task before creating a run", async () => {
    const result = await dispatchConfiguredBatch({ tasks: [
      { id: "bad-model", adapter: "claude", model: "unknown-model-family", prompt: "hello" },
      { id: "bad-prompt", adapter: "claude", prompt_file: "absent.md" },
      { id: "bad-alias", adapter: "codex", alias: "not-in-catalogue", prompt: "hello" },
    ], wait_seconds: 0 }, identity, new AbortController().signal,
    { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: repositoryRoot, AGENT_FABRIC_INSTANCE_ROOT: repositoryRoot });
    expect(result.status).toBe("rejected");
    expect((result.errors as Record<string, unknown>[]).map((error) => error.task_id)).toEqual(["bad-prompt"]);
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
      )).rejects.toThrow(/execution owner is unavailable/u);
    }
  });

});

describe("instance catalogue", () => {
  it("deep merges live instance aliases and preserves the product on malformed overlay", () => {
    const root = mkdtempSync(join(tmpdir(), "fabric-catalogue-"));
    try {
      mkdirSync(join(root, "config"));
      const path = join(root, "config", "model-routing.json");
      const routing = JSON.parse(readFileSync(join(repositoryRoot, "config", "model-routing.json"), "utf8"));
      routing.families.openai.aliases.workhorse = ["custom-luna"];
      writeFileSync(path, JSON.stringify(routing));
      const env = { AGENT_FABRIC_INSTANCE_ROOT: root };
      expect(catalogueSnapshot(repositoryRoot, env).adapters.find((adapter) => adapter.name === "codex")?.models).toContain("custom-luna");
      expect(catalogueSnapshot(repositoryRoot, env).adapters.find((adapter) => adapter.name === "opencode")?.models).toContain("opencode-go/glm-5.3-flash");
      writeFileSync(path, "invalid");
      expect(catalogueSnapshot(repositoryRoot, env).adapters.find((adapter) => adapter.name === "codex")?.models).toContain("gpt-6-luna");
      expect(catalogueSnapshot(repositoryRoot, env).drift).not.toEqual([]);
      rmSync(path);
      expect(catalogueSnapshot(repositoryRoot, env).adapters.find((adapter) => adapter.name === "codex")?.models).toContain("gpt-6-luna");
    } finally { rmSync(root, { recursive: true, force: true }); }
  });
});
