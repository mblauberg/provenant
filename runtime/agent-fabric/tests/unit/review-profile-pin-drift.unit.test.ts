import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

import type { ReviewProfileCatalogue } from "../../src/review/profile/index.ts";
import {
  collectReviewProfilePins,
  evaluateReviewProfilePinDrift,
  readPinObservations,
  reviewProfilePinOutcome,
  type PinRouteObservation,
  type PinRouteObserver,
} from "../../src/review/profile/pin-drift.ts";
import { createCapabilityPinObserver } from "../../src/review/profile/pin-observer.ts";
import { verifyReviewProfileCatalogueDigest } from "../../src/review/profile/catalogue-digest.ts";
import {
  refreshAndPinReviewProfilePins,
  refreshReviewProfilePins,
} from "../../scripts/pin-review-profile-models.ts";

function repositoryPath(relativePath: string): string {
  return fileURLToPath(new URL(`../../../../${relativePath}`, import.meta.url));
}

const catalogueSource = await readFile(
  repositoryPath("config/review-profiles/certifying-review-four-slot-v1.json"),
  "utf8",
);
const routingSource = await readFile(repositoryPath("config/model-routing.json"), "utf8");
const profileSchemaSource = await readFile(
  repositoryPath("runtime/agent-fabric/schemas/review-profile.v1.schema.json"),
  "utf8",
);
const catalogue = JSON.parse(catalogueSource) as ReviewProfileCatalogue;
const routing: unknown = JSON.parse(routingSource);

const cleanup: string[] = [];
afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

/** Resolve every alias to the model the profile already pins. */
const truthfulObserver: PinRouteObserver = async ({ providerFamily }) => ({
  status: "observed",
  model: providerFamily === "anthropic" ? "claude-opus-5" : "gpt-5.6-sol",
  detail: "fixture",
});

function fixedObserver(observation: PinRouteObservation): PinRouteObserver {
  return async () => observation;
}

async function report(input: {
  observe: PinRouteObserver;
  catalogue?: ReviewProfileCatalogue;
  routing?: unknown;
}): ReturnType<typeof evaluateReviewProfilePinDrift> {
  const selected = input.catalogue ?? catalogue;
  return evaluateReviewProfilePinDrift({
    catalogue: selected,
    observations: readPinObservations(selected),
    routing: input.routing ?? routing,
    observe: input.observe,
  });
}

