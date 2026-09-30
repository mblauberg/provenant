/** Expand `route`, `rotate`, `council` and `models` into concrete adapter/model tasks (#848). */
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { withoutGitRedirects, type Identity } from "./identity.js";
import { InputError, nativeAdapter, tierAlias, type BatchTaskInput, type RouteInput } from "./execution-input.js";

export const POOL_FIELDS = ["route", "rotate", "council", "models"] as const;
const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

export interface Pick {
  adapter: string;
  model: string;
  effort?: string;
  family?: string;
  reason: string;
}
type PickResult =
  | { status: "ok"; picks: Pick[]; warnings: string[] }
  | { status: "rejected"; error: string; fix: string };

export function usesPool(input: RouteInput): boolean {
  return input.route !== undefined || input.models !== undefined || input.council !== undefined || input.rotate === true;
}

/**
 * A Claude or Codex seat that names no adapter, model or pool would land on its
 * own models through the default or a tier alias. Take the matching pool
 * instead, where the picker skips the seat's native models; a pool holding only
 * those is refused with `route_native_only`. An explicit adapter or model runs.
 */
export function nativeFirst<T extends RouteInput>(task: T, identity: Identity): T {
  if (nativeAdapter(identity) === undefined || task.adapter !== undefined || task.model !== undefined ||
      usesPool(task)) return task;
  const alias = task.alias === undefined ? "workhorse" : tierAlias(task.alias);
  if (alias === undefined) return task;
  const { alias: _alias, ...rest } = task;
  return { ...rest, route: alias, native_default: alias } as T;
}

function withoutPool<T extends RouteInput>(input: T): T {
  return Object.fromEntries(Object.entries(input).filter(([key]) =>
    !(POOL_FIELDS as readonly string[]).includes(key))) as T;
}

/**
 * Settle mixed selectors by precedence instead of rejecting them: models beats
 * route, alias and model; an explicit model beats route, rotate and council;
 * route beats alias; an alias without a route ignores rotate and council.
 * Every ignored field is named in a warning. `confidential` is not a selector:
 * it filters whatever the selectors resolve to.
 */
function settle<T extends RouteInput>(input: T): { task: T; warnings: string[] } {
  const warnings: string[] = [];
  const ignored = (fields: string[], winner: string) => {
    const named = fields.filter((field) => input[field as keyof RouteInput] !== undefined && input[field as keyof RouteInput] !== false);
    if (named.length) warnings.push(`${named.join(", ")} ignored: ${winner}`);
    return Object.fromEntries(Object.entries(input).filter(([key]) => !named.includes(key))) as T;
  };
  if (input.models !== undefined) {
    if (!Array.isArray(input.models) || input.models.some((item) => typeof item !== "string"))
      throw new InputError("models_invalid", "Pass models as a list of adapter/model[@effort] strings.");
    return { task: ignored(["alias", "model", "council", "rotate"], "models names the council"), warnings };
  }
  if (input.model !== undefined)
    return { task: ignored(["route", "council", "rotate"], `model ${input.model} was named explicitly`), warnings };
  if (input.alias !== undefined && input.route !== undefined)
    return { task: ignored(["alias"], `route ${input.route} was named`), warnings };
  if (input.alias !== undefined)
    return { task: ignored(["council", "rotate"], `alias ${input.alias} names one model; pass route for a pool`), warnings };
  return { task: input, warnings };
}

