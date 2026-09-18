import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { parse as parseYaml } from "yaml";

import { DISPATCH_ADAPTERS, dispatchConfiguredBatch, dispatchConfiguredProvider } from "../src/execution.js";
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

  it("gives every implemented adapter an executing arm in the dispatcher", () => {
    for (const adapter of DISPATCH_ADAPTERS) {
      expect(dispatcher, `cf_dispatch.sh has no ${adapter} arm`).toMatch(
        new RegExp(`^\\s+${adapter}\\)$`, "mu"),
      );
    }
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
      )).rejects.toThrow(/adapter must be one of/u);
      expect(existsSync(join(workspace, ".agent-run"))).toBe(false);
    });

    it(`refuses ${adapter} in a batch task before a run directory exists`, async () => {
      await expect(dispatchConfiguredBatch(
        { tasks: [{ adapter, prompt: "hello" }] },
        identity,
        AbortSignal.abort(),
      )).rejects.toThrow(/adapter must be one of/u);
      expect(existsSync(join(workspace, ".agent-run"))).toBe(false);
    });
  }

  it("names the adapters it does accept", async () => {
    await expect(dispatchConfiguredProvider(
      { adapter: "pi", prompt: "hello" },
      identity,
      AbortSignal.abort(),
    )).rejects.toThrow(/agy, claude, codex, copilot, cursor, kiro, opencode/u);
  });
});
