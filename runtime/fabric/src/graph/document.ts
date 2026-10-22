import { pythonOwnerSync, pythonOwnerEnvironment } from "../python-owner.js";
/**
 * Graph documents (slice 1): plain, already expanded work nodes. Validation is
 * deterministic over the document, the catalogue and the filesystem. The
 * store freezes the checked plan so owner restarts never resolve it again.
 */
import { execFileSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { realpathSync } from "node:fs";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import type { CatalogueSnapshot } from "../catalogue.js";
import { InputError, normaliseRoute, ownerPromptPath, readRoots, workingIdentity, type RouteInput } from "../execution-input.js";
import { productRoot } from "../execution.js";
import { projectRoot, withoutGitRedirects, type Identity } from "../identity.js";

export const GRAPH_ID = /^g-[0-9a-f]{6,16}$/u;
const NODE_ID = /^[a-z0-9][a-z0-9._-]{0,47}$/u;
const TIERS = ["junior", "senior", "principal"] as const;
const MODES = ["read_only", "worktree_write"] as const;
const CAPABILITIES = ["postgres", "browser"];
const DEFAULT_GATE_TIMEOUT = 600;

const DOCUMENT_FIELDS = ["schema_version", "graph_id", "revision", "origin", "limits", "authority", "worktrees", "nodes"];
const LIMIT_FIELDS = ["max_width", "max_nodes", "max_depth", "gate_allowlist"];
const AUTHORITY_FIELDS = ["mode", "network", "confidential", "allow_secrets", "capabilities", "read_roots"];
const ROUTE_FIELDS = ["adapter", "alias", "model", "effort"];
const NODE_FIELDS = ["id", "kind", "role", "tier", "route", "execution", "authority", "worktree", "cwd", "brief",
  "inputs", "depends_on", "needs", "gate", "on_fail", "context_budget", "ui", "size", "risk", "timeout_seconds"];
/** Fields a later slice owns: named, so a document that relies on them is refused rather than half run. */
const LATER: Record<string, string> = {
  execution: "headless execution arrives in slice 5", context_budget: "context budgets arrive in slice 6",
  ui: "UI evidence arrives in slice 6",
};

export interface Authority {
  mode?: (typeof MODES)[number];
  network?: boolean;
  confidential?: boolean;
  allow_secrets?: boolean;
  capabilities?: string[];
  read_roots?: string[];
}

export interface GraphNode {
  id: string;
  kind?: "work";
  role?: string;
  tier?: (typeof TIERS)[number];
  route: { adapter: string; alias?: string; model?: string; effort?: string };
  authority?: Authority;
  worktree?: string;
  cwd?: string;
  brief: { prompt?: string; prompt_file?: string };
  inputs?: Array<{ from: string; part: "result" }>;
  depends_on?: string[];
  needs?: "passed";
  gate?: { fast?: string[][]; timeout_seconds?: number };
  on_fail?: "stop";
  size?: string;
  risk?: string;
  timeout_seconds?: number;
}

export interface GraphDocument {
  schema_version: 1;
  graph_id: string;
  revision: number;
  origin?: Record<string, unknown>;
  limits?: { max_width?: number; max_nodes?: number; max_depth?: number; gate_allowlist?: string[][] };
  authority?: Authority;
  worktrees?: Record<string, { path: string }>;
  nodes: GraphNode[];
}

/** One rejected rule: printed as `node X: error · fix: ...`. */
export interface Fault { node?: string; error: string; fix: string }

/** A node after defaults, inheritance and resolution: what the runner dispatches and hashes. */
export interface PlannedNode {
  id: string;
  tier: (typeof TIERS)[number];
  role?: string;
  route: { adapter: string; alias?: string; model?: string; effort?: string };
  /** The capacity pool; `account` stays `default` until per-account ledgers exist. */
  pool: { adapter: string; model: string; account: string };
  authority: Required<Pick<Authority, "mode" | "confidential" | "allow_secrets">> & Omit<Authority, "mode" | "confidential" | "allow_secrets">;
  cwd?: string;
  worktree?: { key: string; path: string; gitDir: string };
  brief: { prompt?: string; prompt_file?: string; sha256: string };
  inputs: Array<{ from: string; part: "result" }>;
  depends_on: string[];
  gates: string[][];
  gate_timeout_seconds: number;
  timeout_seconds?: number;
  spec_hash: string;
}

export interface GraphPlan {
  graph_id: string;
  revision: number;
  max_width: number;
  nodes: PlannedNode[];
  /** Longest path to a sink, for critical-path-first ordering. */
  height: Record<string, number>;
}

const object = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);
const strings = (value: unknown): value is string[] => Array.isArray(value) && value.every((item) => typeof item === "string");
const argv = (value: unknown): value is string[] => strings(value) && value.length > 0 && value.every((item) => item.length > 0);
const integer = (value: unknown, low: number, high: number): boolean =>
  typeof value === "number" && Number.isInteger(value) && value >= low && value <= high;

