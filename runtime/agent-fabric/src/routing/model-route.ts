import { execFile } from "node:child_process";
import { readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

class ModelRouteRejectedError extends Error {
  readonly code = "MODEL_ROUTE_REJECTED";

  constructor(
    readonly receipt: Record<string, unknown>,
    readonly invocation: { executable: string; arguments: string[] },
  ) {
    super(`model route rejected: ${String(receipt.status)}`);
    this.name = "ModelRouteRejectedError";
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

type ModelRouteRequest = {
  adapter: string;
  role: string;
  capabilitiesFile?: string;
  leadFamily: string;
  requireDistinct: boolean;
} & (
  | { alias: string; taskClass?: never; effort?: string; model?: string }
  | { taskClass: string; alias?: never; effort?: never; model?: never; capabilitiesFile?: string }
);

function nonEmptyString(record: Record<string, unknown>, key: string): boolean {
  return typeof record[key] === "string" && record[key].length > 0;
}

const aliasRank = new Map([["scout", 0], ["workhorse", 1], ["flagship", 2]]);
const effortRank = new Map([["low", 0], ["medium", 1], ["high", 2], ["xhigh", 3], ["max", 4], ["ultra", 5]]);
export const taskClassPolicy = new Map([
  ["mechanical", { minimumAlias: "scout", minimumEffort: "low", role: "worker" }],
  ["legwork", { minimumAlias: "workhorse", minimumEffort: "medium", role: "worker" }],
  ["critical-review", { minimumAlias: "flagship", minimumEffort: "high", role: "critical-review" }],
  ["orchestration", { minimumAlias: "flagship", minimumEffort: "high", role: "orchestrator" }],
]);

function isValidReceipt(receipt: Record<string, unknown>, request: ModelRouteRequest): boolean {
  if (
    receipt.schema_version !== 1 || typeof receipt.status !== "string" ||
    receipt.adapter !== request.adapter || receipt.role !== request.role
  ) return false;
  if (request.taskClass !== undefined) {
    const policy = taskClassPolicy.get(request.taskClass);
    const receiptAliasRank = aliasRank.get(String(receipt.alias));
    const requestedEffortRank = effortRank.get(String(receipt.requested_effort));
    const effectiveEffortRank = effortRank.get(String(receipt.effort));
    if (
      receipt.task_class !== request.taskClass || receipt.route_source !== "task-class" ||
      (receipt.status === "ok" && (
        policy === undefined || request.role !== policy.role ||
        receiptAliasRank === undefined || receiptAliasRank < aliasRank.get(policy.minimumAlias)! ||
        requestedEffortRank === undefined || requestedEffortRank < effortRank.get(policy.minimumEffort)! ||
        effectiveEffortRank === undefined || effectiveEffortRank < effortRank.get(policy.minimumEffort)!
      ))
    ) {
      return false;
    }
  } else if (receipt.alias !== request.alias || receipt.task_class !== undefined || receipt.route_source !== undefined) {
    return false;
  }
  if (request.effort !== undefined && receipt.requested_effort !== request.effort) return false;
  if (request.model !== undefined && receipt.requested_model !== request.model && receipt.resolved_model !== request.model) {
    return false;
  }
  if (receipt.status !== "ok") return true;
  if (
    receipt.lead_family !== request.leadFamily ||
    !nonEmptyString(receipt, "requested_effort") || !nonEmptyString(receipt, "effort") ||
    !nonEmptyString(receipt, "effort_capability_source") || !nonEmptyString(receipt, "endpoint_provider") ||
    !nonEmptyString(receipt, "model_family") || typeof receipt.resolved_model !== "string" ||
    !nonEmptyString(receipt, "identity_source")
  ) return false;
  const distinctFromLead = receipt.model_family !== request.leadFamily;
  if (receipt.distinct_from_lead !== distinctFromLead || (request.requireDistinct && !distinctFromLead)) return false;
  if (receipt.model_selection === "account-default") {
    return receipt.resolved_model === "" && nonEmptyString(receipt, "catalog_model");
  }
  return nonEmptyString(receipt, "resolved_model");
}

export async function resolveModelRouteReceipt(input: {
  routerPath: string;
  receiptPath: string;
  /** Test-only producer seam. Production always uses the repository-owned producer. */
  testClaudeCapabilitiesPath?: string;
  request: ModelRouteRequest;
}): Promise<{
  receipt: Record<string, unknown>;
  invocation: { executable: string; arguments: string[] };
}> {
  if ((input.request.alias === undefined) === (input.request.taskClass === undefined)) {
    throw new TypeError("model route requires exactly one of alias or taskClass");
  }
  if (input.request.taskClass !== undefined && input.request.model !== undefined) {
    throw new TypeError("task-class model route does not accept an explicit model");
  }
  if (
    input.request.adapter === "claude" &&
    input.request.taskClass !== undefined &&
    taskClassPolicy.has(input.request.taskClass) &&
    input.request.capabilitiesFile !== undefined
  ) {
    throw new TypeError("Claude task-class routing requires a wrapper-produced subscription canary");
  }
  if (input.testClaudeCapabilitiesPath !== undefined && process.env.NODE_ENV !== "test") {
    throw new TypeError("Claude capability producer override is test-only");
  }
  let capabilitiesFile = input.request.capabilitiesFile;
  if (input.request.adapter === "claude" && input.request.taskClass !== undefined && capabilitiesFile === undefined) {
    const policy = taskClassPolicy.get(input.request.taskClass);
    if (policy !== undefined) {
      const cataloguePath = resolve(dirname(input.routerPath), "../config/model-routing.json");
      let catalogue: unknown;
      try {
        catalogue = JSON.parse(await readFile(cataloguePath, "utf8"));
      } catch (error: unknown) {
        throw new TypeError("Claude capability probe requires a readable model routing catalogue", { cause: error });
      }
      const adapters = isRecord(catalogue) && isRecord(catalogue.adapters) ? catalogue.adapters : undefined;
      const adapter = adapters?.[input.request.adapter];
      const fixedFamily = isRecord(adapter) ? adapter.fixed_model_family : undefined;
      const taskClassRoutes = isRecord(catalogue) && isRecord(catalogue.task_class_routes)
        ? catalogue.task_class_routes
        : undefined;
      const route = taskClassRoutes?.[input.request.taskClass];
      const routeAlias = isRecord(route) ? route.alias : undefined;
      const families = isRecord(catalogue) && isRecord(catalogue.families) ? catalogue.families : undefined;
      const family = typeof fixedFamily === "string" ? families?.[fixedFamily] : undefined;
      const aliases = isRecord(family) && isRecord(family.aliases) ? family.aliases : undefined;
      if (
        typeof fixedFamily !== "string" || fixedFamily.trim().length === 0 ||
        !isRecord(route) || typeof routeAlias !== "string" || routeAlias.trim().length === 0 ||
        !isRecord(family) || aliases === undefined
      ) {
        throw new TypeError("model routing catalogue does not provide a valid Claude capability probe alias");
      }
      const candidates = typeof routeAlias === "string" ? aliases?.[routeAlias] : undefined;
      const configuredProbeAlias = route.capability_probe_alias;
      let probeAlias: unknown;
      if (configuredProbeAlias !== undefined) {
        const knownFamilyAlias = typeof configuredProbeAlias === "string" &&
          configuredProbeAlias.trim().length > 0 &&
          aliases !== undefined &&
          Object.values(aliases).some(
            (aliasCandidates) => Array.isArray(aliasCandidates) && aliasCandidates.includes(configuredProbeAlias),
          );
        if (!knownFamilyAlias) {
          throw new TypeError("model routing catalogue does not provide a valid Claude capability probe alias");
        }
        probeAlias = configuredProbeAlias;
      } else {
        probeAlias = Array.isArray(candidates) ? candidates[0] : undefined;
      }
      if (typeof probeAlias !== "string" || probeAlias.trim().length === 0) {
        throw new TypeError("model routing catalogue does not provide a valid Claude capability probe alias");
      }
      capabilitiesFile = `${input.receiptPath}.claude-capabilities.json`;
      const producerPath = input.testClaudeCapabilitiesPath ?? resolve(
        dirname(input.routerPath), "../skills/orchestrate/scripts/claude_capabilities.py",
      );
      await execFileAsync(producerPath, [
        "--out", capabilitiesFile,
        "--alias", probeAlias,
        "--effort", policy.minimumEffort,
      ], { encoding: "utf8", timeout: 40_000, maxBuffer: 1024 * 1024 });
    }
  }
  const argumentsList = [
    "resolve",
    "--adapter",
    input.request.adapter,
    ...(input.request.taskClass === undefined
      ? ["--alias", input.request.alias]
      : ["--task-class", input.request.taskClass]),
    "--role",
    input.request.role,
    ...(input.request.effort === undefined ? [] : ["--effort", input.request.effort]),
    ...(capabilitiesFile === undefined ? [] : ["--capabilities-file", capabilitiesFile]),
    ...(input.request.model === undefined ? [] : ["--model", input.request.model]),
    "--lead-family",
    input.request.leadFamily,
    ...(input.request.requireDistinct ? ["--require-distinct"] : []),
  ];
  const invocation = { executable: input.routerPath, arguments: argumentsList };
  let stdout: string;
  try {
    ({ stdout } = await execFileAsync(input.routerPath, argumentsList, {
      encoding: "utf8",
      timeout: 50_000,
      maxBuffer: 1024 * 1024,
    }));
  } catch (error: unknown) {
    if (!isRecord(error) || typeof error.stdout !== "string") throw error;
    stdout = error.stdout;
  }
  const parsed: unknown = JSON.parse(stdout);
  if (!isRecord(parsed) || !isValidReceipt(parsed, input.request)) {
    throw new TypeError("model router returned an invalid receipt");
  }
  await writeFile(input.receiptPath, `${JSON.stringify(parsed, null, 2)}\n`, { mode: 0o600 });
  if (parsed.status !== "ok") throw new ModelRouteRejectedError(parsed, invocation);
  return { receipt: parsed, invocation };
}
