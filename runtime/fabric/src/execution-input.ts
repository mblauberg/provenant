/** Validate the Fabric request and forward routing/control choices to its owner. */
import { realpathSync, statSync } from "node:fs";
import { isAbsolute, relative, resolve, sep } from "node:path";
import type { Identity } from "./identity.js";
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
 * skills/orchestrate/scripts/cf_dispatch.sh and is marked `"dispatch":
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
  mode?: AccessMode;
  worktree?: string;
  cwd?: string;
  network?: boolean;
  sandbox?: string;
  add_dirs?: string[];
  fallback?: boolean | "any" | Array<string | Record<string, unknown>>;
}

export interface DispatchInput extends RouteInput {
  prompt?: string;
  prompt_file?: string;
  resume?: string;
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

export interface BatchInput {
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
}

export function normaliseRoute(input: RouteInput, identity: Identity, catalogue: CatalogueSnapshot): NormalisedRoute {
  const selector =
    input.model ?? (input.alias && !["flagship", "workhorse", "scout"].includes(input.alias) ? input.alias : undefined);
  const candidates =
    selector === undefined
      ? []
      : catalogue.adapters.filter((entry) => {
          const details =
            (entry as typeof entry & { model_details?: Array<{ id?: string; names?: string[] }> }).model_details ?? [];
          return (
            entry.models.includes(selector) ||
            details.some((model) => model.id === selector || model.names?.includes(selector)) ||
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
  let alias = input.alias;
  let model = input.model;
  if (alias !== undefined && !["flagship", "workhorse", "scout"].includes(alias)) {
    const models = catalogue.adapters.find((entry) => entry.name === adapter)?.models ?? [];
    const matches = models.filter((name) =>
      name
        .toLowerCase()
        .split(/[^a-z0-9]+/u)
        .includes(alias!.toLowerCase()),
    );
    model ??= matches.length === 1 ? matches[0] : alias;
    alias = undefined;
  }
  if (model !== undefined) alias = undefined;
  const mode = input.mode ?? "read_only";
  if (!ACCESS_MODES.includes(mode)) throw new InputError("mode_invalid", `Pass mode ${ACCESS_MODES.join(" or ")}.`);
  if (mode === "worktree_write" && input.worktree === undefined) {
    throw new InputError("worktree_required", "Pass worktree=<registered Git worktree root> with mode worktree_write.");
  }
  if (mode !== "worktree_write" && input.worktree !== undefined) {
    throw new InputError("worktree_not_applicable", "Pass mode worktree_write with worktree, or omit worktree.");
  }
  return {
    adapter,
    ...(model === undefined ? { alias: alias ?? "workhorse" } : { model }),
    ...(input.effort === undefined ? {} : { effort: input.effort }),
    role: "worker",
    access_mode: mode,
    ...(input.worktree === undefined ? {} : { worktree: input.worktree }),
    ...Object.fromEntries(
      ["cwd", "network", "sandbox", "add_dirs", "fallback"]
        .filter((key) => input[key as keyof RouteInput] !== undefined)
        .map((key) => [key, input[key as keyof RouteInput]]),
    ),
  };
}

export function routeArguments(route: NormalisedRoute): string[] {
  const args: string[] = [];
  for (const [key, value] of Object.entries(route)) {
    if (value === undefined || key === "cwd") continue;
    if (key === "add_dirs") {
      for (const dir of value as string[]) args.push("--add-dir", dir);
    } else args.push(`--${key.replaceAll("_", "-")}`, typeof value === "string" ? value : JSON.stringify(value));
  }
  return args;
}

export function validatePrompt(prompt: string | undefined, promptFile: string | undefined): void {
  if ((prompt === undefined) === (promptFile === undefined)) {
    throw new InputError("prompt_required", "Pass exactly one of prompt or prompt_file.");
  }
}

export function timeoutSeconds(value: number | undefined, mode?: AccessMode): number {
  const timeout = value ?? (mode === "worktree_write" ? 10800 : DEFAULT_TIMEOUT_SECONDS);
  if (!Number.isFinite(timeout) || timeout <= 0)
    throw new InputError("invalid_input", "timeout_seconds must be finite and positive");
  return timeout;
}

export function workingIdentity(input: RouteInput, identity: Identity): Identity {
  if (input.cwd === undefined) return identity;
  if (input.mode === "worktree_write")
    throw new InputError("cwd_not_applicable", "Use worktree for writers; cwd is a read-only directory.");
  const cwd = canonical(resolve(identity.cwd, input.cwd));
  if (!inside(canonical(identity.cwd), cwd) || !statSync(cwd).isDirectory())
    throw new InputError("cwd_unavailable", "Pass an existing cwd inside this workspace.");
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
