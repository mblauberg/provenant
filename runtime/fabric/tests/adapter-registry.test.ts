import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { DISPATCH_ADAPTERS, dispatchConfiguredBatch, dispatchConfiguredProvider } from "../src/execution.js";
import type { Identity } from "../src/identity.js";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const catalogue = JSON.parse(
  readFileSync(join(repositoryRoot, "config", "model-routing.json"), "utf8"),
) as { adapters: Record<string, { dispatch?: string }> };
const dispatcher = readFileSync(
  join(repositoryRoot, "skills", "orchestrate", "scripts", "cf_dispatch.sh"),
  "utf8",
);

function shellList(name: string): string[] {
  const match = new RegExp(`^${name}="([^"]*)"$`, "mu").exec(dispatcher);
  if (match === null) throw new Error(`cf_dispatch.sh declares no ${name}`);
  return match[1]!.split(/\s+/u).filter((entry) => entry.length > 0).sort();
}

function catalogueAdapters(state: string): string[] {
  return Object.entries(catalogue.adapters)
    .filter(([, entry]) => entry.dispatch === state)
    .map(([name]) => name)
    .sort();
}

/**
 * The adapter list lives in three places that a single change can silently pull
 * apart: the Fabric schema, the routing catalogue and the dispatcher. These
 * tests read all three and fail on any disagreement.
 */
describe("adapter registry", () => {
  it("declares a known dispatch state for every catalogued adapter", () => {
    for (const [name, entry] of Object.entries(catalogue.adapters)) {
      expect(["implemented", "dormant", "unsupported"], `adapter ${name}`).toContain(entry.dispatch);
    }
  });

  it("agrees on the implemented adapters across the schema, the catalogue and the dispatcher", () => {
    const schema = [...DISPATCH_ADAPTERS].sort();
    expect(schema).toStrictEqual(catalogueAdapters("implemented"));
    expect(schema).toStrictEqual(shellList("DISPATCH_IMPLEMENTED_ADAPTERS"));
  });

  it("agrees on the adapters declared for routing but dormant in the dispatcher", () => {
    expect(shellList("DISPATCH_DORMANT_ADAPTERS")).toStrictEqual(catalogueAdapters("dormant"));
  });

  it("keeps adapters the dispatcher cannot execute out of the schema", () => {
    const unrunnable = [...catalogueAdapters("dormant"), ...catalogueAdapters("unsupported")];
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

  for (const adapter of ["opencode", "pi", "not-an-adapter"]) {
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
      { adapter: "opencode", prompt: "hello" },
      identity,
      AbortSignal.abort(),
    )).rejects.toThrow(/agy, claude, codex, copilot, cursor, kiro/u);
  });
});
