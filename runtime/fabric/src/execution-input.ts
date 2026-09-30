/** Validate the Fabric request and forward routing/control choices to its owner. */
import { readFileSync, realpathSync, statSync } from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { projectRoot, withoutGitRedirects, type Identity } from "./identity.js";
import { execFile } from "node:child_process";
import type { CatalogueSnapshot } from "./catalogue.js";
const DEFAULT_TIMEOUT_SECONDS = 3600;
function canonical(path: string) {
  try {
    return realpathSync(path);
  } catch {
    return resolve(path);
  }
}
function inside(root: string, candidate: string) {
  const path = relative(root, candidate);
  return path === "" || (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`));
}
/**
 * Adapters this front door can actually run: each has an executing arm in
 * skills/orchestrate/scripts/adapters/*.py and is marked `"dispatch":
 * "implemented"` in the product-owned `dispatch_registry` in
 * config/adapter-compatibility.yaml. Adapters the registry marks dormant or
 * unsupported are absent on purpose, so they are a typed input error here
 * rather than a refusal paid for with a run directory, prompt staging and
 * route resolution. tests/adapter-registry.test.ts binds this list to the
 * registry and to the dispatcher.
 */
export const DISPATCH_ADAPTERS = ["agy", "claude", "codex", "copilot", "cursor", "kiro", "opencode"] as const;
const SUPPORTED_ADAPTERS = new Set<string>(DISPATCH_ADAPTERS);
const ROLE_ALIASES = ["flagship", "workhorse", "scout"];

/**
 * Seats whose provider runs its own models as native subagents: Claude Code
 * uses its Agent tool and Codex its own subagents, so Fabric carries every
 * other provider for them. Any other or unknown seat keeps ordinary routing.
 */
const NATIVE_ADAPTERS: Record<string, string> = { claude: "claude", codex: "codex" };
export function nativeAdapter(identity: Identity): string | undefined {
  return Object.hasOwn(NATIVE_ADAPTERS, identity.provider) ? NATIVE_ADAPTERS[identity.provider] : undefined;
}

/** `opencode-go/x`, `opencode/x` and `openrouter/x` are OpenCode provider paths. */
const providerPath = (value: string) => /^(?:opencode|opencode-go|openrouter)\//iu.test(value);

/**
 * Split an adapter prefix off an uncatalogued model (`codex/gpt-7-luna`,
 * `opencode/opencode-go/x`), keeping an OpenCode provider path whole
 * (`opencode/big-pickle`). A catalogued id is never split.
 */
function adapterPrefix(selector: string, adapter: string | undefined, catalogue: CatalogueSnapshot):
  { adapter?: string; model: string } {
  const catalogued = catalogue.adapters.some((entry) => entry.models.includes(selector) ||
    (entry.model_details ?? []).some((model) => model.id === selector));
  if (catalogued) return { adapter, model: selector };
  const slash = selector.indexOf("/");
  const head = selector.slice(0, slash), rest = selector.slice(slash + 1);
  if (slash > 0 && rest && SUPPORTED_ADAPTERS.has(head) && (adapter === undefined || adapter === head))
    return { adapter: head, model: providerPath(selector) && !rest.includes("/") ? selector : rest };
  if (adapter === undefined && providerPath(selector)) return { adapter: "opencode", model: selector };
  return { adapter, model: selector };
}
export const ACCESS_MODES = ["read_only", "worktree_write"] as const;
export type AccessMode = (typeof ACCESS_MODES)[number];
export const DISPATCH_CAPABILITIES = ["postgres", "browser"] as const;
export type DispatchCapability = (typeof DISPATCH_CAPABILITIES)[number];

/**
 * The whole routing surface: who runs it, which route, and how much access it
 * gets. Assurance selectors are deliberately absent, because this front door
 * always dispatches ordinary work and cannot honour them.
 */
export interface RouteInput {
  adapter?: string;
  alias?: string;
  model?: string;
  effort?: string;
  mode?: AccessMode | "write" | "worktree" | "rw" | "read" | "ro";
  worktree?: string;
  cwd?: string;
  network?: boolean;
  sandbox?: string;
  capabilities?: DispatchCapability[];
  add_dirs?: string[];
  allow_secrets?: boolean;
  fallback?: boolean | "any" | Array<string | Record<string, unknown>>;
  context_ceiling?: number;
  /** A global weighted pool (strong, bulk, design, writing) or a task-class/alias synonym. */
  route?: string;
  rotate?: boolean;
  council?: number;
  models?: string[];
  /** Never route this task, or a fallback, to a free or prompt-training model; works with any selector. */
  confidential?: boolean;
  /** Why the pool picked this model; set by the pool expansion, shown on the Route line. */
  pick_reason?: string;
  /** Internal: the tier alias a native seat's default was rerouted from, for the pool note. */
  native_default?: string;
}

export interface DispatchInput extends RouteInput {
  prompt?: string;
  prompt_file?: string;
  resume?: string;
  handoff?: string;
  task_id?: string;
  timeout_seconds?: number;
  wait_seconds?: number;
}

export interface BatchTaskInput extends RouteInput {
  id?: string;
  prompt?: string;
  prompt_file?: string;
  timeout_seconds?: number;
}

export interface BatchInput extends RouteInput {
  timeout_seconds?: number;
  tasks: BatchTaskInput[];
  concurrency?: number;
  wait_seconds?: number;
}

export interface NormalisedRoute {
  adapter: string;
  alias?: string;
  model?: string;
  effort?: string;
  role: string;
  access_mode: AccessMode;
  worktree?: string;
  context_ceiling?: number;
  allow_secrets?: boolean;
  capabilities?: DispatchCapability[];
  confidential?: boolean;
  pick_reason?: string;
  read_roots?: string[];
  warnings?: string[];
}

const MODE_SYNONYMS: Record<string, AccessMode> = {
  write: "worktree_write", worktree: "worktree_write", rw: "worktree_write",
  read: "read_only", ro: "read_only",
};
export function canonicalMode(value: string | undefined): string | undefined {
  return value === undefined ? undefined : MODE_SYNONYMS[value] ?? value;
}
const routeKey = (value: string) => value.toLowerCase().replace(/[^a-z0-9]/gu, "");

/** The tier alias a caller meant, tolerating case and a one-letter typo (`Workhorse`, `workhorze`). */
export function tierAlias(alias: string): string | undefined {
  return ROLE_ALIASES.find((candidate) => editDistance(routeKey(candidate), routeKey(alias)) <= 1);
}

export function editDistance(left: string, right: string): number {
  const row = Array.from({ length: right.length + 1 }, (_, index) => index);
  for (let i = 1; i <= left.length; i++) {
    let diagonal = row[0]!;
    row[0] = i;
    for (let j = 1; j <= right.length; j++) {
      const above = row[j]!;
      row[j] = Math.min(row[j]! + 1, row[j - 1]! + 1, diagonal + (left[i - 1] === right[j - 1] ? 0 : 1));
      diagonal = above;
    }
  }
  return row[right.length]!;
}

function correctSelector(selector: string | undefined, catalogue: CatalogueSnapshot, adapter?: string, field = "model"):
  { value?: string; warning?: string; unknown?: true } {
  if (!selector) return {};
  const entries = catalogue.adapters.filter((entry) => adapter === undefined || entry.name === adapter);
  const choices = [...new Set(entries.flatMap((entry) => [
    ...entry.models,
    ...Object.keys(entry.aliases ?? {}),
    ...Object.values(entry.aliases ?? {}).flat(),
    ...(entry.model_details ?? []).flatMap((model) => [model.id, ...(Array.isArray(model.names) ? model.names : [])]),
  ]).filter((item): item is string => typeof item === "string"))];
  const key = routeKey(selector);
  const matches = new Map<string, string>();
  for (const entry of entries) {
    const modelId = (value: string) => (entry.model_details ?? []).find((item) =>
      item.id === value || (Array.isArray(item.names) && item.names.some((name) => typeof name === "string" && routeKey(name) === routeKey(value))))?.id ?? value;
    for (const id of entry.models) if (routeKey(id) === key) {
      const resolved = modelId(id);
      matches.set(resolved, resolved);
    }
    for (const model of entry.model_details ?? []) {
      if (typeof model.id !== "string") continue;
      if (routeKey(model.id) === key) matches.set(model.id, model.id);
      for (const name of Array.isArray(model.names) ? model.names : [])
        if (typeof name === "string" && routeKey(name) === key) matches.set(model.id, model.id);
    }
    for (const [alias, values] of Object.entries(entry.aliases ?? {})) {
      if (routeKey(alias) !== key) continue;
      for (const value of values) {
        const resolved = modelId(value);
        matches.set(resolved, resolved);
      }
    }
    for (const value of Object.values(entry.aliases ?? {}).flat()) {
      if (routeKey(value) !== key) continue;
      const resolved = modelId(value);
      matches.set(resolved, resolved);
    }
  }
  if (matches.size === 1) {
    const value = [...matches.values()][0]!;
    return value === selector ? {} : { value, warning: `corrected ${field} ${selector} to ${value}` };
  }
  if (matches.size > 1) throw new InputError(`${field}_ambiguous`, `Choose one of: ${[...matches.values()].join(", ")}.`);
  const ranked = choices.map((item) => ({ item, score: editDistance(key, routeKey(item)) }))
    .sort((left, right) => left.score - right.score);
  // Correct a typo only to a single nearby name with the same version numbers, never across versions.
  const digits = (value: string) => value.replace(/\D/gu, "");
  const near = ranked.filter(({ item, score }) =>
    score === ranked[0]?.score && score <= (key.length < 8 ? 1 : 2) && digits(item) === digits(selector));
  if (near.length === 1) {
    const value = correctSelector(near[0]!.item, catalogue, adapter, field).value ?? near[0]!.item;
    return { value, warning: `corrected ${field} ${selector} to ${value}` };
  }
  // The catalogue is a default, never a gate: the adapter decides whether it can run the name.
  return { value: selector, unknown: true };
}

export function normaliseRoute(input: RouteInput, identity: Identity, catalogue: CatalogueSnapshot): NormalisedRoute {
  input = { ...input, model: input.model || undefined, alias: input.alias || undefined };
  const warnings: string[] = [];
  // `adapter/model@effort` is the models-list spelling; accept it in model too.
  const suffixed = input.model?.match(/^(.+)@(none|minimal|low|medium|high|xhigh|max|ultra)$/u);
  if (suffixed) {
    if (input.effort !== undefined && input.effort !== suffixed[2])
      warnings.push(`effort ${suffixed[2]} in model ignored: effort ${input.effort} was named`);
    input = { ...input, model: suffixed[1], effort: input.effort ?? suffixed[2] };
  }
  if (input.capabilities !== undefined && (
    !Array.isArray(input.capabilities) ||
    input.capabilities.some((value) => !DISPATCH_CAPABILITIES.includes(value)) ||
    new Set(input.capabilities).size !== input.capabilities.length
  )) {
    throw new InputError("capabilities_invalid", "Pass capabilities as a list of distinct postgres or browser values.");
  }
  let selector =
    input.model ?? (input.alias && !ROLE_ALIASES.includes(input.alias) ? input.alias : undefined);
  const roleAlias = input.model === undefined && input.alias !== undefined ? tierAlias(input.alias) : undefined;
  if (roleAlias !== undefined && roleAlias !== input.alias) warnings.push(`corrected alias ${input.alias} to ${roleAlias}`);
  if (roleAlias !== undefined) input.alias = roleAlias;
  const isRoleAlias = roleAlias !== undefined;
  if (selector !== undefined && !isRoleAlias) {
    const prefixed = adapterPrefix(selector, input.adapter, catalogue);
    if (prefixed.adapter !== input.adapter || prefixed.model !== selector) {
      input.adapter = prefixed.adapter;
      selector = prefixed.model;
      if (input.model !== undefined) input.model = selector;
      else input.alias = selector;
    }
  }
  const corrected = isRoleAlias ? {} : correctSelector(selector, catalogue, input.adapter, input.model === undefined ? "alias" : "model");
  if (corrected.warning) warnings.push(corrected.warning);
  selector = corrected.value ?? selector;
  if (corrected.value !== undefined) {
    if (input.model !== undefined) input.model = corrected.value;
    else if (input.alias !== undefined) {
      input.model = corrected.value;
      input.alias = undefined;
    }
  }
  const candidates =
    selector === undefined
      ? []
      : catalogue.adapters.filter((entry) => {
          const details = entry.model_details ?? [];
          return (
            entry.models.includes(selector) ||
            Object.hasOwn(entry.aliases ?? {}, selector) ||
            details.some((model) => model.id === selector || (Array.isArray(model.names) && model.names.includes(selector))) ||
            entry.models.some((model) =>
              model
                .toLowerCase()
                .split(/[^a-z0-9]+/u)
                .includes(selector.toLowerCase()),
            )
          );
        });
  const inferred = candidates.find((entry) => entry.name === identity.provider)?.name ?? candidates[0]?.name;
  if (corrected.unknown && input.adapter === undefined && inferred === undefined)
    throw new InputError("adapter_required", `Prefix ${selector} with its adapter (${DISPATCH_ADAPTERS.join(", ")}), e.g. opencode/<id>, or pass adapter.`);
  const adapter =
    input.adapter ?? inferred ?? (SUPPORTED_ADAPTERS.has(identity.provider) ? identity.provider : undefined);
  if (adapter === undefined) throw new InputError("adapter_required", `Pass adapter ${DISPATCH_ADAPTERS.join(", ")}.`);
  if (!SUPPORTED_ADAPTERS.has(adapter)) {
    throw new InputError("adapter_invalid", `Pass adapter ${DISPATCH_ADAPTERS.join(", ")}.`);
  }
  if (corrected.unknown) warnings.push(`${selector} is not in the ${adapter} catalogue; passing it as given`);
  if (adapter === nativeAdapter(identity)) {
    warnings.unshift(`NATIVE: ${adapter}/${input.model ?? input.alias ?? "workhorse"} is this ${adapter} seat's own model; ` +
      "spawn a native subagent instead of Fabric");
  }
  const requestedMode = input.mode ?? "read_only";
  const mode = canonicalMode(requestedMode) as AccessMode;
  if (mode !== requestedMode) warnings.push(`corrected mode ${requestedMode} to ${mode}`);
  if (!ACCESS_MODES.includes(mode)) throw new InputError("mode_invalid", `Pass mode ${ACCESS_MODES.join(" or ")}.`);
  if (mode === "worktree_write" && input.worktree === undefined) {
    throw new InputError("worktree_required", "Pass worktree=<registered Git worktree root> with mode worktree_write.");
  }
  if (mode !== "worktree_write" && input.worktree !== undefined) {
    throw new InputError("worktree_not_applicable", "Pass mode worktree_write with worktree, or omit worktree.");
  }
  return {
    adapter,
    ...(input.alias === undefined && input.model === undefined ? { alias: "workhorse" } : {}),
    ...(input.alias === undefined ? {} : { alias: input.alias }),
    ...(input.model === undefined ? {} : { model: input.model }),
    ...(input.effort === undefined ? {} : { effort: input.effort }),
    role: "worker",
    access_mode: mode,
    ...(input.worktree === undefined ? {} : { worktree: resolve(identity.cwd, input.worktree) }),
    ...(warnings.length ? { warnings } : {}),
    ...(input.capabilities?.length ? { capabilities: [...input.capabilities].sort() } : {}),
    ...Object.fromEntries(
      ["cwd", "network", "sandbox", "add_dirs", "fallback", "context_ceiling", "allow_secrets", "confidential", "pick_reason"]
        .filter((key) => input[key as keyof RouteInput] !== undefined)
        .map((key) => [key, input[key as keyof RouteInput]]),
    ),
  };
}