describe("certifying profile pin drift", () => {
  it("collects one comparison entry per distinct pin, with every declaring slot", () => {
    const pins = collectReviewProfilePins(catalogue);
    expect(pins.map((pin) => `${pin.providerFamily}/${pin.model}`)).toStrictEqual([
      "openai/gpt-5.6-sol",
      "anthropic/claude-opus-5",
      "xai/cursor-grok-4.5-high",
      "google/Gemini 3.1 Pro (High)",
    ]);
    // Each primary pin is declared by one slot in each of the two chair profiles.
    expect(pins[0]!.slots).toStrictEqual(["openai/native", "anthropic/other-primary"]);
    expect(pins[1]!.routeAliases).toStrictEqual(["flagship"]);
    expect(pins[1]!.requestedEffort).toBe("xhigh");
  });

  it("reports a fresh profile clean, and never counts an unobservable family as evidence", async () => {
    const result = await report({ observe: truthfulObserver });
    expect(result.compared.map((pin) => [pin.providerFamily, pin.state])).toStrictEqual([
      ["openai", "clean"],
      ["anthropic", "clean"],
    ]);
    expect(result.attested.map((pin) => pin.providerFamily)).toStrictEqual(["xai", "google"]);
    expect(result.attested.every((pin) => /^\d{4}-\d{2}-\d{2}$/u.test(pin.observedOn))).toBe(true);
    expect(reviewProfilePinOutcome(result)).toStrictEqual({ status: "pass", code: "REVIEW_PROFILE_PIN_OK" });
  });

  it("reports drift when an alias now resolves to a different model", async () => {
    const result = await report({
      observe: async ({ providerFamily }) => providerFamily === "anthropic"
        ? { status: "observed", model: "claude-opus-6", detail: "fixture" }
        : { status: "observed", model: "gpt-5.6-sol", detail: "fixture" },
    });
    const anthropic = result.compared.find((pin) => pin.providerFamily === "anthropic")!;
    expect(anthropic.state).toBe("drifted");
    expect(anthropic.detail).toContain("now resolves to claude-opus-6, not claude-opus-5");
    expect(reviewProfilePinOutcome(result)).toStrictEqual({ status: "fail", code: "REVIEW_PROFILE_PIN_DRIFT" });
  });

  it("reports an unobservable provider as unknown, never clean and never drifted", async () => {
    for (const detail of [
      "quota exhausted",
      "Claude subscription authentication is unavailable",
      "connect ENETDOWN",
    ]) {
      const result = await report({ observe: fixedObserver({ status: "unobservable", detail }) });
      expect(result.compared.map((pin) => pin.state)).toStrictEqual(["unknown", "unknown"]);
      expect(result.compared.some((pin) => pin.state === "clean" || pin.state === "drifted")).toBe(false);
      expect(result.compared[0]!.detail).toBe(detail);
      expect(reviewProfilePinOutcome(result)).toStrictEqual({ status: "idle", code: "REVIEW_PROFILE_PIN_UNKNOWN" });
    }
  });

  it("separates a retired model from an unobservable provider", async () => {
    const result = await report({
      observe: fixedObserver({ status: "retired", detail: "catalogue no longer offers gpt-5.6-sol" }),
    });
    expect(result.compared.map((pin) => pin.state)).toStrictEqual(["drifted", "drifted"]);
    expect(reviewProfilePinOutcome(result).code).toBe("REVIEW_PROFILE_PIN_DRIFT");
  });

  it("reports a pin with no recorded observation as unknown, not clean", async () => {
    const stripped = JSON.parse(catalogueSource) as ReviewProfileCatalogue & { pinObservations: unknown[] };
    stripped.pinObservations = stripped.pinObservations.filter(
      (entry) => (entry as { providerFamily: string }).providerFamily !== "anthropic",
    );
    const result = await report({ catalogue: stripped, observe: truthfulObserver });
    const anthropic = result.compared.find((pin) => pin.providerFamily === "anthropic")!;
    expect(anthropic).toMatchObject({ state: "unknown", observedOn: null, detail: "no recorded observation for this pin" });
    expect(reviewProfilePinOutcome(result).status).toBe("idle");
  });

  it("refuses to accept a manual attestation in place of a family's own observer", async () => {
    const downgraded = JSON.parse(catalogueSource) as ReviewProfileCatalogue & {
      pinObservations: { providerFamily: string; observedVia: string }[];
    };
    for (const entry of downgraded.pinObservations) {
      if (entry.providerFamily === "anthropic") entry.observedVia = "manual-attestation";
    }
    const result = await report({ catalogue: downgraded, observe: truthfulObserver });
    expect(result.compared.find((pin) => pin.providerFamily === "anthropic")).toMatchObject({ state: "unknown" });
    expect(result.attested.map((pin) => pin.providerFamily)).toStrictEqual(["xai", "google"]);
  });

  it("reports drift when model routing no longer defines the recorded alias", async () => {
    const withoutFlagship = JSON.parse(routingSource) as { families: Record<string, { aliases: Record<string, unknown> }> };
    delete withoutFlagship.families.anthropic!.aliases.flagship;
    const result = await report({ observe: truthfulObserver, routing: withoutFlagship });
    expect(result.compared.find((pin) => pin.providerFamily === "anthropic")).toMatchObject({
      state: "drifted",
      detail: "model routing no longer defines alias flagship for anthropic",
    });
  });
});

async function agentsHomeWith(producers: Record<string, string>): Promise<string> {
  const home = await mkdtemp(join(tmpdir(), "agent-fabric-pin-observer-"));
  cleanup.push(home);
  const scripts = join(home, "skills", "orchestrate", "scripts");
  await mkdir(scripts, { recursive: true });
  for (const [name, body] of Object.entries(producers)) {
    await writeFile(join(scripts, name), body, { mode: 0o700 });
  }
  return home;
}