function canonical(path: string): string {
  try { return realpathSync.native(path); } catch { return resolve(path); }
}

function inside(root: string, candidate: string): boolean {
  const path = relative(root, candidate);
  return path === "" || (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`));
}

/** Canonical JSON: sorted keys, no whitespace, so equal specs hash equally on every host. */
export function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (object(value))
    return `{${Object.keys(value).filter((key) => value[key] !== undefined).sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}

const sha256 = (text: string | Buffer) => createHash("sha256").update(text).digest("hex");

/** Read the original prompt through dispatch's path checks and inode-safe reader, before copying it. */
export function readGraphPromptFile(path: string, identity: Identity, env: NodeJS.ProcessEnv = process.env): Buffer {
  const scripts = fileURLToPath(new URL("../../../../skills/orchestrate/scripts/", import.meta.url));
  const root = productRoot(env);
  const python = pythonOwnerSync(root, env);
  const script = `import base64,json,sys
from pathlib import Path
sys.path.insert(0,sys.argv[1])
from dispatch_run import read_prompt_input,PreflightError
try:
 data=read_prompt_input(Path(sys.argv[2]),Path(sys.argv[3]),Path(sys.argv[3]),json.loads(sys.argv[4]))
 print(json.dumps({"prompt":base64.b64encode(data).decode()}))
except PreflightError as error:
 print(json.dumps({"error":error.code,"fix":str(error)}))
`;
  const result = JSON.parse(execFileSync(python, ["-I", "-c", script, scripts, ownerPromptPath(identity, path), identity.cwd,
    JSON.stringify(readRoots(identity, undefined, path))], { cwd: root, env: pythonOwnerEnvironment(env), encoding: "utf8", timeout: 10_000,
    maxBuffer: 16 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] })) as { prompt: string; error?: string; fix?: string };
  if (result.error) throw new InputError(result.error, result.fix!);
  return Buffer.from(result.prompt, "base64");
}

/** The dispatch identity of one attempt: never the node id itself, so any node id fits dispatch's task id rule. */
export function attemptTaskId(key: { project: string; graph_id: string; node_id: string; revision: number; attempt: number; restart: number }): string {
  const digest = sha256(canonicalJson({ project: key.project, graph_id: key.graph_id, node_id: key.node_id,
    revision: key.revision, attempt: key.attempt, restart: key.restart }));
  return `${key.graph_id}.${digest.slice(0, 24)}`;
}

/** A fresh graph id for a document that does not name one. */
export function newGraphId(): string {
  return `g-${randomBytes(6).toString("hex")}`;
}

function git(path: string, args: string[]): string | undefined {
  try {
    return execFileSync("git", ["-C", path, ...args], { encoding: "utf8", env: withoutGitRedirects(process.env),
      stdio: ["ignore", "pipe", "ignore"], timeout: 10_000 }).trim();
  } catch { return undefined; }
}

/** A registered linked worktree root in one of the seat's projects, or why not. */
export function linkedWorktree(path: string, identity: Identity): { path: string; gitDir: string } | Fault {
  const fix = "Pass the root of a linked worktree (`provenant worktree`), never the primary checkout.";
  let real: string;
  try { real = realpathSync.native(path); } catch { return { error: "worktree_unavailable", fix: `${path} does not exist. ${fix}` }; }
  const [top, gitDir, common] = (git(real, ["rev-parse", "--path-format=absolute", "--show-toplevel", "--absolute-git-dir", "--git-common-dir"]) ?? "").split("\n");
  if (!top || !gitDir || !common) return { error: "worktree_not_git", fix: `${real} is not a Git worktree. ${fix}` };
  if (canonical(top) !== real) return { error: "worktree_not_root", fix: `${real} is inside ${top}. ${fix}` };
  if (canonical(gitDir) === canonical(common)) return { error: "worktree_primary", fix: `${real} is a primary checkout. ${fix}` };
  const projects = [identity.project, ...(identity.registeredProjects ?? [])].map(canonical);
  if (!projects.includes(canonical(projectRoot(real))))
    return { error: "worktree_unregistered", fix: `${real} belongs to no registered Fabric project; run \`fabric whoami\` in its repository.` };
  return { path: real, gitDir: canonical(gitDir) };
}

function unknownFields(value: Record<string, unknown>, allowed: string[], where: string, node?: string): Fault[] {
  return Object.keys(value).filter((key) => !allowed.includes(key)).map((key) => ({
    ...(node === undefined ? {} : { node }), error: "field_unknown", fix: `Remove ${where}${key}; slice 1 accepts ${allowed.join(", ")}.`,
  }));
}

function authorityFaults(value: unknown, where: string, node?: string): Fault[] {
  if (value === undefined) return [];
  const at = node === undefined ? {} : { node };
  if (!object(value)) return [{ ...at, error: "authority_invalid", fix: `Pass ${where} as an object.` }];
  const faults = unknownFields(value, AUTHORITY_FIELDS, `${where}.`, node);
  if (value.mode !== undefined && !MODES.includes(value.mode as never))
    faults.push({ ...at, error: "mode_invalid", fix: `Pass ${where}.mode read_only or worktree_write.` });
  for (const key of ["network", "confidential", "allow_secrets"])
    if (value[key] !== undefined && typeof value[key] !== "boolean")
      faults.push({ ...at, error: "authority_invalid", fix: `Pass ${where}.${key} as true or false.` });
  if (value.capabilities !== undefined && (!strings(value.capabilities) || value.capabilities.some((item) => !CAPABILITIES.includes(item))))
    faults.push({ ...at, error: "capabilities_invalid", fix: `Pass ${where}.capabilities from ${CAPABILITIES.join(", ")}.` });
  if (value.read_roots !== undefined && (!strings(value.read_roots) || value.read_roots.some((root) => !isAbsolute(root))))
    faults.push({ ...at, error: "read_roots_invalid", fix: `Pass ${where}.read_roots as absolute paths.` });
  return faults;
}

/**
 * Validate a submitted document. Every fault is reported, one per rule broken;
 * a plan is returned only when there are none.
 */
export function validateGraph(input: unknown, identity: Identity, catalogue: CatalogueSnapshot,
  env: NodeJS.ProcessEnv = process.env): { faults: Fault[]; plan?: GraphPlan } {
  if (!object(input)) return { faults: [{ error: "document_invalid", fix: "Pass one JSON object with schema_version 1." }] };
  const faults: Fault[] = unknownFields(input, DOCUMENT_FIELDS, "");
  if (input.schema_version !== 1) faults.push({ error: "schema_version_invalid", fix: "Set schema_version to 1." });
  if (typeof input.graph_id !== "string" || !GRAPH_ID.test(input.graph_id))
    faults.push({ error: "graph_id_invalid", fix: "Set graph_id to g- plus 6 to 16 lower-case hex digits, or omit it." });
  if (input.revision !== 1) faults.push({ error: "revision_invalid", fix: "Submit revision 1; revisions arrive in slice 3." });
  if (input.origin !== undefined && !object(input.origin)) faults.push({ error: "origin_invalid", fix: "Pass origin as an object." });
  const limits = input.limits ?? {};
  if (!object(limits)) faults.push({ error: "limits_invalid", fix: "Pass limits as an object." });
  const limitValues = object(limits) ? limits : {};
  if (object(limits)) faults.push(...unknownFields(limits, LIMIT_FIELDS, "limits."));
  const maxNodes = limitValues.max_nodes ?? 64, maxWidth = limitValues.max_width ?? 6, maxDepth = limitValues.max_depth ?? 3;
  if (!integer(maxNodes, 1, 256)) faults.push({ error: "max_nodes_invalid", fix: "Set limits.max_nodes from 1 to 256." });
  if (!integer(maxWidth, 1, 8)) faults.push({ error: "max_width_invalid", fix: "Set limits.max_width from 1 to 8." });
  if (!integer(maxDepth, 1, 3)) faults.push({ error: "max_depth_invalid", fix: "Set limits.max_depth from 1 to 3." });
  const allowlist = limitValues.gate_allowlist ?? [];
  if (!Array.isArray(allowlist) || !allowlist.every(argv))
    faults.push({ error: "gate_allowlist_invalid", fix: "Pass limits.gate_allowlist as a list of non-empty argv arrays." });
  const allowed = new Set((Array.isArray(allowlist) ? allowlist.filter(argv) : []).map((entry) => canonicalJson(entry)));

  faults.push(...authorityFaults(input.authority, "authority"));
  const root: Authority = object(input.authority) ? input.authority as Authority : {};
  for (const path of strings(root.read_roots) ? root.read_roots : []) {
    try { workingIdentity({ cwd: path }, identity); }
    catch (error) {
      if (error instanceof InputError) faults.push({ error: "authority_exceeds_seat", fix: `authority.read_roots ${path}: ${error.fix}` });
      else throw error;
    }
  }

  const worktrees = input.worktrees ?? {};
  const resolvedWorktrees = new Map<string, { path: string; gitDir: string }>();
  if (!object(worktrees)) faults.push({ error: "worktrees_invalid", fix: "Pass worktrees as {key: {path}}." });
  else for (const [key, entry] of Object.entries(worktrees)) {
    if (!object(entry) || typeof entry.path !== "string" || !isAbsolute(entry.path) || Object.keys(entry).some((field) => field !== "path")) {
      faults.push({ error: "worktree_invalid", fix: `Pass worktrees.${key} as {path: <absolute linked worktree root>}.` });
      continue;
    }
    const resolved = linkedWorktree(entry.path, identity);
    if ("error" in resolved) faults.push({ ...resolved, fix: `worktrees.${key}: ${resolved.fix}` });
    else resolvedWorktrees.set(key, resolved);
  }

  const nodes = Array.isArray(input.nodes) ? input.nodes : [];
  if (!Array.isArray(input.nodes) || nodes.length === 0) faults.push({ error: "nodes_invalid", fix: "Pass at least one node." });
  if (integer(maxNodes, 1, 256) && nodes.length > (maxNodes as number))
    faults.push({ error: "max_nodes_exceeded", fix: `Split the graph: ${nodes.length} nodes exceed limits.max_nodes ${maxNodes}.` });
  const ids = new Set<string>();
  for (const node of nodes) {
    if (!object(node) || typeof node.id !== "string" || !NODE_ID.test(node.id)) {
      faults.push({ error: "node_id_invalid", fix: "Give every node an id matching [a-z0-9][a-z0-9._-]{0,47}." });
      continue;
    }
    if (ids.has(node.id)) faults.push({ node: node.id, error: "node_id_duplicate", fix: "Give every node a unique id." });
    ids.add(node.id);
  }

  const planned: PlannedNode[] = [];
  const parents = new Map<string, string[]>();
  for (const raw of nodes) {
    if (!object(raw) || typeof raw.id !== "string" || !NODE_ID.test(raw.id)) continue;
    const id = raw.id;
    const at = { node: id };
    const nodeFaults: Fault[] = unknownFields(raw, NODE_FIELDS, "", id);
    for (const [field, reason] of Object.entries(LATER))
      if (raw[field] !== undefined) nodeFaults.push({ ...at, error: "slice_unsupported", fix: `Remove ${field}: ${reason}.` });
    if (raw.kind !== undefined && raw.kind !== "work")
      nodeFaults.push({ ...at, error: "slice_unsupported", fix: "Use kind work; review and lead nodes arrive in slices 2 and 5." });
    if (raw.needs !== undefined && raw.needs !== "passed")
      nodeFaults.push({ ...at, error: "slice_unsupported", fix: "Use needs passed; needs fast arrives in slice 2." });
    if (raw.on_fail !== undefined && raw.on_fail !== "stop")
      nodeFaults.push({ ...at, error: "slice_unsupported", fix: "Use on_fail stop; repair and escalation arrive in slice 2." });
    if (raw.tier !== undefined && !TIERS.includes(raw.tier as never))
      nodeFaults.push({ ...at, error: "tier_invalid", fix: `Pass tier ${TIERS.join(", ")}.` });
    for (const field of ["role", "size", "risk"])
      if (raw[field] !== undefined && typeof raw[field] !== "string") nodeFaults.push({ ...at, error: `${field}_invalid`, fix: `Pass ${field} as a string.` });
    if (raw.timeout_seconds !== undefined && !integer(raw.timeout_seconds, 1, 86_400))
      nodeFaults.push({ ...at, error: "timeout_invalid", fix: "Pass timeout_seconds from 1 to 86400." });

    const dependsOn = raw.depends_on ?? [];
    if (!strings(dependsOn)) nodeFaults.push({ ...at, error: "depends_on_invalid", fix: "Pass depends_on as a list of node ids." });
    const deps = strings(dependsOn) ? [...new Set(dependsOn)] : [];
    for (const parent of deps)
      if (!ids.has(parent)) nodeFaults.push({ ...at, error: "dependency_unknown", fix: `Remove ${parent} from depends_on or add that node.` });
    parents.set(id, deps.filter((parent) => ids.has(parent)));
    const inputs = raw.inputs ?? [];
    if (!Array.isArray(inputs) || !inputs.every((item) => object(item) && typeof item.from === "string" && Object.keys(item).every((key) => key === "from" || key === "part")))
      nodeFaults.push({ ...at, error: "inputs_invalid", fix: "Pass inputs as [{from: <node id>, part: \"result\"}]." });
    const parsedInputs = Array.isArray(inputs) ? inputs.filter(object) as Array<{ from: string; part?: unknown }> : [];
    for (const item of parsedInputs) {
      if (item.part !== undefined && item.part !== "result")
        nodeFaults.push({ ...at, error: "slice_unsupported", fix: `inputs from ${item.from}: use part result; diff and notes arrive in slices 2 and 6.` });
      if (!deps.includes(item.from)) nodeFaults.push({ ...at, error: "input_not_dependency", fix: `Add ${item.from} to depends_on.` });
    }

    const brief = raw.brief;
    let briefPlan: PlannedNode["brief"] | undefined;
    if (!object(brief) || Object.keys(brief).some((key) => key !== "prompt" && key !== "prompt_file") ||
        (typeof brief.prompt === "string") === (typeof brief.prompt_file === "string")) {
      nodeFaults.push({ ...at, error: "brief_invalid", fix: "Pass brief as {prompt} or {prompt_file}, exactly one." });
    } else if (typeof brief.prompt === "string") {
      if (!brief.prompt.trim()) nodeFaults.push({ ...at, error: "brief_invalid", fix: "Pass a non-empty brief.prompt." });
      briefPlan = { prompt: brief.prompt, sha256: sha256(brief.prompt) };
    } else {
      const path = resolve(identity.cwd, brief.prompt_file as string);
      try {
        briefPlan = { prompt_file: path, sha256: sha256(readGraphPromptFile(path, identity, env)) };
      } catch (error) {
        nodeFaults.push(error instanceof InputError ? { ...at, error: error.code, fix: error.fix }
          : { ...at, error: "brief_unavailable", fix: `Pass a readable regular brief.prompt_file (${path}).` });
      }
    }

    nodeFaults.push(...authorityFaults(raw.authority, "authority", id));
    const own: Authority = object(raw.authority) ? raw.authority as Authority : {};
    const mode = own.mode ?? "read_only";
    if (mode === "worktree_write" && root.mode !== "worktree_write")
      nodeFaults.push({ ...at, error: "authority_widened", fix: "Set the root authority.mode to worktree_write, or make this node read_only." });
    if (own.network === true && root.network !== true)
      nodeFaults.push({ ...at, error: "authority_widened", fix: "Remove network true, or allow network in the root authority." });
    if (own.allow_secrets === true && root.allow_secrets !== true)
      nodeFaults.push({ ...at, error: "authority_widened", fix: "Remove allow_secrets, or allow it in the root authority." });
    for (const capability of strings(own.capabilities) ? own.capabilities : [])
      if (!(root.capabilities ?? []).includes(capability))
        nodeFaults.push({ ...at, error: "authority_widened", fix: `Remove capability ${capability}, or grant it in the root authority.` });
    for (const path of strings(own.read_roots) ? own.read_roots : [])
      if (!(root.read_roots ?? []).some((parent) => inside(canonical(parent), canonical(path))))
        nodeFaults.push({ ...at, error: "authority_widened", fix: `Remove read root ${path}, or grant a containing root in the root authority.` });

    let worktree: PlannedNode["worktree"];
    if (mode === "worktree_write") {
      if (typeof raw.worktree !== "string") nodeFaults.push({ ...at, error: "worktree_required", fix: "Name a worktrees key for this writer." });
      else if (!object(worktrees) || !Object.hasOwn(worktrees, raw.worktree))
        nodeFaults.push({ ...at, error: "worktree_unknown", fix: `Add worktrees.${raw.worktree}, or name an existing key.` });
      else {
        const resolved = resolvedWorktrees.get(raw.worktree);
        if (resolved) worktree = { key: raw.worktree, ...resolved };
      }
      if (raw.cwd !== undefined) nodeFaults.push({ ...at, error: "cwd_not_applicable", fix: "Remove cwd: a writer works in its worktree." });
    } else if (raw.worktree !== undefined) {
      nodeFaults.push({ ...at, error: "worktree_not_applicable", fix: "Remove worktree, or set authority.mode worktree_write." });
    }
    let cwd: string | undefined;
    if (raw.cwd !== undefined && mode !== "worktree_write") {
      if (typeof raw.cwd !== "string") nodeFaults.push({ ...at, error: "cwd_invalid", fix: "Pass cwd as a path." });
      else try { cwd = workingIdentity({ cwd: raw.cwd, mode: "read_only" }, identity).cwd; }
      catch (error) {
        if (!(error instanceof InputError)) throw error;
        nodeFaults.push({ ...at, error: error.code, fix: error.fix });
      }
    }

    const gate = raw.gate;
    let gates: string[][] = [], gateTimeout = DEFAULT_GATE_TIMEOUT;
    if (gate !== undefined) {
      if (!object(gate)) nodeFaults.push({ ...at, error: "gate_invalid", fix: "Pass gate as {fast: [argv...], timeout_seconds}." });
      else {
        if (gate.slow !== undefined) nodeFaults.push({ ...at, error: "slice_unsupported", fix: "Remove gate.slow; slow gates arrive in slice 2." });
        nodeFaults.push(...unknownFields(gate, ["fast", "slow", "timeout_seconds"], "gate.", id));
        if (gate.timeout_seconds !== undefined && !integer(gate.timeout_seconds, 1, 7200))
          nodeFaults.push({ ...at, error: "gate_timeout_invalid", fix: "Pass gate.timeout_seconds from 1 to 7200." });
        else if (gate.timeout_seconds !== undefined) gateTimeout = gate.timeout_seconds as number;
        if (gate.fast !== undefined && !(Array.isArray(gate.fast) && gate.fast.every(argv)))
          nodeFaults.push({ ...at, error: "gate_invalid", fix: "Pass gate.fast as a list of argv arrays." });
        else gates = (gate.fast as string[][] | undefined) ?? [];
        for (const command of gates)
          if (!allowed.has(canonicalJson(command)))
            nodeFaults.push({ ...at, error: "gate_not_allowed", fix: `Add ${JSON.stringify(command)} to limits.gate_allowlist, or remove it from the gate.` });
      }
    }

    const route = raw.route;
    let plannedRoute: PlannedNode["route"] | undefined, pool: PlannedNode["pool"] | undefined;
    if (!object(route) || typeof route.adapter !== "string" || (route.model === undefined && route.alias === undefined)) {
      nodeFaults.push({ ...at, error: "route_required", fix: "Pass route {adapter, model or alias[, effort]}; slice 1 needs an explicit route." });
    } else {
      nodeFaults.push(...unknownFields(route, ROUTE_FIELDS, "route.", id));
      try {
        let normal = normaliseRoute({
          adapter: route.adapter, alias: route.alias, model: route.model, effort: route.effort, mode,
          // An unresolved worktree is already a fault; the route is still checked on its own.
          ...(mode === "worktree_write" ? { worktree: worktree?.path ?? identity.cwd } : {}),
        } as RouteInput, identity, catalogue, true);
        const unknown = (normal.warnings ?? []).find((warning) => warning.includes("catalogue; passing it as given"));
        if (unknown) nodeFaults.push({ ...at, error: "route_unknown", fix: `${unknown.replace(/; passing it as given$/u, "")}; name a catalogue model or alias.` });
        if (normal.model === undefined && normal.alias !== undefined) {
          const model = catalogue.adapters.find((entry) => entry.name === normal.adapter)?.aliases[normal.alias]?.[0];
          if (model === undefined) throw new InputError("route_unknown", `Name a catalogue model for ${normal.adapter}/${normal.alias}.`);
          // Freeze the alias to the model whose capacity is reserved. Dispatch
          // must not pick another candidate after the reservation is taken.
          normal = normaliseRoute({ ...route, adapter: normal.adapter, alias: undefined, model, effort: normal.effort, mode,
            ...(mode === "worktree_write" ? { worktree: worktree?.path ?? identity.cwd } : {}) } as RouteInput, identity, catalogue, true);
        }
        plannedRoute = Object.fromEntries(Object.entries({ adapter: normal.adapter, alias: normal.model === undefined ? normal.alias : undefined,
          model: normal.model, effort: normal.effort }).filter(([, value]) => value !== undefined)) as PlannedNode["route"];
        pool = { adapter: normal.adapter, model: normal.model ?? normal.alias ?? "default", account: "default" };
      } catch (error) {
        if (!(error instanceof InputError)) throw error;
        nodeFaults.push({ ...at, error: error.code, fix: error.fix });
      }
    }

    faults.push(...nodeFaults);
    if (nodeFaults.length || !briefPlan || !plannedRoute || !pool) continue;
    planned.push({
      id,
      tier: (raw.tier as PlannedNode["tier"] | undefined) ?? "senior",
      ...(typeof raw.role === "string" ? { role: raw.role } : {}),
      route: plannedRoute,
      pool,
      authority: {
        mode,
        confidential: own.confidential === true || root.confidential === true,
        allow_secrets: own.allow_secrets === true,
        ...(own.network !== undefined ? { network: own.network } : root.network !== undefined ? { network: root.network } : {}),
        ...(own.capabilities?.length ? { capabilities: [...own.capabilities].sort() } : {}),
        ...(own.read_roots?.length ? { read_roots: own.read_roots.map(canonical) } : {}),
      },
      ...(cwd === undefined ? {} : { cwd }),
      ...(worktree === undefined ? {} : { worktree }),
      brief: briefPlan,
      inputs: parsedInputs.map((item) => ({ from: item.from, part: "result" as const })),
      depends_on: deps,
      gates,
      gate_timeout_seconds: gateTimeout,
      ...(typeof raw.timeout_seconds === "number" ? { timeout_seconds: raw.timeout_seconds } : {}),
      spec_hash: "",
    });
  }

  // Acyclic, by Kahn's algorithm over depends_on.
  const indegree = new Map([...ids].map((id) => [id, parents.get(id)?.length ?? 0]));
  const children = new Map<string, string[]>([...ids].map((id) => [id, []]));
  for (const [child, list] of parents) for (const parent of list) children.get(parent)?.push(child);
  const queue = [...indegree].filter(([, degree]) => degree === 0).map(([id]) => id);
  const order: string[] = [];
  while (queue.length) {
    const id = queue.shift()!;
    order.push(id);
    for (const child of children.get(id) ?? []) {
      indegree.set(child, indegree.get(child)! - 1);
      if (indegree.get(child) === 0) queue.push(child);
    }
  }
  if (order.length !== ids.size) {
    const cyclic = [...ids].filter((id) => !order.includes(id)).sort();
    faults.push({ error: "cycle", fix: `Break the dependency cycle through ${cyclic.join(", ")}.` });
  }

  // Confidentiality propagates through data: any parent's output reaching a node makes it confidential.
  const byId = new Map(planned.map((node) => [node.id, node]));
  const rawById = new Map(nodes.filter(object).map((node) => [node.id as string, node]));
  const confidential = new Set<string>();
  for (const id of order) {
    const node = rawById.get(id);
    const explicit = object(node?.authority) ? node.authority.confidential : undefined;
    const inherited = root.confidential === true || (parents.get(id) ?? []).some((parent) => confidential.has(parent));
    if (inherited && explicit === false) {
      const source = (parents.get(id) ?? []).filter((parent) => confidential.has(parent));
      faults.push({ node: id, error: "confidentiality_downgrade",
        fix: `Remove confidential false: it consumes confidential ${source.length ? source.join(", ") : "root"} output.` });
    }
    if (inherited || explicit === true) confidential.add(id);
    const plannedNode = byId.get(id);
    if (plannedNode) plannedNode.authority.confidential = confidential.has(id);
  }

  // One writer per worktree: writers sharing one must be totally ordered by depends_on.
  const ancestors = (id: string, seen = new Set<string>()): Set<string> => {
    for (const parent of parents.get(id) ?? []) if (!seen.has(parent)) { seen.add(parent); ancestors(parent, seen); }
    return seen;
  };
  const writers = new Map<string, string[]>();
  for (const node of nodes.filter(object))
    if (object(node.authority) && node.authority.mode === "worktree_write" && typeof node.worktree === "string") {
      const key = resolvedWorktrees.get(node.worktree)?.path ?? node.worktree;
      writers.set(key, [...(writers.get(key) ?? []), node.id as string]);
    }
  if (order.length === ids.size)
    for (const [path, list] of writers)
      for (let i = 0; i < list.length; i++)
        for (let j = i + 1; j < list.length; j++)
          if (!ancestors(list[i]!).has(list[j]!) && !ancestors(list[j]!).has(list[i]!))
            faults.push({ node: list[j], error: "writers_unordered",
              fix: `Order ${list[i]} and ${list[j]} by depends_on: both write ${path}.` });

  // Width and depth, after the shape is known.
  const height: Record<string, number> = {};
  for (const id of [...order].reverse())
    height[id] = 1 + Math.max(0, ...(children.get(id) ?? []).map((child) => height[child] ?? 0));

  if (faults.length) return { faults };
  for (const node of planned) {
    node.spec_hash = sha256(canonicalJson({
      route: node.route, authority: node.authority, cwd: node.cwd ?? identity.cwd, worktree: node.worktree?.path,
      brief: node.brief.sha256, inputs: node.inputs, depends_on: [...node.depends_on].sort(), gates: node.gates,
      gate_timeout_seconds: node.gate_timeout_seconds, timeout_seconds: node.timeout_seconds,
    }));
  }
  return { faults, plan: { graph_id: input.graph_id as string, revision: 1, max_width: maxWidth as number, nodes: planned, height } };
}

/** One line per fault, each with its fix. */
export function faultLines(faults: Fault[]): string {
  return faults.map((fault) => `${fault.node ? `node ${fault.node}: ` : ""}${fault.error} · fix: ${fault.fix}`).join("\n");
}