export function routeArguments(route: NormalisedRoute): string[] {
  const args: string[] = [];
  for (const [key, value] of Object.entries(route)) {
    if (key === "warnings") continue;
    if (value === undefined || (key === "alias" && route.model !== undefined)) continue;
    if (key === "add_dirs") {
      for (const dir of value as string[]) args.push("--add-dir", dir);
    } else if (key === "read_roots") {
      for (const dir of value as string[]) args.push("--read-root", dir);
    } else if (key === "allow_secrets" || key === "confidential") {
      if (value === true) args.push(`--${key.replaceAll("_", "-")}`);
    } else args.push(`--${key.replaceAll("_", "-")}`, typeof value === "string" ? value : JSON.stringify(value));
  }
  return args;
}

export function validatePrompt(prompt: string | undefined, promptFile: string | undefined): void {
  if ((prompt === undefined) === (promptFile === undefined)) {
    throw new InputError("prompt_required", "Pass exactly one of prompt or prompt_file.");
  }
}

export function timeoutSeconds(value: number | undefined, mode?: RouteInput["mode"]): number {
  const timeout = value ?? (mode === "worktree_write" ? 10800 : DEFAULT_TIMEOUT_SECONDS);
  if (!Number.isFinite(timeout) || timeout <= 0)
    throw new InputError("invalid_input", "timeout_seconds must be finite and positive");
  return timeout;
}

