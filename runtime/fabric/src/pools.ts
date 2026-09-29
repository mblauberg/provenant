/** Expand `route`, `rotate`, `council` and `models` into concrete adapter/model tasks (#848). */
import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { withoutGitRedirects, type Identity } from "./identity.js";
import { InputError, type BatchTaskInput, type RouteInput } from "./execution-input.js";

export const POOL_FIELDS = ["route", "rotate", "council", "models", "confidential"] as const;
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
  return POOL_FIELDS.some((field) => input[field] !== undefined && input[field] !== false);
}

function withoutPool<T extends RouteInput>(input: T): T {
  return Object.fromEntries(Object.entries(input).filter(([key]) =>
    !(POOL_FIELDS as readonly string[]).includes(key))) as T;
}

function validate(input: RouteInput): void {
  if (input.model !== undefined || input.alias !== undefined)
    throw new InputError("route_conflict", "Pass route or models, or pass alias/model, not both.");
  if (input.route === undefined && input.models === undefined)
    throw new InputError("route_required", "Pass route with rotate, council or confidential.");
  if (input.models !== undefined && (!Array.isArray(input.models) || input.models.some((item) => typeof item !== "string")))
    throw new InputError("models_invalid", "Pass models as a list of adapter/model[@effort] strings.");
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
  const pending: Array<{ index: number; request: Record<string, unknown> }> = [];
  tasks.forEach((task, index) => {
    if (!usesPool(task)) return;
    try {
      validate(task);
      pending.push({ index, request: {
        ...Object.fromEntries(POOL_FIELDS.filter((field) => task[field] !== undefined).map((field) => [field, task[field]])),
        ...(task.adapter === undefined ? {} : { adapter: task.adapter }),
        ...(task.effort === undefined ? {} : { effort: task.effort }),
        project: identity.project,
      } });
    } catch (error) {
      if (!(error instanceof InputError)) throw error;
      errors.push({ task_id: task.id ?? `task-${index + 1}`, error: error.code, fix: error.fix });
    }
  });
  const results = pending.length ? await pick(python, root, pending.map((item) => item.request), identity, env, signal) : [];
  const answers = new Map(pending.map((item, position) => [item.index, results[position]!]));
  const warnings: string[] = [];
  const expanded = tasks.flatMap((task, index): BatchTaskInput[] => {
    if (!usesPool(task)) return [task];
    const answer = answers.get(index);
    if (answer === undefined) return [];
    const id = task.id ?? `task-${index + 1}`;
    if (answer.status !== "ok") {
      errors.push({ task_id: id, error: answer.error, fix: answer.fix });
      return [];
    }
    warnings.push(...answer.warnings);
    const council = task.council !== undefined || task.models !== undefined;
    return answer.picks.map((choice, member) => ({
      ...withoutPool(task),
      id: council ? `${id}-${member + 1}` : id,
      adapter: choice.adapter,
      model: choice.model,
      ...(choice.effort === undefined ? {} : { effort: choice.effort }),
      pick_reason: choice.reason,
    }));
  });
  return { tasks: expanded, errors, warnings: [...new Set(warnings)] };
}
