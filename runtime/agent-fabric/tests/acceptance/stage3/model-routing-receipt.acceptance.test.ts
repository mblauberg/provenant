import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import {
  readJsonObject,
  repositoryPath,
  requirePublicFunction,
} from "../../support/primary-adapter-testkit.ts";

describe("FR-015 controlled model routing receipt", () => {
  it("reports the router exit status and stderr when resolve emits no JSON", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-router-failure-"));
    const receiptPath = join(directory, "model-route.json");
    const routerPath = join(directory, "failing-router");
    await writeFile(routerPath, `#!/usr/bin/env python3
import sys

print("router diagnostic", file=sys.stderr)
sys.exit(7)
`, { mode: 0o700 });

    try {
      await expect(resolveRoute({
        routerPath,
        receiptPath,
        request: {
          adapter: "codex",
          alias: "scout",
          role: "worker",
          leadFamily: "anthropic",
          requireDistinct: true,
        },
      })).rejects.toSatisfy((error: unknown) => {
        return error instanceof Error &&
          /model router failed.*exit code 7.*router diagnostic/su.test(error.message) &&
          !error.message.includes("Unexpected end of JSON input");
      });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("reports a zero-exit router that emits no JSON as a router failure", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-router-empty-"));
    const receiptPath = join(directory, "model-route.json");
    const routerPath = join(directory, "empty-router");
    await writeFile(routerPath, "#!/bin/sh\nexit 0\n", { mode: 0o700 });

    try {
      await expect(resolveRoute({
        routerPath,
        receiptPath,
        request: {
          adapter: "codex",
          alias: "scout",
          role: "worker",
          leadFamily: "anthropic",
          requireDistinct: true,
        },
      })).rejects.toSatisfy((error: unknown) => {
        return error instanceof Error &&
          /model router failed.*exit code 0.*stderr unavailable/su.test(error.message) &&
          !error.message.includes("Unexpected end of JSON input");
      });
      await expect(readFile(receiptPath, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("reports a router spawn error without presenting it as an exit code", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-router-enoent-"));
    const receiptPath = join(directory, "model-route.json");

    try {
      await expect(resolveRoute({
        routerPath: join(directory, "missing-router"),
        receiptPath,
        request: {
          adapter: "codex",
          alias: "scout",
          role: "worker",
          leadFamily: "anthropic",
          requireDistinct: true,
        },
      })).rejects.toSatisfy((error: unknown) => {
        return error instanceof Error &&
          error.message.includes("spawn error ENOENT") &&
          !error.message.includes("exit code ENOENT");
      });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("reports the selector exit status and stderr when select emits no JSON", async () => {
    const selectRoute = requirePublicFunction("selectPreferredModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-selector-failure-"));
    const receiptPath = join(directory, "selection.json");
    const routerPath = join(directory, "failing-selector");
    await writeFile(routerPath, `#!/usr/bin/env python3
import json
import sys

if sys.argv[1] == "select":
    print("selector diagnostic", file=sys.stderr)
    sys.exit(9)

print(json.dumps({
    "schema_version": 1,
    "status": "ok",
    "adapter": "codex",
    "role": "critical-review",
    "task_class": "critical-review",
    "route_source": "task-class",
    "alias": "flagship",
    "requested_effort": "high",
    "effort": "high",
    "effort_capability_source": "runtime-model-catalog",
    "endpoint_provider": "openai",
    "lead_family": "anthropic",
    "model_family": "openai",
    "distinct_from_lead": True,
    "resolved_model": "reviewer",
    "identity_source": "runtime-capability+catalog"
}))
`, { mode: 0o700 });

    try {
      await expect(selectRoute({
        routerPath,
        receiptPath,
        preferencesPath: join(directory, "preferences.json"),
        spreadStatePath: join(directory, "spread-state.json"),
        taskClass: "critical-review",
        role: "critical-review",
        candidates: [{
          candidateId: "critical-review",
          request: {
            adapter: "codex",
            taskClass: "critical-review",
            role: "critical-review",
            leadFamily: "anthropic",
            requireDistinct: true,
          },
          availability: { observation: "Unknown", reason: "AvailabilityNotObserved" },
        }],
      })).rejects.toSatisfy((error: unknown) => {
        return error instanceof Error &&
          /model preference selector failed.*exit code 9.*selector diagnostic/su.test(error.message) &&
          !error.message.includes("Unexpected end of JSON input");
      });
      await expect(readFile(`${receiptPath}.candidates.json`, "utf8"))
        .rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("admits a Claude task-class route at the probed effort and records unverified effort provenance", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-claude-capability-"));
    const receiptPath = join(directory, "model-route.json");
    const producerPath = join(directory, "fake-claude-capabilities");
    await writeFile(producerPath, `#!/usr/bin/env node
const fs = require("node:fs");
const out = process.argv[process.argv.indexOf("--out") + 1];
const alias = process.argv[process.argv.indexOf("--alias") + 1];
const effort = process.argv[process.argv.indexOf("--effort") + 1];
fs.writeFileSync(out, JSON.stringify({
  schema_version: 1, source: "claude subscription canary", observed_at: new Date().toISOString(),
  provenance: { kind: "subscription_runtime_canary", auth_method: "claude.ai", subscription_type: "pro" },
  models: { [alias]: { resolved_model: "claude-opus-4-8", requested_effort: effort, effort_verified: false } }
}));
`, { mode: 0o700 });

    const resolution = await resolveRoute({
      routerPath: repositoryPath("scripts/model-route"),
      receiptPath,
      testClaudeCapabilitiesPath: producerPath,
      request: {
        adapter: "claude",
        taskClass: "critical-review",
        role: "critical-review",
        leadFamily: "openai",
        requireDistinct: true,
      },
    }) as { receipt: Record<string, unknown> };

    expect(resolution.receipt).toMatchObject({
      status: "ok", task_class: "critical-review", alias: "flagship",
      resolved_model: "claude-opus-4-8", requested_effort: "high", effort: "high",
      // The model is runtime-verified; the effort is only accepted-not-observed,
      // and the receipt must never launder that into runtime-model-catalog.
      identity_source: "runtime-capability+catalog",
      effort_capability_source: "provider-unverified",
    });
  });

  it("rejects caller-supplied Claude task-class capability evidence", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-claude-capability-bypass-"));
    const receiptPath = join(directory, "model-route.json");
    const attackerSnapshot = join(directory, "attacker-snapshot.json");
    await writeFile(attackerSnapshot, JSON.stringify({
      schema_version: 1,
      source: "claude subscription canary",
      observed_at: new Date().toISOString(),
      provenance: { kind: "subscription_runtime_canary", auth_method: "claude.ai", subscription_type: "pro" },
      models: { opus: { resolved_model: "haiku", supported_efforts: ["high"] } },
    }));

    await expect(resolveRoute({
      routerPath: repositoryPath("scripts/model-route"),
      receiptPath,
      request: {
        adapter: "claude",
        taskClass: "critical-review",
        capabilitiesFile: attackerSnapshot,
        role: "critical-review",
        leadFamily: "openai",
        requireDistinct: true,
      },
    })).rejects.toThrow(/wrapper-produced subscription canary/u);
    await expect(readFile(receiptPath, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("rejects a Claude capability producer override outside the test environment", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-claude-producer-override-"));
    const receiptPath = join(directory, "model-route.json");
    const originalNodeEnvironment = process.env.NODE_ENV;
    process.env.NODE_ENV = "production";
    try {
      await expect(resolveRoute({
        routerPath: repositoryPath("scripts/model-route"),
        receiptPath,
        testClaudeCapabilitiesPath: join(directory, "attacker-producer"),
        request: {
          adapter: "claude",
          taskClass: "critical-review",
          role: "critical-review",
          leadFamily: "openai",
          requireDistinct: true,
        },
      })).rejects.toThrow(/producer override is test-only/u);
      await expect(readFile(receiptPath, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      if (originalNodeEnvironment === undefined) delete process.env.NODE_ENV;
      else process.env.NODE_ENV = originalNodeEnvironment;
    }
  });

  it("binds a task class to the router invocation and retained receipt", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-task-route-receipt-"));
    const receiptPath = join(directory, "model-route.json");
    const routerPath = repositoryPath("scripts/model-route");
    const capabilitiesFile = join(directory, "capabilities.json");
    await writeFile(capabilitiesFile, `${JSON.stringify({
      schema_version: 1,
      source: "codex debug models",
      observed_at: new Date().toISOString(),
      models: {
        "gpt-5.6-luna": {
          resolved_model: "gpt-5.6-luna",
          supported_efforts: ["low"],
        },
      },
    })}\n`);

    const resolution = await resolveRoute({
      routerPath,
      receiptPath,
      request: {
        adapter: "codex",
        taskClass: "mechanical",
        capabilitiesFile,
        role: "worker",
        leadFamily: "anthropic",
        requireDistinct: true,
      },
    }) as {
      invocation: { executable: string; arguments: string[] };
      receipt: Record<string, unknown>;
    };

    expect(resolution.invocation.executable).toBe(routerPath);
    expect(resolution.invocation.arguments[0]).toBe("resolve");
    expect(resolution.invocation.arguments).toEqual(expect.arrayContaining([
      "--task-class", "mechanical",
      "--capabilities-file", capabilitiesFile,
    ]));
    expect(resolution.invocation.arguments).not.toContain("--alias");
    expect(resolution.receipt).toMatchObject({
      status: "ok",
      task_class: "mechanical",
      route_source: "task-class",
      alias: "scout",
      requested_effort: "low",
      effort: "low",
      resolved_model: "",
      catalog_model: "gpt-5.6-luna",
      model_selection: "account-default",
      identity_source: "account-default",
    });
    expect(JSON.parse(await readFile(receiptPath, "utf8"))).toEqual(resolution.receipt);
  });

  it("rejects an explicit model before invoking a task-class route", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-task-model-conflict-"));
    const receiptPath = join(directory, "model-route.json");

    await expect(resolveRoute({
      routerPath: repositoryPath("scripts/model-route"),
      receiptPath,
      request: {
        adapter: "claude",
        taskClass: "critical-review",
        model: "haiku",
        role: "critical-review",
        leadFamily: "openai",
        requireDistinct: true,
      } as never,
    })).rejects.toThrow(/does not accept an explicit model/u);
    await expect(readFile(receiptPath, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("persists a typed unknown-task rejection before failing closed", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-unknown-task-receipt-"));
    const receiptPath = join(directory, "model-route.json");
    const resolution = resolveRoute({
      routerPath: repositoryPath("scripts/model-route"),
      receiptPath,
      request: {
        adapter: "claude",
        taskClass: "renamed-review",
        capabilitiesFile: join(directory, "not-needed-for-invalid-input.json"),
        role: "critical-review",
        leadFamily: "openai",
        requireDistinct: true,
      },
    });

    await expect(resolution).rejects.toMatchObject({
      code: "MODEL_ROUTE_REJECTED",
      receipt: {
        status: "unknown_task_class",
        adapter: "claude",
        role: "critical-review",
        task_class: "renamed-review",
        route_source: "task-class",
      },
    });
    expect(await readJsonObject(receiptPath)).toMatchObject({
      status: "unknown_task_class",
      task_class: "renamed-review",
    });
  });

  it("rejects and does not persist a mismatched ok receipt", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-bad-route-receipt-"));
    const receiptPath = join(directory, "model-route.json");
    const routerPath = join(directory, "fake-router");
    await writeFile(routerPath, `#!/usr/bin/env python3
import json

print(json.dumps({
    "schema_version": 1, "status": "ok", "adapter": "claude", "role": "worker",
    "alias": "scout", "requested_effort": "low", "effort": "low",
    "effort_capability_source": "runtime-model-catalog", "endpoint_provider": "anthropic",
    "model_family": "anthropic", "resolved_model": "haiku", "identity_source": "runtime-capability+catalog"
}))
`, { mode: 0o700 });

    await expect(resolveRoute({
      routerPath,
      receiptPath,
      request: {
        adapter: "codex",
        alias: "scout",
        role: "worker",
        leadFamily: "anthropic",
        requireDistinct: true,
      },
    })).rejects.toThrow(/invalid receipt/u);
    await expect(readFile(receiptPath, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("rejects and does not persist a malformed status-ok receipt", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-malformed-route-receipt-"));
    const receiptPath = join(directory, "model-route.json");
    const routerPath = join(directory, "fake-router");
    await writeFile(routerPath, `#!/usr/bin/env python3
import json

print(json.dumps({ "schema_version": 1, "status": "ok", "adapter": "codex", "role": "worker", "alias": "scout" }))
`, { mode: 0o700 });

    await expect(resolveRoute({
      routerPath,
      receiptPath,
      request: {
        adapter: "codex",
        alias: "scout",
        role: "worker",
        leadFamily: "anthropic",
        requireDistinct: true,
      },
    })).rejects.toThrow(/invalid receipt/u);
    await expect(readFile(receiptPath, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
  });

  it.each([
    ["downgraded alias", "scout", "high", "high", "openai", "anthropic", true],
    ["downgraded effort", "flagship", "low", "low", "openai", "anthropic", true],
    ["downgraded effective effort", "flagship", "high", "low", "openai", "anthropic", true],
    ["same-family reviewer", "flagship", "high", "high", "openai", "openai", false],
    ["wrong lead family", "flagship", "high", "high", "anthropic", "google", true],
    ["empty effective effort", "flagship", "high", "", "openai", "anthropic", true],
  ])("rejects and does not persist a task-class receipt with %s", async (
    _case, alias, requestedEffort, effectiveEffort, receiptLeadFamily, modelFamily, distinctFromLead,
  ) => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-policy-mismatch-"));
    const receiptPath = join(directory, "model-route.json");
    const routerPath = join(directory, "fake-router");
    await writeFile(routerPath, `#!/usr/bin/env python3
import json

print(json.dumps({
    "schema_version": 1, "status": "ok", "adapter": "codex", "role": "critical-review",
    "task_class": "critical-review", "route_source": "task-class", "alias": ${JSON.stringify(alias)},
    "requested_effort": ${JSON.stringify(requestedEffort)}, "effort": ${JSON.stringify(effectiveEffort)},
    "effort_capability_source": "runtime-model-catalog", "endpoint_provider": "openai",
    "lead_family": ${JSON.stringify(receiptLeadFamily)}, "model_family": ${JSON.stringify(modelFamily)},
    "distinct_from_lead": ${distinctFromLead ? "True" : "False"}, "resolved_model": "reviewer",
    "identity_source": "runtime-capability+catalog"
}))
`, { mode: 0o700 });

    await expect(resolveRoute({
      routerPath,
      receiptPath,
      request: {
        adapter: "codex",
        taskClass: "critical-review",
        capabilitiesFile: join(directory, "capabilities.json"),
        role: "critical-review",
        leadFamily: "openai",
        requireDistinct: true,
      },
    })).rejects.toThrow(/invalid receipt/u);
    await expect(readFile(receiptPath, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
  });

  it("retains the complete rejection receipt and fails closed for a disabled adapter", async () => {
    const resolveRoute = requirePublicFunction("resolveModelRouteReceipt");
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-route-receipt-"));
    const receiptPath = join(directory, "model-route.json");
    const routerPath = repositoryPath("scripts/model-route");

    const resolution = resolveRoute({
      routerPath,
      receiptPath,
      request: {
        adapter: "pi",
        alias: "workhorse",
        role: "worker",
        model: "qwen3-coder",
        leadFamily: "anthropic",
        requireDistinct: true,
      },
    });
    await expect(resolution).rejects.toMatchObject({
      code: "MODEL_ROUTE_REJECTED",
      receipt: { status: "adapter_disabled", adapter: "pi" },
    });
    const retained = await readJsonObject(receiptPath);

    expect(retained).toMatchObject({
      schema_version: 1,
      status: "adapter_disabled",
      adapter: "pi",
      alias: "workhorse",
      role: "worker",
      lead_family: "anthropic",
      model_family: "alibaba",
      endpoint_provider: "configured",
      requested_effort: "medium",
      effort: "medium",
      adapter_enabled: false,
    });
    expect(JSON.parse(await readFile(receiptPath, "utf8"))).toEqual(retained);
  });
});