export interface DispatchDefaults {
  add_dirs?: string[];
  network?: boolean;
  timeout_seconds?: number;
}

const POLICY = ".agents/fabric-policy.json";
const DEFAULT_CHECKS: Record<keyof DispatchDefaults, (value: unknown) => boolean> = {
  add_dirs: (value) => Array.isArray(value) && value.every((item) => typeof item === "string" && item !== ""),
  network: (value) => typeof value === "boolean",
  timeout_seconds: (value) => typeof value === "number" && Number.isFinite(value) && value > 0,
};

/** The workspace policy's `dispatch_defaults`; a malformed entry is dropped with a warning. */
export function dispatchDefaults(workspace: string): { defaults: DispatchDefaults; warnings: string[] } {
  let policy: unknown;
  try {
    policy = JSON.parse(readFileSync(join(workspace, POLICY), "utf8"));
  } catch (error) {
    const missing = (error as NodeJS.ErrnoException).code === "ENOENT";
    return { defaults: {}, warnings: missing ? [] : [`${POLICY} is unreadable; no dispatch defaults applied`] };
  }
  const raw = (policy as { dispatch_defaults?: unknown } | null)?.dispatch_defaults;
  if (raw === undefined) return { defaults: {}, warnings: [] };
  if (typeof raw !== "object" || raw === null || Array.isArray(raw))
    return { defaults: {}, warnings: [`${POLICY} dispatch_defaults is not an object; ignored`] };
  const defaults: Record<string, unknown> = {}, warnings: string[] = [];
  for (const [key, value] of Object.entries(raw)) {
    if (Object.hasOwn(DEFAULT_CHECKS, key) && DEFAULT_CHECKS[key as keyof DispatchDefaults](value)) defaults[key] = value;
    else warnings.push(`${POLICY} dispatch_defaults.${key} ignored: set add_dirs, network or timeout_seconds`);
  }
  return { defaults, warnings };
}

