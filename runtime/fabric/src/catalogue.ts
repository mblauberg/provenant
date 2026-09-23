/** Read the Python-owned merged routing snapshot, cached by source mtimes. */
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
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
  aliases: Record<string, string[]>;
  models: string[];
  model_details: Record<string, unknown>[];
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
  return [join(productRoot, "config", "model-routing.json"),
    join(instanceRoot, "config", "model-routing.json"),
    join(stateRoot, "capabilities.json"),
    join(productRoot, "config", "adapter-compatibility.yaml")]
    .map((path) => {
      try {
        const stat = statSync(path);
        return `${path}:${stat.mtimeMs}:${stat.size}`;
      } catch {
        return `${path}:absent`;
      }
    }).join("|");
}

export function catalogueSnapshot(root?: string, env: NodeJS.ProcessEnv = process.env): CatalogueSnapshot {
  const productRoot = root ?? env.AGENT_FABRIC_PRODUCT_ROOT ?? findProductRoot();
  const configuredInstance = env.AGENT_FABRIC_INSTANCE_ROOT || join(homedir(), ".agents");
  const instanceRoot = configuredInstance === "~" ? homedir()
    : configuredInstance.startsWith("~/") ? join(homedir(), configuredInstance.slice(2)) : configuredInstance;
  const stamp = sourceStamp(productRoot, instanceRoot, env);
  const key = `${productRoot}|${instanceRoot}`;
  const previous = cache.get(key);
  if (previous?.stamp === stamp) return previous.value;
  const configuredScript = join(productRoot, "scripts", "model_route.py");
  const script = existsSync(configuredScript) ? configuredScript : join(findProductRoot(), "scripts", "model_route.py");
  if (!existsSync(script)) return empty;
  const run = spawnSync(env.HARNESS_PYTHON || "python3", [script, "snapshot", "--json"], {
    cwd: productRoot,
    env: { ...process.env, ...env, AGENT_FABRIC_PRODUCT_ROOT: productRoot, AGENT_FABRIC_INSTANCE_ROOT: instanceRoot },
    encoding: "utf8",
    timeout: 10_000,
    maxBuffer: 4 * 1024 * 1024,
  });
  if (run.status !== 0) return { ...empty, drift: ["catalogue snapshot unavailable; fix: check the harness Python installation"] };
  let routing: any;
  try { routing = JSON.parse(run.stdout); }
  catch { return { ...empty, drift: ["catalogue snapshot invalid; fix: check model-route"] }; }
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
