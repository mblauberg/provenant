import { execFile } from "node:child_process";
import { access, constants, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
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

async function isExecutable(path: string): Promise<boolean> {
  try {
    await access(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

async function findProjectPython(routerPath: string): Promise<string | undefined> {
  const roots = new Set([resolve(dirname(routerPath)), resolve(process.cwd())]);
  for (const root of roots) {
    let current = root;
    while (true) {
      const candidate = join(current, ".venv", "bin", "python");
      if (await isExecutable(candidate)) return candidate;
      const parent = dirname(current);
      if (parent === current) break;
      current = parent;
    }
  }
  return undefined;
}

async function resolveRouterScriptPath(routerPath: string): Promise<string> {
  try {
    const source = await readFile(routerPath, "utf8");
    if (source.startsWith("#!/usr/bin/env bash") && source.includes("model_route.py")) {
      const pythonRouterPath = join(dirname(routerPath), "model_route.py");
      if (await isExecutable(pythonRouterPath)) return pythonRouterPath;
    }
  } catch {
    // Let the interpreter report a missing or unreadable router path.
  }
  return routerPath;
}

async function resolveRouterInterpreter(routerPath: string): Promise<string> {
  const harnessPython = process.env.HARNESS_PYTHON;
  if (harnessPython !== undefined && harnessPython.length > 0 && await isExecutable(harnessPython)) {
    return harnessPython;
  }
  const projectPython = await findProjectPython(routerPath);
  if (projectPython !== undefined) return projectPython;
  try {
    await execFileAsync("python3", ["-c", "import pytest, yaml"], {
      encoding: "utf8",
      timeout: 10_000,
      maxBuffer: 1024 * 1024,
    });
    return "python3";
  } catch {
    throw new Error(
      "model router requires Python with pytest and PyYAML; set HARNESS_PYTHON or install the repository .venv",
    );
  }
}

function describeRouterFailure(error: unknown): string {
  if (!isRecord(error)) return "exit status unavailable; stderr unavailable";
  const exitCode = typeof error.code === "number" || typeof error.code === "string"
    ? `exit code ${String(error.code)}`
    : "exit status unavailable";
  const signal = typeof error.signal === "string" ? `, signal ${error.signal}` : "";
  const stderr = typeof error.stderr === "string" && error.stderr.trim().length > 0
    ? error.stderr.trimEnd()
    : "stderr unavailable";
  return `${exitCode}${signal}; stderr: ${stderr}`;
}

function parseRouterOutput(stdout: string, operation: string, failure: unknown | undefined): unknown {
  try {
    return JSON.parse(stdout);
  } catch (error: unknown) {
    if (failure !== undefined) {
      throw new Error(`${operation} failed (${describeRouterFailure(failure)})`, { cause: error });
    }
    throw error;
  }
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
  const routerScriptPath = await resolveRouterScriptPath(input.routerPath);
  const interpreter = await resolveRouterInterpreter(routerScriptPath);
  const invocation = { executable: interpreter, arguments: [routerScriptPath, ...argumentsList] };
  let stdout: string;
  let failure: unknown;
  try {
    ({ stdout } = await execFileAsync(interpreter, invocation.arguments, {
      encoding: "utf8",
      timeout: 50_000,
      maxBuffer: 1024 * 1024,
    }));
  } catch (error: unknown) {
    failure = error;
    stdout = isRecord(error) && typeof error.stdout === "string" ? error.stdout : "";
  }
  const parsed: unknown = parseRouterOutput(stdout, "model router", failure);
  if (!isRecord(parsed) || !isValidReceipt(parsed, input.request)) {
    throw new TypeError("model router returned an invalid receipt");
  }
  await writeFile(input.receiptPath, `${JSON.stringify(parsed, null, 2)}\n`, { mode: 0o600 });
  if (parsed.status !== "ok") throw new ModelRouteRejectedError(parsed, invocation);
  return { receipt: parsed, invocation };
}

type AvailabilityFact =
  | { observation: "Observed"; value: "available" | "unavailable" }
  | { observation: "Unobserved" }
  | { observation: "Unknown"; reason: string };

type PreferredRouteCandidate = {
  candidateId: string;
  request: ModelRouteRequest;
  availability: AvailabilityFact;
};

function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (isRecord(value)) {
    return `{${Object.keys(value).sort().map(
      (key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`,
    ).join(",")}}`;
  }
  return JSON.stringify(value) ?? "undefined";
}

function isAvailabilityFact(value: unknown): value is AvailabilityFact {
  if (!isRecord(value)) return false;
  if (value.observation === "Unobserved") return true;
  if (value.observation === "Unknown") return nonEmptyString(value, "reason");
  return value.observation === "Observed" &&
    (value.value === "available" || value.value === "unavailable");
}

function isSpreadState(value: unknown): value is {
  schema_version: 1;
  assignments: Record<string, number>;
  selection_count: number;
} {
  if (
    !isRecord(value) ||
    value.schema_version !== 1 ||
    !isRecord(value.assignments) ||
    !Number.isSafeInteger(value.selection_count) ||
    Number(value.selection_count) < 0
  ) return false;
  const counts: unknown[] = Object.values(value.assignments);
  return counts.every(
    (count) => Number.isSafeInteger(count) && Number(count) >= 0,
  ) && counts.reduce<number>(
    (total, count) => total + Number(count),
    0,
  ) === value.selection_count;
}

function isValidSelectionReceipt(
  receipt: Record<string, unknown>,
  taskClass: string,
  role: string,
  hardCandidates: Array<{
    candidateId: string;
    route: Record<string, unknown>;
    availability: AvailabilityFact;
  }>,
): boolean {
  const policy = taskClassPolicy.get(taskClass);
  if (
    policy === undefined ||
    receipt.schema_version !== 1 ||
    receipt.status !== "ok" ||
    receipt.task_class !== taskClass ||
    receipt.role !== role ||
    receipt.selection_source !== "hard-admissible-preference" ||
    !nonEmptyString(receipt, "chosen_candidate_id") ||
    !isRecord(receipt.chosen_route) ||
    !Array.isArray(receipt.candidates) ||
    !isRecord(receipt.preference) ||
    !Array.isArray(receipt.preference.applied) ||
    !Array.isArray(receipt.preference.not_honoured) ||
    !Array.isArray(receipt.preference.ignored) ||
    !isRecord(receipt.spreading) ||
    receipt.spreading.policy !== "fair-round-robin" ||
    !isSpreadState(receipt.spreading.state_before) ||
    !isSpreadState(receipt.spreading.state_after)
  ) return false;
  const chosen = receipt.chosen_route;
  const chosenAlias = aliasRank.get(String(chosen.alias));
  const chosenRequestedEffort = effortRank.get(String(chosen.requested_effort));
  const chosenEffort = effortRank.get(String(chosen.effort));
  if (
    chosen.schema_version !== 1 ||
    chosen.status !== "ok" ||
    chosen.task_class !== taskClass ||
    chosen.route_source !== "task-class" ||
    chosen.role !== role ||
    role !== policy.role ||
    chosenAlias === undefined ||
    chosenAlias < aliasRank.get(policy.minimumAlias)! ||
    chosenRequestedEffort === undefined ||
    chosenRequestedEffort < effortRank.get(policy.minimumEffort)! ||
    chosenEffort === undefined ||
    chosenEffort < effortRank.get(policy.minimumEffort)!
  ) return false;
  const receiptCandidates = receipt.candidates as unknown[];
  const selected = receiptCandidates.filter(
    (candidate: unknown): candidate is Record<string, unknown> =>
      isRecord(candidate) && candidate.selected === true,
  );
  const hardCandidate = hardCandidates.find(
    (candidate) => candidate.candidateId === receipt.chosen_candidate_id,
  );
  const candidatesMatch = receiptCandidates.length === hardCandidates.length &&
    hardCandidates.every((hardCandidate) => {
      const candidate = receiptCandidates.find(
        (value: unknown) => isRecord(value) && value.candidate_id === hardCandidate.candidateId,
      );
      if (
        !isRecord(candidate) ||
        !isAvailabilityFact(candidate.availability) ||
        canonicalJson(candidate.route) !== canonicalJson(hardCandidate.route) ||
        canonicalJson(candidate.availability) !== canonicalJson(hardCandidate.availability)
      ) return false;
      const observedUnavailable = candidate.availability.observation === "Observed" &&
        candidate.availability.value === "unavailable";
      const candidateAlias = aliasRank.get(String(hardCandidate.route.alias));
      const candidateRequestedEffort = effortRank.get(String(hardCandidate.route.requested_effort));
      const candidateEffort = effortRank.get(String(hardCandidate.route.effort));
      const expectedAdmissible = hardCandidate.route.schema_version === 1 &&
        hardCandidate.route.status === "ok" &&
        hardCandidate.route.task_class === taskClass &&
        hardCandidate.route.route_source === "task-class" &&
        hardCandidate.route.role === role &&
        role === policy.role &&
        candidateAlias !== undefined &&
        candidateAlias >= aliasRank.get(policy.minimumAlias)! &&
        candidateRequestedEffort !== undefined &&
        candidateRequestedEffort >= effortRank.get(policy.minimumEffort)! &&
        candidateEffort !== undefined &&
        candidateEffort >= effortRank.get(policy.minimumEffort)! &&
        !observedUnavailable;
      return candidate.admissible === expectedAdmissible &&
        typeof candidate.disposition === "string";
    });
  const before = receipt.spreading.state_before;
  const after = receipt.spreading.state_after;
  const chosenFamily = String(chosen.model_family);
  const familyKeys = new Set([
    ...Object.keys(before.assignments),
    ...Object.keys(after.assignments),
  ]);
  const stateTransition = after.selection_count === before.selection_count + 1 &&
    [...familyKeys].every((family) =>
      Number(after.assignments[family] ?? 0) ===
      Number(before.assignments[family] ?? 0) + (family === chosenFamily ? 1 : 0)
    );
  const admissibleFamilies = new Set(receiptCandidates.flatMap((candidate) =>
    isRecord(candidate) &&
    candidate.admissible === true &&
    typeof candidate.model_family === "string"
      ? [candidate.model_family]
      : []
  ));
  const minimumCount = Math.min(
    ...[...admissibleFamilies].map((family) => Number(before.assignments[family] ?? 0)),
  );
  const fairFamily = admissibleFamilies.has(chosenFamily) &&
    Number(before.assignments[chosenFamily] ?? 0) === minimumCount;
  return candidatesMatch &&
    stateTransition &&
    fairFamily &&
    chosenFamily.length > 0 &&
    selected.length === 1 &&
    hardCandidate !== undefined &&
    selected[0]?.candidate_id === receipt.chosen_candidate_id &&
    selected[0]?.admissible === true &&
    selected[0]?.disposition === "selected" &&
    canonicalJson(selected[0]?.route) === canonicalJson(chosen) &&
    canonicalJson(hardCandidate.route) === canonicalJson(chosen);
}

export async function selectPreferredModelRouteReceipt(input: {
  routerPath: string;
  receiptPath: string;
  preferencesPath: string;
  spreadStatePath: string;
  taskClass: string;
  role: string;
  candidates: PreferredRouteCandidate[];
}): Promise<{
  receipt: Record<string, unknown>;
  invocation: { executable: string; arguments: string[] };
}> {
  const candidatesPath = `${input.receiptPath}.candidates.json`;
  const hardCandidates = await Promise.all(input.candidates.map(async (candidate, index) => {
    const hardReceiptPath = `${input.receiptPath}.hard-${index}.json`;
    try {
      try {
        const result = await resolveModelRouteReceipt({
          routerPath: input.routerPath,
          receiptPath: hardReceiptPath,
          request: candidate.request,
        });
        return {
          candidateId: candidate.candidateId,
          route: result.receipt,
          availability: candidate.availability,
        };
      } catch (error: unknown) {
        if (!(error instanceof ModelRouteRejectedError)) throw error;
        return {
          candidateId: candidate.candidateId,
          route: error.receipt,
          availability: candidate.availability,
        };
      }
    } finally {
      await Promise.all([
        rm(hardReceiptPath, { force: true }),
        rm(`${hardReceiptPath}.claude-capabilities.json`, { force: true }),
      ]);
    }
  }));
  await writeFile(candidatesPath, `${JSON.stringify({
    schema_version: 1,
    candidates: hardCandidates.map((candidate) => ({
      candidate_id: candidate.candidateId,
      route: candidate.route,
      availability: candidate.availability,
    })),
  }, null, 2)}\n`, { mode: 0o600 });
  const argumentsList = [
    "select",
    "--task-class", input.taskClass,
    "--role", input.role,
    "--candidates-file", candidatesPath,
    "--preferences-file", input.preferencesPath,
    "--spread-state-file", input.spreadStatePath,
  ];
  const routerScriptPath = await resolveRouterScriptPath(input.routerPath);
  const interpreter = await resolveRouterInterpreter(routerScriptPath);
  const invocation = { executable: interpreter, arguments: [routerScriptPath, ...argumentsList] };
  let stdout: string;
  let failure: unknown;
  try {
    ({ stdout } = await execFileAsync(interpreter, invocation.arguments, {
      encoding: "utf8",
      timeout: 50_000,
      maxBuffer: 4 * 1024 * 1024,
    }));
  } catch (error: unknown) {
    failure = error;
    stdout = isRecord(error) && typeof error.stdout === "string" ? error.stdout : "";
  } finally {
    await rm(candidatesPath, { force: true });
  }
  const parsed: unknown = parseRouterOutput(stdout, "model preference selector", failure);
  if (
    !isRecord(parsed) ||
    parsed.schema_version !== 1 ||
    typeof parsed.status !== "string" ||
    parsed.task_class !== input.taskClass ||
    parsed.role !== input.role
  ) {
    throw new TypeError("model preference selector returned an invalid receipt");
  }
  if (parsed.status !== "ok") {
    await writeFile(input.receiptPath, `${JSON.stringify(parsed, null, 2)}\n`, { mode: 0o600 });
    throw new ModelRouteRejectedError(parsed, invocation);
  }
  if (!isValidSelectionReceipt(parsed, input.taskClass, input.role, hardCandidates)) {
    throw new TypeError("model preference selector returned an invalid receipt");
  }
  await writeFile(input.receiptPath, `${JSON.stringify(parsed, null, 2)}\n`, { mode: 0o600 });
  return { receipt: parsed, invocation };
}