/** Fill route fields the dispatch left unset; network only where the adapter controls it (Codex). */
export function applyDispatchDefaults(route: NormalisedRoute, defaults: DispatchDefaults): NormalisedRoute {
  const fields = route as NormalisedRoute & Record<string, unknown>;
  if (fields.add_dirs === undefined && defaults.add_dirs !== undefined) fields.add_dirs = [...defaults.add_dirs];
  if (fields.network === undefined && defaults.network !== undefined && route.adapter === "codex")
    fields.network = defaults.network;
  return route;
}

const registeredRoots = (identity: Identity) => identity.registeredProjects ?? [identity.project];

/** Whether a canonical directory lies in a registered project or one of its linked worktrees. */
function inRegisteredProject(directory: string, projectRoots: string[]): boolean {
  const candidateProject = projectRoot(directory);
  return projectRoots.some((root) => inside(canonical(root), directory) || candidateProject === canonical(root));
}

export function workingIdentity(input: RouteInput, identity: Identity, projectRoots: string[] = registeredRoots(identity)): Identity {
  if (input.cwd === undefined) return identity;
  if (input.mode === "worktree_write")
    throw new InputError("cwd_not_applicable", "Use worktree for writers; cwd is a read-only directory.");
  const cwd = canonical(resolve(identity.cwd, input.cwd));
  let directory = false;
  try { directory = statSync(cwd).isDirectory(); } catch { /* typed below */ }
  if (!directory)
    throw new InputError("cwd_unavailable", "Pass an existing cwd directory.");
  if (!inside(canonical(identity.cwd), cwd) && !inRegisteredProject(cwd, projectRoots))
    throw new InputError("cwd_unavailable",
      "Pass a cwd inside a registered Fabric project; register that project by running `fabric whoami` there, or dispatch from it.");
  return { ...identity, cwd };
}

