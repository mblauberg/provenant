/** Validate the Fabric request and forward routing/control choices to its owner. */
import { realpathSync, statSync } from "node:fs";
import { isAbsolute, relative, resolve, sep } from "node:path";
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
export const ACCESS_MODES = ["read_only", "worktree_write"] as const;
export type AccessMode = (typeof ACCESS_MODES)[number];

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
  add_dirs?: string[];
  allow_secrets?: boolean;
  fallback?: boolean | "any" | Array<string | Record<string, unknown>>;
  context_ceiling?: number;
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
  { value?: string; warning?: string } {
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
  const closest = ranked.slice(0, 3).map(({ item }) => item);
  throw new InputError(`${field}_invalid`, `Choose a valid ${field}: ${closest.join(", ") || choices.join(", ")}.`);
}

export function normaliseRoute(input: RouteInput, identity: Identity, catalogue: CatalogueSnapshot): NormalisedRoute {
  input = { ...input, model: input.model || undefined, alias: input.alias || undefined };
  const warnings: string[] = [];
  let selector =
    input.model ?? (input.alias && !["flagship", "workhorse", "scout"].includes(input.alias) ? input.alias : undefined);
  const roleAlias = input.model === undefined && input.alias !== undefined
    ? ["flagship", "workhorse", "scout"].find((alias) => editDistance(routeKey(alias), routeKey(input.alias!)) <= 1)
    : undefined;
  if (roleAlias !== undefined && roleAlias !== input.alias) warnings.push(`corrected alias ${input.alias} to ${roleAlias}`);
  if (roleAlias !== undefined) input.alias = roleAlias;
  const isRoleAlias = roleAlias !== undefined;
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
  const adapter =
    input.adapter ?? inferred ?? (SUPPORTED_ADAPTERS.has(identity.provider) ? identity.provider : undefined);
  if (adapter === undefined) throw new InputError("adapter_required", `Pass adapter ${DISPATCH_ADAPTERS.join(", ")}.`);
  if (!SUPPORTED_ADAPTERS.has(adapter)) {
    throw new InputError("adapter_invalid", `Pass adapter ${DISPATCH_ADAPTERS.join(", ")}.`);
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
    ...Object.fromEntries(
      ["cwd", "network", "sandbox", "add_dirs", "fallback", "context_ceiling", "allow_secrets"]
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
    } else if (key === "allow_secrets") {
      if (value === true) args.push("--allow-secrets");
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

export function workingIdentity(input: RouteInput, identity: Identity, projectRoots: string[] = identity.registeredProjects ?? [identity.project]): Identity {
  if (input.cwd === undefined) return identity;
  if (input.mode === "worktree_write")
    throw new InputError("cwd_not_applicable", "Use worktree for writers; cwd is a read-only directory.");
  const cwd = canonical(resolve(identity.cwd, input.cwd));
  let directory = false;
  try { directory = statSync(cwd).isDirectory(); } catch { /* typed below */ }
  const candidateProject = projectRoot(cwd);
  const belongsToRegisteredProject = projectRoots.some((root) => inside(canonical(root), cwd) || candidateProject === canonical(root));
  if ((!inside(canonical(identity.cwd), cwd) && !belongsToRegisteredProject) || !directory)
    throw new InputError("cwd_unavailable", "Pass an existing cwd inside a registered Fabric project.");
  return { ...identity, cwd };
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
