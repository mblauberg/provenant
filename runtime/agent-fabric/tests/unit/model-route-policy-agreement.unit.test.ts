import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import { expect, it } from "vitest";

import { taskClassPolicy } from "../../src/routing/model-route.ts";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

it("keeps the independent task-class verification policy aligned with the routing catalogue", async () => {
  const cataloguePath = fileURLToPath(
    new URL("../../../../config/model-routing.json", import.meta.url),
  );
  const catalogue: unknown = JSON.parse(await readFile(cataloguePath, "utf8"));
  if (!isRecord(catalogue) || !isRecord(catalogue.task_class_routes)) {
    throw new TypeError("model routing catalogue task classes are malformed");
  }
  const routes = catalogue.task_class_routes;

  expect([...taskClassPolicy.keys()].sort()).toEqual(Object.keys(routes).sort());
  for (const [taskClass, policy] of taskClassPolicy) {
    const route = routes[taskClass];
    if (!isRecord(route)) {
      throw new TypeError(`model routing catalogue task class ${taskClass} is malformed`);
    }
    expect(policy, taskClass).toEqual({
      minimumAlias: route.alias,
      minimumEffort: route.effort,
      role: route.role,
    });
  }
});

it("keeps soft preferences in a separate catalogue without hard routing authority", async () => {
  const preferencesPath = fileURLToPath(
    new URL("../../../../config/model-preferences.json", import.meta.url),
  );
  const preferences: unknown = JSON.parse(await readFile(preferencesPath, "utf8"));
  if (!isRecord(preferences)) {
    throw new TypeError("model preferences are malformed");
  }

  expect(preferences).not.toHaveProperty("task_class_routes");
  expect(preferences).not.toHaveProperty("families");
  expect(preferences).not.toHaveProperty("adapters");
  expect(preferences).not.toHaveProperty("risk_tier_overrides");
});
