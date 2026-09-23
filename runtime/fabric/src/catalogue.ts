/**
 * Read-only access to the instance routing catalogue and product adapter registry.
 *
 * `dispatch_registry` in config/adapter-compatibility.yaml is the product-owned
 * source for which adapters exist and what the dispatcher does with them;
 * config/model-routing.json carries the user-owned alias/model routing that
 * hangs off those adapters. Routing comes from the instance, with a product fallback only when absent; neither
 * read is required to open the store or serve the mailbox tools, so every
 * failure mode here is a typed error on the asking tool call, never a server
 * startup failure (the packed-package smoke runs with no product root at all).
 */
import { createRequire } from "node:module";
import { lstatSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

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
  read_only_guarantee?: string;
  write_modes?: string[];
  disabled_reason?: string;
  endpoint_adapters: string[];
}

export interface CatalogueSnapshot {
  adapters: AdapterEntry[];
  endpoints: Record<string, { base_url: string; token_env: string; model_family: string; adapters: string[] }>;
}

function findProductRoot(): string | undefined {
  const configured = process.env.AGENT_FABRIC_PRODUCT_ROOT;
  if (configured !== undefined && configured.trim().length > 0) {
    return resolve(configured);
  }
  // The product checkout is three levels above the fabric package inside the
  // repository; an installed package has no such parent and stays catalogue-free.
  const candidate = resolve(packageRoot, "..", "..");
  if (configured === undefined && candidate !== packageRoot) {
    return candidate;
  }
  return undefined;
}

export function catalogueSnapshot(root?: string, env: NodeJS.ProcessEnv = process.env): CatalogueSnapshot {
  const productRoot = root ?? env.AGENT_FABRIC_PRODUCT_ROOT ?? findProductRoot() ?? packageRoot;
  const configuredInstance = env.AGENT_FABRIC_INSTANCE_ROOT || join(homedir(), ".agents");
  const instanceRoot = configuredInstance === "~" ? homedir()
    : configuredInstance.startsWith("~/") ? join(homedir(), configuredInstance.slice(2)) : configuredInstance;
  const instancePath = join(instanceRoot, "config", "model-routing.json");
  let routingPath = instancePath;
  try { lstatSync(instancePath); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") routingPath = join(productRoot, "config", "model-routing.json");
  }
  const compatibilityPath = join(productRoot, "config", "adapter-compatibility.yaml");
  const empty: CatalogueSnapshot = { adapters: [], endpoints: {} };
  let routing: any;
  let compatibility: any;
  try {
    routing = JSON.parse(readFileSync(routingPath, "utf8"));
  } catch {
    return empty;
  }
  try {
    // A YAML parser is not a fabric runtime dependency; the compatibility file
    // is only consulted when present. The registry test owns its schema.
    const { parse: parseYaml } = require("yaml") as { parse: (text: string) => unknown };
    compatibility = parseYaml(readFileSync(compatibilityPath, "utf8"));
  } catch {
    compatibility = undefined;
  }
  const registry = (compatibility as { dispatch_registry?: Record<string, any> } | undefined)
    ?.dispatch_registry ?? {};
  const routingAdapters = (routing?.adapters ?? {}) as Record<string, any>;
  const families = (routing?.families ?? {}) as Record<string, any>;
  const adapters: AdapterEntry[] = Object.entries(routingAdapters).map(([name, entry]) => {
    const registryEntry = registry?.[name] as Record<string, any> | undefined;
    // A fixed-family adapter inherits that family's alias table. A multi-family
    // adapter (cursor, agy) declares preferred families instead; its aliases are
    // the union over those families, which is what model_route resolves from.
    const familyNames = (entry as any).fixed_model_family !== null
      ? [(entry as any).fixed_model_family]
      : (((entry as any).model_family_preferences?.preferred ?? []) as string[]);
    const aliases: Record<string, string[]> = {};
    for (const family of familyNames) {
      for (const [alias, models] of Object.entries(
        (families[family] as any)?.aliases ?? {},
      )) {
        aliases[alias] = [...new Set([...(aliases[alias] ?? []), ...(models as string[])])];
      }
    }
    return {
      name,
      // Dispatch state is product-owned: the registry in
      // config/adapter-compatibility.yaml is the single writer. The routing
      // catalogue carries no dispatch field; an adapter missing from the
      // registry is unsupported by definition.
      dispatch: ((registryEntry as { dispatch?: AdapterEntry["dispatch"] } | undefined)?.dispatch
        ?? "unsupported") as AdapterEntry["dispatch"],
      endpoint_provider: ((entry as any).endpoint_provider ?? null) as string | null,
      fixed_model_family: ((entry as any).fixed_model_family ?? null) as string | null,
      effort_transport: ((entry as any).effort_transport ?? "flag") as string,
      aliases,
      models: [...new Set(Object.values(aliases).flat())],
      ...(registryEntry?.read_only_guarantee === undefined
        ? {}
        : { read_only_guarantee: registryEntry.read_only_guarantee as string }),
      ...(registryEntry?.write_modes === undefined
        ? {}
        : { write_modes: registryEntry.write_modes as string[] }),
      ...(registryEntry === undefined || registryEntry.dispatch === "implemented"
        ? {}
        : { disabled_reason: (registryEntry as any).disabled_reason as string | undefined }),
      endpoint_adapters: Object.entries((routing?.endpoints ?? {}) as Record<string, any>)
        .filter(([, profile]) => ((profile as any).adapters ?? []).includes(name))
        .map(([profileName]) => profileName),
    };
  });
  return {
    adapters: adapters.sort((left, right) => left.name.localeCompare(right.name)),
    endpoints: (routing?.endpoints ?? {}) as CatalogueSnapshot["endpoints"],
  };
}
