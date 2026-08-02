import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { expect, it } from "vitest";

import {
  resolveModelRouteReceipt,
  selectPreferredModelRouteReceipt,
} from "../../src/routing/model-route.ts";

function repositoryPath(relativePath: string): string {
  return fileURLToPath(new URL(`../../../../${relativePath}`, import.meta.url));
}

it.each([
  { taskClass: "mechanical", role: "worker", expectedAlias: "haiku" },
  { taskClass: "legwork", role: "worker", expectedAlias: "opus" },
  { taskClass: "critical-review", role: "critical-review", expectedAlias: "opus" },
  { taskClass: "orchestration", role: "orchestrator", expectedAlias: "opus" },
])("derives the $taskClass Claude capability probe alias from the catalogue", async ({
  taskClass, role, expectedAlias,
}) => {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-model-route-catalogue-"));
  const receiptPath = join(directory, "model-route.json");
  const argumentsPath = join(directory, "producer-arguments.json");
  const producerPath = join(directory, "fake-claude-capabilities");
  await writeFile(producerPath, `#!/usr/bin/env node
const fs = require("node:fs");
const out = process.argv[process.argv.indexOf("--out") + 1];
const alias = process.argv[process.argv.indexOf("--alias") + 1];
const effort = process.argv[process.argv.indexOf("--effort") + 1];
fs.writeFileSync(${JSON.stringify(argumentsPath)}, JSON.stringify(process.argv.slice(2)));
fs.writeFileSync(out, JSON.stringify({
  schema_version: 1, source: "claude subscription canary", observed_at: new Date().toISOString(),
  provenance: { kind: "subscription_runtime_canary", auth_method: "claude.ai", subscription_type: "pro" },
  models: { [alias]: { resolved_model: "claude-" + alias + "-5", requested_effort: effort, effort_verified: false } }
}));
`, { mode: 0o700 });

  try {
    await resolveModelRouteReceipt({
      routerPath: repositoryPath("scripts/model-route"),
      receiptPath,
      testClaudeCapabilitiesPath: producerPath,
      request: {
        adapter: "claude",
        taskClass,
        role,
        leadFamily: "openai",
        requireDistinct: true,
      },
    });

    const producerArguments = JSON.parse(await readFile(argumentsPath, "utf8")) as string[];
    expect(producerArguments[producerArguments.indexOf("--alias") + 1]).toBe(expectedAlias);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

it("selects through the Python preference stage without changing the hard route", async () => {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-model-preference-"));
  const receiptPath = join(directory, "selection.json");
  const preferencesPath = join(directory, "preferences.json");
  const spreadStatePath = join(directory, "spread-state.json");
  const capabilitiesPath = join(directory, "codex-capabilities.json");
  await writeFile(preferencesPath, `${JSON.stringify({
    schema_version: 1,
    task_classes: {
      "critical-review": { family_affinity: ["openai"] },
    },
    spreading: { policy: "fair-round-robin" },
  })}\n`);
  await writeFile(capabilitiesPath, `${JSON.stringify({
    schema_version: 1,
    source: "codex debug models",
    observed_at: new Date().toISOString(),
    models: {
      "gpt-5.6-sol": {
        resolved_model: "gpt-5.6-sol",
        supported_efforts: ["high", "max"],
      },
    },
  })}\n`);

  try {
    const result = await selectPreferredModelRouteReceipt({
      routerPath: fileURLToPath(new URL("../../../../scripts/model-route", import.meta.url)),
      receiptPath,
      preferencesPath,
      spreadStatePath,
      taskClass: "critical-review",
      role: "critical-review",
      candidates: [{
        candidateId: "openai-flagship",
        request: {
          adapter: "codex",
          taskClass: "critical-review",
          role: "critical-review",
          capabilitiesFile: capabilitiesPath,
          leadFamily: "anthropic",
          requireDistinct: true,
        },
        availability: { observation: "Unknown", reason: "AvailabilityNotObserved" },
      }],
    });

    expect(result.receipt.chosen_route).toEqual(expect.objectContaining({
      status: "ok",
      adapter: "codex",
      task_class: "critical-review",
      alias: "flagship",
      requested_effort: "max",
      effort: "max",
      model_family: "openai",
      resolved_model: "",
      catalog_model: "gpt-5.6-sol",
    }));
    expect(result.receipt.candidates).toEqual([
      expect.objectContaining({
        candidate_id: "openai-flagship",
        selected: true,
        availability: {
          observation: "Unknown",
          reason: "AvailabilityNotObserved",
        },
      }),
    ]);
    expect(JSON.parse(await readFile(spreadStatePath, "utf8"))).toEqual({
      schema_version: 1,
      assignments: { openai: 1 },
      selection_count: 1,
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

it("passes over a policy-mismatched task-class candidate without rejecting the receipt", async () => {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-model-preference-floor-"));
  const receiptPath = join(directory, "selection.json");
  const preferencesPath = join(directory, "preferences.json");
  const spreadStatePath = join(directory, "spread-state.json");
  const capabilitiesPath = join(directory, "codex-capabilities.json");
  await writeFile(preferencesPath, `${JSON.stringify({ schema_version: 1 })}\n`);
  await writeFile(capabilitiesPath, `${JSON.stringify({
    schema_version: 1,
    source: "codex debug models",
    observed_at: new Date().toISOString(),
    models: {
      "gpt-5.6-sol": {
        resolved_model: "gpt-5.6-sol",
        supported_efforts: ["high", "max"],
      },
      "gpt-5.6-terra": {
        resolved_model: "gpt-5.6-terra",
        supported_efforts: ["medium"],
      },
    },
  })}\n`);

  try {
    const result = await selectPreferredModelRouteReceipt({
      routerPath: repositoryPath("scripts/model-route"),
      receiptPath,
      preferencesPath,
      spreadStatePath,
      taskClass: "critical-review",
      role: "critical-review",
      candidates: [
        {
          candidateId: "critical-review",
          request: {
            adapter: "codex",
            taskClass: "critical-review",
            role: "critical-review",
            capabilitiesFile: capabilitiesPath,
            leadFamily: "anthropic",
            requireDistinct: true,
          },
          availability: { observation: "Observed", value: "available" },
        },
        {
          candidateId: "legwork",
          request: {
            adapter: "codex",
            taskClass: "legwork",
            role: "worker",
            capabilitiesFile: capabilitiesPath,
            leadFamily: "anthropic",
            requireDistinct: true,
          },
          availability: { observation: "Observed", value: "available" },
        },
      ],
    });

    expect(result.receipt.chosen_candidate_id).toBe("critical-review");
    expect(result.receipt.candidates).toEqual(expect.arrayContaining([
      expect.objectContaining({
        candidate_id: "legwork",
        admissible: false,
        selected: false,
        disposition: "hard_policy_mismatch",
      }),
    ]));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

it("uses a valid per-task-class capability probe alias override", async () => {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-model-route-override-"));
  const scriptsDirectory = join(directory, "scripts");
  const configDirectory = join(directory, "config");
  const routerPath = join(scriptsDirectory, "model-route");
  const receiptPath = join(directory, "model-route.json");
  const argumentsPath = join(directory, "producer-arguments.json");
  const producerPath = join(directory, "fake-claude-capabilities");
  await Promise.all([
    mkdir(scriptsDirectory),
    mkdir(configDirectory),
  ]);
  await writeFile(join(configDirectory, "model-routing.json"), `${JSON.stringify({
    task_class_routes: {
      orchestration: {
        alias: "flagship",
        effort: "high",
        role: "orchestrator",
        capability_probe_alias: "haiku",
      },
    },
    families: {
      anthropic: {
        aliases: {
          flagship: ["opus"],
          scout: ["haiku"],
        },
      },
    },
    adapters: {
      claude: {
        fixed_model_family: "anthropic",
      },
    },
  })}\n`);
  await writeFile(routerPath, `#!/usr/bin/env python3
import json

print(json.dumps({
    "schema_version": 1, "status": "ok", "adapter": "claude", "role": "orchestrator",
    "task_class": "orchestration", "route_source": "task-class", "alias": "flagship",
    "requested_effort": "high", "effort": "high", "effort_capability_source": "runtime-model-catalog",
    "endpoint_provider": "anthropic", "lead_family": "openai", "model_family": "anthropic",
    "distinct_from_lead": True, "resolved_model": "claude-opus-5",
    "identity_source": "runtime-capability+catalog"
}))
`, { mode: 0o700 });
  await writeFile(producerPath, `#!/usr/bin/env node
const fs = require("node:fs");
const out = process.argv[process.argv.indexOf("--out") + 1];
const alias = process.argv[process.argv.indexOf("--alias") + 1];
const effort = process.argv[process.argv.indexOf("--effort") + 1];
fs.writeFileSync(${JSON.stringify(argumentsPath)}, JSON.stringify(process.argv.slice(2)));
fs.writeFileSync(out, JSON.stringify({
  schema_version: 1, source: "claude subscription canary", observed_at: new Date().toISOString(),
  provenance: { kind: "subscription_runtime_canary", auth_method: "claude.ai", subscription_type: "pro" },
  models: { [alias]: { resolved_model: "claude-" + alias + "-5", requested_effort: effort, effort_verified: false } }
}));
`, { mode: 0o700 });

  try {
    await resolveModelRouteReceipt({
      routerPath,
      receiptPath,
      testClaudeCapabilitiesPath: producerPath,
      request: {
        adapter: "claude",
        taskClass: "orchestration",
        role: "orchestrator",
        leadFamily: "openai",
        requireDistinct: true,
      },
    });

    const producerArguments = JSON.parse(await readFile(argumentsPath, "utf8")) as string[];
    expect(producerArguments[producerArguments.indexOf("--alias") + 1]).toBe("haiku");
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

it.each([
  { label: "an empty string", capabilityProbeAlias: "" },
  { label: "a non-string value", capabilityProbeAlias: 7 },
  { label: "a model outside the adapter family aliases", capabilityProbeAlias: "fable" },
])("fails closed when the capability probe alias override is $label", async ({
  capabilityProbeAlias,
}) => {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-model-route-invalid-override-"));
  const routerPath = join(directory, "scripts", "model-route");
  await Promise.all([
    mkdir(join(directory, "scripts")),
    mkdir(join(directory, "config")),
  ]);
  await writeFile(join(directory, "config", "model-routing.json"), `${JSON.stringify({
    task_class_routes: {
      orchestration: {
        alias: "flagship",
        effort: "high",
        role: "orchestrator",
        capability_probe_alias: capabilityProbeAlias,
      },
    },
    families: {
      anthropic: {
        aliases: {
          flagship: ["opus"],
        },
      },
    },
    adapters: {
      claude: {
        fixed_model_family: "anthropic",
      },
    },
  })}\n`);

  try {
    await expect(resolveModelRouteReceipt({
      routerPath,
      receiptPath: join(directory, "model-route.json"),
      request: {
        adapter: "claude",
        taskClass: "orchestration",
        role: "orchestrator",
        leadFamily: "openai",
        requireDistinct: true,
      },
    })).rejects.toThrow(/valid Claude capability probe alias/u);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

it.each([
  { label: "unreadable", catalogueContents: undefined, expectedMessage: /readable model routing catalogue/u },
  { label: "invalid JSON", catalogueContents: "{", expectedMessage: /readable model routing catalogue/u },
  { label: "missing its alias chain", catalogueContents: "{}", expectedMessage: /valid Claude capability probe alias/u },
])("fails closed before probing when the catalogue is $label", async ({
  catalogueContents, expectedMessage,
}) => {
  const directory = await mkdtemp(join(tmpdir(), "agent-fabric-model-route-invalid-catalogue-"));
  const routerPath = join(directory, "scripts", "model-route");
  await Promise.all([
    mkdir(join(directory, "scripts")),
    mkdir(join(directory, "config")),
  ]);
  if (catalogueContents !== undefined) {
    await writeFile(join(directory, "config", "model-routing.json"), catalogueContents);
  }

  try {
    await expect(resolveModelRouteReceipt({
      routerPath,
      receiptPath: join(directory, "model-route.json"),
      request: {
        adapter: "claude",
        taskClass: "orchestration",
        role: "orchestrator",
        leadFamily: "openai",
        requireDistinct: true,
      },
    })).rejects.toThrow(expectedMessage);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