/**
 * An absolute prompt path whose directory is canonical, so the owner compares
 * it with its resolved workspace (macOS /var is /private/var). The file itself
 * is not resolved: the owner still refuses a linked prompt.
 */
export function ownerPromptPath(identity: Identity, promptFile: string): string {
  const path = resolve(identity.cwd, promptFile);
  return join(canonical(dirname(path)), basename(path));
}

/**
 * Directories outside the caller's cwd that a read-only cwd or a prompt file
 * uses, once each is matched to a registered project. The owner keeps its
 * workspace bound and accepts these as the only exceptions, so the run stays
 * in the caller's run root while the provider reads the other project.
 */
export function readRoots(
  identity: Identity,
  cwd: string | undefined,
  promptFile: string | undefined,
  projectRoots: string[] = registeredRoots(identity),
): string[] {
  const home = canonical(identity.cwd);
  const roots: string[] = [];
  if (cwd !== undefined && !inside(home, canonical(cwd))) roots.push(cwd);
  if (promptFile !== undefined) {
    const directory = dirname(ownerPromptPath(identity, promptFile));
    if (!inside(home, directory) && !roots.some((root) => inside(canonical(root), directory)) &&
        inRegisteredProject(directory, projectRoots))
      roots.push(directory);
  }
  return roots;
}

