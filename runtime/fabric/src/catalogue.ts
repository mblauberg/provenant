/** Read the Python-owned merged routing snapshot, cached by source mtimes. */
import { execFile, spawnSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(import.meta.url);

export interface AdapterEntry {
  name: string;
  dispatch: "implemented" | "dormant" | "unsupported";
  endpoint_provider: string | null;
  fixed_model_family: string | null;
  effort_transport: string;
  latest_aliases?: boolean;
  aliases: Record<string, string[]>;
  models: string[];
  model_details: Array<{ id?: string; names?: string[]; [key: string]: unknown }>;
  read_only_guarantee?: string;
  write_modes?: string[];
  disabled_reason?: string;
  endpoint_adapters: string[];
}

export interface CatalogueSnapshot {
  schema?: "fabric.catalogue.v1";
  sha256?: string;
  sources?: string[];
  drift: string[];
  adapters: AdapterEntry[];
  endpoints: Record<string, { base_url: string; token_env: string; model_family: string; adapters: string[] }>;
}

const empty: CatalogueSnapshot = { adapters: [], endpoints: {}, drift: [] };
const cache = new Map<string, { stamp: string; value: CatalogueSnapshot }>();

function findProductRoot(): string {
  return resolve(packageRoot, "..", "..");
}

function sourceStamp(productRoot: string, instanceRoot: string, env: NodeJS.ProcessEnv): string {
  const stateRoot = env.AGENT_FABRIC_STATE_ROOT ?? join(homedir(), ".local", "state", "agent-harness", "fabric");
  const paths = [join(productRoot, "config", "model-routing.json"),
    join(instanceRoot, "config", "model-routing.json"),
    join(stateRoot, "capabilities.json"),
    join(productRoot, "config", "adapter-compatibility.yaml")];
  return `${Math.floor(Date.now() / 3_600_000)}|${paths
    .map((path) => {
      try {
        const stat = statSync(path);
        return `${path}:${stat.mtimeMs}:${stat.size}`;
      } catch {
        return `${path}:absent`;
      }
    }).join("|")}`;
}

export function catalogueSnapshot(root?: string, env: NodeJS.ProcessEnv = process.env): CatalogueSnapshot {
  const configuredRoot = root || env.AGENT_FABRIC_PRODUCT_ROOT;
  const productRoot = configuredRoot && isAbsolute(configuredRoot) ? configuredRoot : findProductRoot();
  const configuredInstance = env.AGENT_FABRIC_INSTANCE_ROOT || join(homedir(), ".agents");
  const instanceRoot = configuredInstance === "~" ? homedir()
    : configuredInstance.startsWith("~/") ? join(homedir(), configuredInstance.slice(2)) : configuredInstance;
  const stamp = sourceStamp(productRoot, instanceRoot, env);
  const key = `${productRoot}|${instanceRoot}`;
  const previous = cache.get(key);
  if (previous?.stamp === stamp) return previous.value;
  const configuredScript = join(productRoot, "scripts", "model_route.py");
  const script = existsSync(configuredScript) ? configuredScript : join(findProductRoot(), "scripts", "model_route.py");
  const failed = (message: string): CatalogueSnapshot => {
    const value = { ...empty, drift: [message] };
    cache.set(key, { stamp, value });
    return value;
  };
  if (!existsSync(script)) return failed("catalogue snapshot unavailable; fix: restore scripts/model_route.py");
  const configuredPython = env.HARNESS_PYTHON;
  const python = configuredPython && isAbsolute(configuredPython) ? configuredPython : "python3";
  const run = spawnSync(python, [script, "snapshot", "--json"], {
    cwd: productRoot,
    env: { ...process.env, ...env, AGENT_FABRIC_PRODUCT_ROOT: productRoot, AGENT_FABRIC_INSTANCE_ROOT: instanceRoot },
    encoding: "utf8",
    timeout: 10_000,
    maxBuffer: 4 * 1024 * 1024,
  });
  if (run.status !== 0) {
    const detail = ((run.stderr || run.error?.message || "unknown error").split("\n", 1)[0] ?? "unknown error").slice(0, 160);
    return failed(`catalogue snapshot unavailable: ${detail}; fix: check the harness Python installation`);
  }
  let routing: any;
  try { routing = JSON.parse(run.stdout); }
  catch { return failed("catalogue snapshot invalid; fix: check model-route"); }
  let compatibility: any;
  try {
    const { parse: parseYaml } = require("yaml") as { parse: (text: string) => unknown };
    compatibility = parseYaml(readFileSync(join(productRoot, "config", "adapter-compatibility.yaml"), "utf8"));
  } catch { compatibility = undefined; }
  const registry = (compatibility as { dispatch_registry?: Record<string, any> } | undefined)?.dispatch_registry ?? {};
  const routingAdapters = (routing.adapters ?? {}) as Record<string, any>;
  const families = (routing.families ?? {}) as Record<string, any>;
  const endpoints = (routing.endpoints ?? {}) as CatalogueSnapshot["endpoints"];
  const adapters: AdapterEntry[] = Object.entries(routingAdapters).map(([name, entry]) => {
    const registryEntry = registry[name] as Record<string, any> | undefined;
    const familyNames = entry.fixed_model_family != null
      ? [entry.fixed_model_family]
      : (entry.model_family_preferences?.preferred ?? []) as string[];
    const aliases: Record<string, string[]> = {};
    for (const family of familyNames) {
      for (const [alias, models] of Object.entries((families[family] as any)?.aliases ?? {})) {
        aliases[alias] = [...new Set([...(aliases[alias] ?? []), ...(models as string[])])];
      }
    }
    for (const [alias, models] of Object.entries(entry.aliases ?? {})) {
      aliases[alias] = [...new Set([...(aliases[alias] ?? []), ...(models as string[])])];
    }
    const modelDetails = Array.isArray(entry.models) ? entry.models : [];
    const models = [...new Set([...Object.values(aliases).flat(), ...modelDetails.map((model: any) => model.id)])];
    return {
      name,
      dispatch: registryEntry?.dispatch ?? "unsupported",
      endpoint_provider: entry.endpoint_provider ?? null,
      fixed_model_family: entry.fixed_model_family ?? null,
      effort_transport: entry.effort_transport ?? "flag",
      ...(entry.latest_aliases === true ? { latest_aliases: true } : {}),
      aliases,
      models,
      model_details: modelDetails,
      ...(registryEntry?.read_only_guarantee === undefined ? {} : { read_only_guarantee: registryEntry.read_only_guarantee }),
      ...(registryEntry?.write_modes === undefined ? {} : { write_modes: registryEntry.write_modes }),
      ...(registryEntry === undefined || registryEntry.dispatch === "implemented" ? {}
        : { disabled_reason: registryEntry.disabled_reason }),
      endpoint_adapters: Object.entries(endpoints)
        .filter(([, profile]) => (profile.adapters ?? []).includes(name)).map(([profileName]) => profileName),
    };
  });
  const value: CatalogueSnapshot = {
    schema: routing.schema,
    sha256: routing.sha256,
    sources: routing.sources,
    drift: [...(routing.drift ?? []), ...(routing.stale_alias_warnings ?? [])],
    adapters: adapters.sort((left, right) => left.name.localeCompare(right.name)),
    endpoints,
  };
  cache.set(key, { stamp, value });
  return value;
}

const LISTED: Record<string, string> = { agy: "agy", codex: "codex", cursor: "cursor-agent", kiro: "kiro-cli", opencode: "opencode" };
const FLAT_LIMIT = 24, GROUP_LIMIT = 20, OUTPUT_BUDGET = 4096;

/**
 * Live model discovery through the Python probe, which bounds the listing and
 * caches it for a day. The digest stays short: small lists are printed whole,
 * larger ones by family, and a large family only by count until `match` narrows it.
 */
export async function liveModels(adapter: string, options: LiveModelOptions = {}): Promise<{ digest: string }> {
  const digest = await liveDigest(adapter, options);
  // Every path is budgeted below; this is the backstop for an unexpectedly long head line.
  return { digest: digest.length <= OUTPUT_BUDGET ? digest : `${digest.slice(0, OUTPUT_BUDGET - 40)}\n… truncated; pass match to narrow` };
}

interface LiveModelOptions { match?: string; root?: string; env?: NodeJS.ProcessEnv; snapshot?: CatalogueSnapshot }

/** The catalogued ids as one line within `room` characters, naming what it leaves out. */
function cataloguedLine(models: string[], match: string | undefined, room: number): string {
  const needle = match?.toLowerCase();
  const chosen = needle ? models.filter((model) => model.toLowerCase().includes(needle)) : models;
  let line = needle ? `catalogued (${chosen.length} of ${models.length} match ${match}):` : "catalogued:";
  if (chosen.length === 0) return `${line} -`;
  let kept = 0;
  for (const id of chosen) {
    if (line.length + id.length + 1 > room - 48) break;
    line += ` ${id}`;
    kept += 1;
  }
  return kept < chosen.length ? `${line} ${chosen.length - kept} more omitted; pass match to narrow` : line;
}

async function liveDigest(adapter: string, options: LiveModelOptions): Promise<string> {
  const env = options.env ?? process.env;
  const snapshot = options.snapshot ?? catalogueSnapshot(options.root, env);
  const entry = snapshot.adapters.find((candidate) => candidate.name === adapter);
  if (entry === undefined) {
    return `${adapter}: unknown adapter; known: ${snapshot.adapters.map((item) => item.name).sort().join(", ")}`;
  }
  const withCatalogue = (head: string) => `${head}\n${cataloguedLine(entry.models, options.match, OUTPUT_BUDGET - head.length - 1)}`;
  if (adapter === "claude") return withCatalogue("claude: no live list; run Claude models as native subagents (Agent tool)");
  const executable = LISTED[adapter];
  if (executable === undefined) return withCatalogue(`${adapter}: no live list`);
  const configuredRoot = options.root || env.AGENT_FABRIC_PRODUCT_ROOT;
  const productRoot = configuredRoot && isAbsolute(configuredRoot) ? configuredRoot : findProductRoot();
  const configuredPython = env.HARNESS_PYTHON;
  const python = configuredPython && isAbsolute(configuredPython) ? configuredPython : "python3";
  const stdout = await new Promise<string>((resolveOutput) => {
    execFile(python, [join(productRoot, "scripts", "model_route.py"), "probe", "--adapter", adapter, "--executable", executable, "--listing-only"],
      // The listing-only probe bounds each child within its own 30-second deadline, inside this one.
      { cwd: productRoot, env: { ...process.env, ...env }, encoding: "utf8", timeout: 40_000, maxBuffer: 4 * 1024 * 1024 },
      (_error, output) => resolveOutput(output ?? ""));
  });
  let record: { models?: unknown; message?: string; status?: string; cache_hit?: boolean } = {};
  try { record = JSON.parse(stdout); } catch { /* reported below */ }
  const listed = Array.isArray(record.models) ? record.models.filter((model): model is string => typeof model === "string") : [];
  if (listed.length === 0) {
    return withCatalogue(`${adapter}: live list unavailable (${String(record.message ?? record.status ?? "probe failed").slice(0, 200)})`);
  }
  const needle = options.match?.toLowerCase();
  const models = needle ? listed.filter((model) => model.toLowerCase().includes(needle)) : listed;
  const cached = record.cache_hit ? " (cached)" : "";
  const head = needle ? `${adapter}: ${models.length} of ${listed.length} live models match ${options.match}${cached}`
    : `${adapter}: ${listed.length} live models${cached}`;
  let lines: string[];
  if (models.length <= FLAT_LIMIT) {
    lines = models.length ? [models.join(" ")] : [];
  } else {
    const groups = new Map<string, string[]>();
    for (const model of models) {
      const cut = model.includes("/") ? model.indexOf("/") : model.indexOf("-");
      const group = cut > 0 ? model.slice(0, cut + 1) : "";
      groups.set(group, [...(groups.get(group) ?? []), model.slice(group.length)]);
    }
    lines = [...groups].map(([group, names]) =>
      `${group || "other"} (${names.length}): ${names.length > GROUP_LIMIT ? "pass match to list" : names.join(" ")}`);
  }
  const tail = `dispatch any as model "${adapter}/<id>"; an uncatalogued id runs with a note`;
  // Whole lines only, within one budget; a long flat line collapses to its count.
  const reserve = head.length + tail.length + 80;
  const kept: string[] = [];
  let used = reserve, omittedModels = 0;
  for (const line of lines) {
    if (used + line.length + 1 <= OUTPUT_BUDGET) { kept.push(line); used += line.length + 1; continue; }
    omittedModels += models.length <= FLAT_LIMIT ? models.length : Number(/\((\d+)\)/u.exec(line)?.[1] ?? 0);
  }
  const omitted = lines.length - kept.length;
  const note = omitted === 0 ? [] : [models.length <= FLAT_LIMIT
    ? `${omittedModels} models omitted; pass match to narrow`
    : `${omitted} more groups (${omittedModels} models) omitted; pass match to narrow`];
  return [head, ...kept, ...note, tail].join("\n");
}