/** Ask the Python router for picks: one call for every request in a dispatch or batch. */
async function pick(
  python: string,
  root: string,
  requests: Record<string, unknown>[],
  identity: Identity,
  env: NodeJS.ProcessEnv,
  signal: AbortSignal,
): Promise<PickResult[]> {
  const configured = join(root, "scripts", "model_route.py");
  const script = existsSync(configured) ? configured : join(repositoryRoot, "scripts", "model_route.py");
  const output = await new Promise<string>((resolveOutput, rejectOutput) => {
    const child = execFile(python, [script, "pick"], {
      cwd: identity.cwd,
      env: withoutGitRedirects({ ...env, AGENT_FABRIC_PRODUCT_ROOT: root }),
      signal,
      killSignal: "SIGKILL",
      timeout: 20_000,
      maxBuffer: 1024 * 1024,
    }, (error, stdout) => (error ? rejectOutput(new Error("Route pools unavailable; check scripts/model_route.py.")) : resolveOutput(stdout)));
    child.stdin!.end(JSON.stringify({ requests }));
  });
  const parsed = JSON.parse(output) as { status?: string; results?: PickResult[]; fix?: string };
  if (parsed.status !== "ok" || !Array.isArray(parsed.results) || parsed.results.length !== requests.length)
    throw new Error(parsed.fix ?? "Route pools returned an invalid answer.");
  return parsed.results;
}

export interface Expanded {
  tasks: BatchTaskInput[];
  /** True when any task became a council of several members. */
  council: boolean;
  errors: Record<string, unknown>[];
  warnings: string[];
}

/**
 * Replace each pool request with one task per pick. A single or rotate pick
 * keeps the task id; a council becomes `<id>-1`..`<id>-N`, so the ordinary
 * batch machinery runs it and returns one result path per member.
 */
export async function expandPools(
  tasks: BatchTaskInput[],
  python: string,
  root: string,
  identity: Identity,
  env: NodeJS.ProcessEnv,
  signal: AbortSignal,
): Promise<Expanded> {
  const errors: Record<string, unknown>[] = [];
  const warnings: string[] = [];
  const settled = tasks.map((task, index) => {
    if (!usesPool(task)) return task;
    try {
      const result = settle(task);
      warnings.push(...result.warnings);
      return result.task;
    } catch (error) {
      if (!(error instanceof InputError)) throw error;
      errors.push({ task_id: task.id ?? `task-${index + 1}`, error: error.code, fix: error.fix });
      return undefined;
    }
  });
  const pending: Array<{ index: number; request: Record<string, unknown> }> = [];
  settled.forEach((task, index) => {
    if (task === undefined || !usesPool(task)) return;
    pending.push({ index, request: {
      ...Object.fromEntries(POOL_FIELDS.filter((field) => task[field] !== undefined).map((field) => [field, task[field]])),
      ...(task.adapter === undefined ? {} : { adapter: task.adapter }),
      ...(task.effort === undefined ? {} : { effort: task.effort }),
      ...(task.confidential === true ? { confidential: true } : {}),
      ...(nativeAdapter(identity) === undefined ? {} : { native: nativeAdapter(identity) }),
      project: identity.project,
    } });
  });
  const results = pending.length ? await pick(python, root, pending.map((item) => item.request), identity, env, signal) : [];
  const answers = new Map(pending.map((item, position) => [item.index, results[position]!]));
  let council = false;
  const expanded = settled.flatMap((task, index): BatchTaskInput[] => {
    if (task === undefined) return [];
    if (!usesPool(task)) return [task];
    const answer = answers.get(index);
    if (answer === undefined) return [];
    const id = task.id ?? `task-${index + 1}`;
    if (answer.status !== "ok") {
      errors.push({ task_id: id, error: answer.error, fix: answer.fix });
      return [];
    }
    if (task.native_default !== undefined) {
      warnings.push(`${task.native_default} taken from the route pool: ${nativeAdapter(identity)} models run as native subagents`);
    }
    warnings.push(...answer.warnings);
    const members = task.council !== undefined || task.models !== undefined;
    council ||= members;
    const { alias: _alias, native_default: _default, ...rest } = withoutPool(task);
    return answer.picks.map((choice, member) => ({
      ...rest,
      id: members ? `${id}-${member + 1}` : id,
      adapter: choice.adapter,
      model: choice.model,
      ...(choice.effort === undefined ? {} : { effort: choice.effort }),
      pick_reason: choice.reason,
    }));
  });
  return { tasks: expanded, council, errors, warnings: [...new Set(warnings)] };
}