const codexProducer = (models: readonly string[]): string => `#!/usr/bin/env node
const fs = require("node:fs");
fs.appendFileSync(process.env.PIN_OBSERVER_CALL_LOG, "codex\\n");
process.stdout.write(JSON.stringify({
  schema_version: 1,
  source: "codex debug models",
  observed_at: new Date().toISOString(),
  models: Object.fromEntries(${JSON.stringify(models)}.map((slug) => [slug, { resolved_model: slug }])),
}));
`;

describe("capability pin observer", () => {
  it("observes the live model an alias resolves to through the repository producer", async () => {
    const home = await agentsHomeWith({ "codex_capabilities.py": codexProducer(["gpt-5.6-sol", "gpt-5.6-luna"]) });
    process.env.PIN_OBSERVER_CALL_LOG = join(home, "calls.log");
    const observe = createCapabilityPinObserver({ agentsHome: home });
    await expect(observe({
      providerFamily: "openai", routeAlias: "flagship", routeCandidates: ["gpt-5.6-sol"], requestedEffort: "max",
    })).resolves.toMatchObject({ status: "observed", model: "gpt-5.6-sol" });
  });

  it("separates a retired slug from a producer that did not complete", async () => {
    const home = await agentsHomeWith({ "codex_capabilities.py": codexProducer(["gpt-6-sol"]) });
    process.env.PIN_OBSERVER_CALL_LOG = join(home, "calls.log");
    const observe = createCapabilityPinObserver({ agentsHome: home });
    await expect(observe({
      providerFamily: "openai", routeAlias: "flagship", routeCandidates: ["gpt-5.6-sol"], requestedEffort: null,
    })).resolves.toMatchObject({ status: "retired" });
  });

  it.each([
    ["exits non-zero on quota or authentication failure", `#!/usr/bin/env node
console.error("capability discovery failed: quota exhausted");
process.exit(1);
`],
    ["is absent from the tree", undefined],
    ["returns an untrusted snapshot", `#!/usr/bin/env node
process.stdout.write(JSON.stringify({ schema_version: 1, source: "hand written", observed_at: new Date().toISOString(),
  models: { "gpt-5.6-sol": { resolved_model: "gpt-5.6-sol" } } }));
`],
  ])("reports unobservable when the producer %s", async (_label, body) => {
    const home = await agentsHomeWith(body === undefined ? {} : { "codex_capabilities.py": body });
    const observe = createCapabilityPinObserver({ agentsHome: home });
    const observation = await observe({
      providerFamily: "openai", routeAlias: "flagship", routeCandidates: ["gpt-5.6-sol"], requestedEffort: null,
    });
    expect(observation.status).toBe("unobservable");
  });

  it("rejects a Claude snapshot without subscription-canary provenance", async () => {
    const home = await agentsHomeWith({
      "claude_capabilities.py": `#!/usr/bin/env node
const fs = require("node:fs");
fs.writeFileSync(process.argv[process.argv.indexOf("--out") + 1], JSON.stringify({
  schema_version: 1, source: "claude subscription canary", observed_at: new Date().toISOString(),
  models: { opus: { resolved_model: "claude-opus-5" } },
}));
`,
    });
    const observe = createCapabilityPinObserver({ agentsHome: home });
    await expect(observe({
      providerFamily: "anthropic", routeAlias: "flagship", routeCandidates: ["opus"], requestedEffort: "xhigh",
    })).resolves.toMatchObject({ status: "unobservable" });
  });

  it("spends one probe per freshness window however often it is asked", async () => {
    const home = await agentsHomeWith({ "codex_capabilities.py": codexProducer(["gpt-5.6-sol"]) });
    const log = join(home, "calls.log");
    process.env.PIN_OBSERVER_CALL_LOG = log;
    await writeFile(log, "");
    let clock = Date.parse("2026-07-26T00:00:00Z");
    const observe = createCapabilityPinObserver({
      agentsHome: home, cacheDirectory: join(home, "state"), now: () => clock,
    });
    const request = {
      providerFamily: "openai" as const, routeAlias: "flagship", routeCandidates: ["gpt-5.6-sol"], requestedEffort: null,
    };
    await observe(request);
    await expect(observe(request)).resolves.toMatchObject({ status: "observed", model: "gpt-5.6-sol" });
    expect((await readFile(log, "utf8")).trim().split("\n")).toHaveLength(1);
    clock += 7 * 60 * 60 * 1000;
    await observe(request);
    expect((await readFile(log, "utf8")).trim().split("\n")).toHaveLength(2);
  });

  it("does not treat a future-dated cache entry as fresh evidence", async () => {
    const home = await agentsHomeWith({ "codex_capabilities.py": codexProducer(["gpt-5.6-sol"]) });
    const state = join(home, "state");
    const now = Date.parse("2026-07-26T00:00:00Z");
    await mkdir(state, { recursive: true });
    await writeFile(join(state, "review-profile-pin-observations.json"), `${JSON.stringify({
      schemaVersion: 1,
      entries: {
        "openai/flagship/gpt-5.6-sol": {
          observedModel: "gpt-5.6-sol",
          observedAtMs: now + 60_000,
        },
      },
    })}\n`);
    const observe = createCapabilityPinObserver({
      agentsHome: home,
      cacheDirectory: state,
      cacheOnly: true,
      now: () => now,
    });

    await expect(observe({
      providerFamily: "openai", routeAlias: "flagship", routeCandidates: ["gpt-5.6-sol"], requestedEffort: null,
    })).resolves.toStrictEqual({
      status: "unobservable",
      detail: "no provider capability result cached within the last six hours; live provider capability probe was not run",
    });
  });

  it("bypasses a fresh cache and refreshes it when live observation is explicitly requested", async () => {
    const home = await agentsHomeWith({ "codex_capabilities.py": codexProducer(["gpt-5.6-sol"]) });
    const state = join(home, "state");
    const log = join(home, "calls.log");
    const now = Date.parse("2026-07-26T00:00:00Z");
    process.env.PIN_OBSERVER_CALL_LOG = log;
    await mkdir(state, { recursive: true });
    await writeFile(log, "");
    await writeFile(join(state, "review-profile-pin-observations.json"), `${JSON.stringify({
      schemaVersion: 1,
      entries: {
        "openai/flagship/gpt-5.6-sol": { observedModel: "stale-cache-value", observedAtMs: now },
      },
    })}\n`);
    const observe = createCapabilityPinObserver({
      agentsHome: home,
      cacheDirectory: state,
      forceLive: true,
      now: () => now,
    });

    await expect(observe({
      providerFamily: "openai", routeAlias: "flagship", routeCandidates: ["gpt-5.6-sol"], requestedEffort: null,
    })).resolves.toMatchObject({
      status: "observed",
      model: "gpt-5.6-sol",
      detail: "observed live via codex_capabilities.py",
    });
    expect(await readFile(log, "utf8")).toBe("codex\n");
    expect(await readFile(join(state, "review-profile-pin-observations.json"), "utf8"))
      .toContain('"observedModel": "gpt-5.6-sol"');
  });
});

