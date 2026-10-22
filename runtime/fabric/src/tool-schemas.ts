import { z } from "zod";

import { ACCESS_MODES, DELIVERY_RUN_ID } from "./execution-input.js";

const detail = z.enum(["brief", "full"]).optional();
const str = z.string().optional();
const ids = (maximum?: number) => z.union([
  maximum === undefined ? z.array(z.string()) : z.array(z.string()).max(maximum), z.string(),
]).transform((value) => typeof value === "string" ? [value] : value).optional();
const wait = z.unknown().optional();
const optionalNumber = z.unknown().optional();
const route = {
  delivery_run_id: z.string().regex(DELIVERY_RUN_ID).optional(),
  host: str,
  operation_id: str,
  adapter: str,
  alias: str,
  model: str.meta({ description: "A model name or catalogue alias (for example sonnet, sol, luna, deepseek-free, glm-free); the most predictable selector. adapter/model also works." }),
  effort: str,
  account: str.meta({ description: "Claude account label from accounts.claude in telemetry configuration." }),
  mode: z.enum(ACCESS_MODES).optional(),
  worktree: str,
  cwd: str,
  network: z.boolean().optional(),
  sandbox: z.enum(["read-only", "workspace-write", "full"]).optional(),
  capabilities: z.array(z.enum(["postgres", "browser"])).refine((items) => new Set(items).size === items.length, {
    message: "Pass capabilities as a list of distinct postgres or browser values.",
  }).optional(),
  add_dirs: z.array(z.string()).optional(),
  allow_secrets: z.boolean().optional(),
  fallback: z
    .union([z.boolean(), z.literal("any"), z.array(z.union([z.string(), z.record(z.string(), z.unknown())]))])
    .optional(),
  route: z.string().optional().meta({ description: "Tier pool: junior (laid-out work, cheap lenses), senior (judgement, review, delegation) or principal (critical decisions); or design or writing. adapter narrows the pool; an explicit model wins over route. Prefer naming a model (deepseek-free, glm-free, sonnet, sol) when you know what you want." }),
  rotate: z.boolean().optional().meta({ description: "Cycle the route's pool per project instead of taking its top model; ignored with council." }),
  council: z.number().int().min(1).max(8).optional().meta({ description: "Run 1-8 members from the route as one batch, families spread first; members repeat when fewer are available." }),
  models: z.array(z.string()).min(1).max(8).optional().meta({ description: "Ad-hoc council of adapter/model[@effort] entries; bypasses route and adapter." }),
  confidential: z.boolean().optional().meta({ description: "Never use a prompt-training model, fallbacks included (free models count unless their entry says they do not train); works with any selector." }),
};
export const taskShape = { prompt: str, prompt_file: str, timeout_seconds: optionalNumber, ...route };
export const taskFields = { id: str, ...taskShape };
const batch = {
  tasks: z.array(z.object(taskFields).catchall(z.unknown()).meta({ additionalProperties: false })).min(1).max(64),
  concurrency: optionalNumber,
  wait_seconds: wait,
};

// These shapes are the source used by server.ts registrations and the schema export.
export const toolSchemas = {
  fabric_whoami: { detail },
  fabric_send: { to: z.string(), body: z.string(), kind: str, reply_to: str, task_id: str, output_path: str },
  fabric_inbox: {
    peek: z.boolean().optional(), task_id: str, claim_seconds: z.number().int().min(1).max(3600).optional(),
    ids: ids(100), claim: z.boolean().optional(), limit: z.number().int().min(1).max(100).optional(),
    wait_seconds: wait, digest: z.boolean().optional(),
  },
  fabric_runs: { ids: ids(20), wait_seconds: wait, state: str, limit: z.number().int().min(1).max(200).optional(), detail },
  fabric_events: { cursor: z.string().max(8192).optional(), wait_seconds: wait },
  fabric_dispatch: {
    ...taskShape, task_id: str, tasks: batch.tasks.optional(), concurrency: batch.concurrency, resume: str,
    handoff: str, session: str, fresh: z.boolean().optional(), context_ceiling: z.number().optional(), wait_seconds: wait, detail,
  },
  fabric_status: {
    ids: ids(), id: str, wait_seconds: wait, until: z.enum(["any", "all"]).optional(), detail,
    tail_chars: z.number().int().min(0).max(4000).optional(), fields: z.array(z.string()).optional(),
  },
  fabric_work_claim: {
    action: z.enum(["acquire", "renew", "verify", "release"]), session_id: z.string().min(1), issue: str,
    paths: z.array(z.string()).optional(), id: str, generation: z.number().int().positive().optional(), seconds: z.number().int().min(1).optional(),
  },
  fabric_landing_lease: {
    action: z.enum(["acquire", "renew", "verify", "release", "wait"]), session_id: z.string().min(1),
    expected_sha: str, generation: z.number().int().positive().optional(), seconds: z.number().int().min(1).max(3600).optional(), wait_seconds: z.number().optional(),
  },
  fabric_session: { action: z.enum(["inspect", "list", "forget"]), name: str },
  fabric_cancel: { id: z.string(), reason: str, operation_id: str, detail },
  fabric_graph: {
    action: z.enum(["run", "status", "cancel", "resume"]), file: str, document: z.record(z.string(), z.unknown()).optional(),
    id: str, node: str, force: z.boolean().optional(), wait_seconds: wait, detail,
  },
  fabric_fetch: { id: z.string() },
  fabric_output: {
    id: z.string(), part: z.enum(["result", "stderr", "events", "receipt"]).optional(), offset: z.number().int().nonnegative().optional(),
    tail: z.boolean().optional(), max_bytes: optionalNumber,
  },
  fabric_acknowledge: { message_id: z.string(), claim_id: z.string() },
  fabric_task: {
    action: z.enum(["create", "claim", "update", "list"]), task_id: str, objective: str, owner: str,
    depends_on: z.array(z.string()).optional(), state: str, note: str,
  },
  fabric_note: { detail: z.string() },
  fabric_activity: { limit: z.number().int().min(1).max(100).optional(), after_seq: z.number().int().nonnegative().optional() },
  fabric_hosts: { action: z.enum(["list", "doctor"]).default("list"), hosts: z.array(z.string()).optional() },
  fabric_adapters: {
    detail, models: z.string().max(40).optional().describe("Adapter whose live models to list (cached probe)."),
    match: z.string().max(80).optional().describe("Substring filter for models."),
    action: z.enum(["reset"]).optional().describe("reset clears cooldowns (selector: adapter or adapter/model; omit for all); use after the user says usage is back or switches accounts."),
    selector: z.string().min(1).max(80).optional(),
  },
};