/**
 * The read roots and cwd a resume reuses. Each was canonical when granted; one
 * that now resolves elsewhere, or has left every registered project, is refused.
 */
export function savedReadRoots(
  identity: Identity,
  roots: unknown,
  cwd: unknown,
  projectRoots: string[] = registeredRoots(identity),
): string[] {
  if (roots === undefined || roots === null || (Array.isArray(roots) && roots.length === 0)) return [];
  const bound = Array.isArray(roots) ? [...roots, ...(typeof cwd === "string" ? [cwd] : [])] : [roots];
  if (!bound.every((path) => typeof path === "string" && isAbsolute(path) && canonical(path) === path &&
      inRegisteredProject(path, projectRoots)))
    throw new InputError("resume_read_root_changed",
      "Dispatch a new run; a saved read root or cwd has moved or left every registered Fabric project.");
  return roots as string[];
}

export class InputError extends Error {
  constructor(
    readonly code: string,
    readonly fix: string,
  ) {
    super(fix);
  }
}

export function rejected(error: unknown): Record<string, unknown> {
  if (error instanceof InputError) return { status: "rejected", error: error.code, fix: error.fix };
  const detail = (error instanceof Error ? error.message : String(error)).replace(/\s+/gu, " ").trim();
  return {
    status: "rejected",
    error: "preflight_unavailable",
    fix: `Check the harness Python environment, execution owner scripts and workspace permissions: ${detail}`,
  };
}

export async function preflight(
  python: string,
  owner: string,
  tasks: Record<string, unknown>[],
  identity: Identity,
  env: NodeJS.ProcessEnv,
  signal: AbortSignal,
): Promise<Record<string, unknown>> {
  signal.throwIfAborted();
  const child = execFile(python, [owner, "--preflight-json"], {
    cwd: identity.cwd,
    env: withoutGitRedirects(env),
    signal,
    killSignal: "SIGKILL",
    timeout: 50_000,
    maxBuffer: 1024 * 1024,
  });
  const output = new Promise<string>((resolveOutput, rejectOutput) => {
    let stdout = "";
    child.stdout!.on("data", (chunk: string) => {
      stdout += chunk;
    });
    child.once("error", rejectOutput);
    child.once("close", (code) =>
      code === 0
        ? resolveOutput(stdout)
        : rejectOutput(new Error("Preflight unavailable; restore the harness Python environment.")),
    );
  });
  child.stdin!.end(JSON.stringify({ tasks }));
  return JSON.parse(await output) as Record<string, unknown>;
}