describe("profile:pin refresh", () => {
  async function profileCopy(): Promise<{ path: string; agentsHome: string }> {
    const home = await mkdtemp(join(tmpdir(), "agent-fabric-pin-refresh-"));
    cleanup.push(home);
    await mkdir(join(home, "config", "review-profiles"), { recursive: true });
    await mkdir(join(home, "runtime", "agent-fabric", "schemas"), { recursive: true });
    await mkdir(join(home, "runtime", "agent-fabric", "src", "review", "profile"), { recursive: true });
    await writeFile(join(home, "config", "review-profiles", "certifying-review-four-slot-v1.json"), catalogueSource);
    await writeFile(join(home, "config", "model-routing.json"), routingSource);
    await writeFile(join(home, "runtime", "agent-fabric", "schemas", "review-profile.v1.schema.json"), profileSchemaSource);
    return { path: join(home, "config", "review-profiles", "certifying-review-four-slot-v1.json"), agentsHome: home };
  }

  it("repairs every slot declaring a drifted pin and dates only the pins it observed", async () => {
    const { path, agentsHome } = await profileCopy();
    const result = await refreshReviewProfilePins({
      agentsHome,
      observe: async ({ providerFamily }) => ({
        status: "observed",
        model: providerFamily === "anthropic" ? "claude-opus-6" : "gpt-5.6-sol",
        detail: "fixture",
      }),
      now: () => Date.parse("2026-08-01T00:00:00Z"),
    });
    expect(result.changed).toBe(true);
    expect(result.outcomes.map((outcome) => [outcome.providerFamily, outcome.action])).toStrictEqual([
      ["openai", "unchanged"],
      ["anthropic", "repaired"],
      ["xai", "attested"],
      ["google", "attested"],
    ]);
    const updated = JSON.parse(await readFile(path, "utf8")) as ReviewProfileCatalogue & {
      pinObservations: { providerFamily: string; model: string; observedOn: string }[];
    };
    const anthropicSlots = updated.chairProfiles
      .flatMap((chair) => chair.slots)
      .filter((slot) => slot.providerFamily === "anthropic");
    expect(anthropicSlots).toHaveLength(2);
    expect(anthropicSlots.every((slot) => slot.model === "claude-opus-6")).toBe(true);
    expect(updated.pinObservations).toContainEqual({
      providerFamily: "anthropic", model: "claude-opus-6", routeAlias: "flagship",
      observedOn: "2026-08-01", observedVia: "claude-subscription-canary",
    });
    // A family with no producer keeps the date nobody re-observed.
    expect(updated.pinObservations.find((entry) => entry.providerFamily === "xai")!.observedOn).toBe("2026-07-16");
  });

  it("writes nothing when the provider cannot be observed", async () => {
    const { path, agentsHome } = await profileCopy();
    const result = await refreshReviewProfilePins({
      agentsHome,
      observe: fixedObserver({ status: "unobservable", detail: "quota exhausted" }),
      now: () => Date.parse("2026-08-01T00:00:00Z"),
    });
    expect(result.changed).toBe(false);
    expect(result.outcomes.filter((outcome) => outcome.action === "unobservable")).toHaveLength(2);
    expect(await readFile(path, "utf8")).toBe(catalogueSource);
  });

  it("advances the catalogue pin after partial writes even when another pin is unobservable", async () => {
    const { agentsHome } = await profileCopy();
    const result = await refreshAndPinReviewProfilePins({
      agentsHome,
      observe: async ({ providerFamily }) => providerFamily === "openai"
        ? { status: "observed", model: "gpt-5.6-sol", detail: "fixture" }
        : { status: "unobservable", detail: "quota exhausted" },
      now: () => Date.parse("2026-08-01T00:00:00Z"),
    });
    expect(result.refresh.changed).toBe(true);
    expect(result.refresh.outcomes.some((outcome) => outcome.action === "unobservable")).toBe(true);
    expect(result.cataloguePin.changed).toBe(true);
    await expect(verifyReviewProfileCatalogueDigest(
      agentsHome,
      result.cataloguePin.digest,
      result.cataloguePin.profileDocumentDigest,
    )).resolves.toMatchObject({
      digest: result.cataloguePin.digest,
      profileDocumentDigest: result.cataloguePin.profileDocumentDigest,
    });
  });

  it("rejects a custom refresh path that is not the digest-bound catalogue member", async () => {
    const { agentsHome } = await profileCopy();
    await expect(refreshAndPinReviewProfilePins({
      agentsHome,
      reviewProfilePath: join(agentsHome, "scratch-profile.json"),
      observe: truthfulObserver,
    })).rejects.toThrow(/digest-bound catalogue member/u);
  });
});
