import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, extname, join, relative, resolve, sep } from "node:path";

import { ImportType, initSync, parse } from "es-module-lexer";
import { describe, expect, it } from "vitest";

type DomainEdge = Readonly<{ from: string; to: string }>;

type ReciprocalAllowance = Readonly<{
  id: string;
  members: readonly string[];
  reason: string;
}>;

type ComputedImportAllowance = Readonly<{
  id: string;
  import: string;
  reason: string;
}>;

type BoundaryGolden = Readonly<{
  schema_version: 2;
  requirement: "F-033";
  allowed_edge_count: number;
  allowed_edges: readonly DomainEdge[];
  reciprocal_scc_count: number;
  temporary_computed_imports: readonly ComputedImportAllowance[];
  temporary_reciprocal_edges: readonly ReciprocalAllowance[];
  layer_order_needed: string;
}>;

const sourceRoot = resolve(import.meta.dirname, "../../src");
const goldenPath = resolve(import.meta.dirname, "../fixtures/f033-module-boundaries.json");
const layerOrderNeeded = "ADR-0003";
let goldenRegeneratedForRun: BoundaryGolden | undefined;
initSync();

function productionFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true })
    .flatMap((entry) => {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) return productionFiles(path);
      return entry.isFile() && entry.name.endsWith(".ts") && !entry.name.endsWith(".d.ts")
        ? [path]
        : [];
    })
    .sort();
}

function exposeTypeOnlyReexports(source: string): string {
  const trivia = String.raw`(?:\s|\/\*[\s\S]*?\*\/|\/\/[^\r\n]*(?:\r?\n|$))`;
  const typeReexport = new RegExp(
    String.raw`\bexport(${trivia}+)type(${trivia}*)(?=[{*])`,
    "gu",
  );
  return source.replace(
    typeReexport,
    (_match: string, before: string, after: string) => `export${before}    ${after}`,
  );
}

function importSpecifiers(path: string): {
  computed: string[];
  literal: string[];
} {
  const source = readFileSync(path, "utf8");
  const imports = parse(source, path)[0];
  const typeReexports = parse(exposeTypeOnlyReexports(source), `${path}#type-reexports`)[0];
  return {
    computed: imports
      .filter(({ n, t }) => t === ImportType.Dynamic && n === undefined)
      .map(({ s, e }) => source.slice(s, e)),
    literal: [...new Set([
      ...imports.flatMap(({ n }) => n === undefined ? [] : [n]),
      ...typeReexports.flatMap(({ n }) => n === undefined ? [] : [n]),
    ])],
  };
}

function resolveProductionImport(importer: string, specifier: string): string | undefined {
  if (!specifier.startsWith(".")) return undefined;
  const cleanSpecifier = specifier.split(/[?#]/u, 1)[0];
  if (cleanSpecifier === undefined) return undefined;
  const unresolved = resolve(dirname(importer), cleanSpecifier);
  const candidates = extname(unresolved) === ".js"
    ? [unresolved.slice(0, -3) + ".ts"]
    : extname(unresolved) === ".ts"
      ? [unresolved]
      : [`${unresolved}.ts`, join(unresolved, "index.ts")];
  return candidates.find((candidate) => productionFileSet.has(candidate));
}

function domainOf(path: string): string {
  const parts = relative(sourceRoot, path).split(sep);
  if (parts.length > 1) return parts[0] ?? "unknown";
  if (parts[0] === "index.ts") return "api";
  if (parts[0] === "errors.ts") return "err";
  return (parts[0] ?? "unknown").replace(/\.ts$/u, "");
}

function edgeKey(edge: DomainEdge): string {
  return `${edge.from} -> ${edge.to}`;
}

function sccKey(members: readonly string[]): string {
  return [...members].sort().join(", ");
}

const productionFileSet = new Set(productionFiles(sourceRoot));

function inspectGraph(): {
  edges: DomainEdge[];
  computedImports: string[];
  inwardApiImports: string[];
  reciprocalSccs: string[][];
  unresolvedImports: string[];
} {
  const edgeKeys = new Set<string>();
  const computedImports: string[] = [];
  const inwardApiImports: string[] = [];
  const unresolvedImports: string[] = [];

  for (const importer of productionFileSet) {
    const imports = importSpecifiers(importer);
    computedImports.push(...imports.computed.map((expression) =>
      `${relative(sourceRoot, importer)} imports ${expression}`));
    for (const specifier of imports.literal) {
      const location = `${relative(sourceRoot, importer)} imports ${specifier}`;
      if (specifier === "@local/agent-fabric") {
        inwardApiImports.push(location);
        continue;
      }
      if (!specifier.startsWith(".")) continue;
      const imported = resolveProductionImport(importer, specifier);
      if (imported === undefined) {
        unresolvedImports.push(location);
        continue;
      }
      if (imported === join(sourceRoot, "index.ts") && importer !== imported) {
        inwardApiImports.push(location);
      }
      const from = domainOf(importer);
      const to = domainOf(imported);
      if (from !== to) edgeKeys.add(edgeKey({ from, to }));
    }
  }

  const edges = [...edgeKeys]
    .sort()
    .map((key) => {
      const [from, to] = key.split(" -> ");
      if (from === undefined || to === undefined) throw new Error(`invalid edge ${key}`);
      return { from, to };
    });
  return {
    edges,
    computedImports: computedImports.sort(),
    inwardApiImports: inwardApiImports.sort(),
    reciprocalSccs: stronglyConnectedComponents(edges),
    unresolvedImports: unresolvedImports.sort(),
  };
}

function stronglyConnectedComponents(edges: readonly DomainEdge[]): string[][] {
  const adjacency = new Map<string, string[]>();
  for (const { from, to } of edges) {
    const neighbours = adjacency.get(from);
    if (neighbours === undefined) {
      adjacency.set(from, [to]);
    } else {
      neighbours.push(to);
    }
    if (!adjacency.has(to)) adjacency.set(to, []);
  }

  let nextIndex = 0;
  const indices = new Map<string, number>();
  const lowLinks = new Map<string, number>();
  const stack: string[] = [];
  const onStack = new Set<string>();
  const components: string[][] = [];

  const visit = (domain: string): void => {
    const index = nextIndex;
    nextIndex += 1;
    indices.set(domain, index);
    lowLinks.set(domain, index);
    stack.push(domain);
    onStack.add(domain);

    for (const neighbour of adjacency.get(domain) ?? []) {
      const neighbourIndex = indices.get(neighbour);
      if (neighbourIndex === undefined) {
        visit(neighbour);
        const lowLink = lowLinks.get(domain);
        const neighbourLowLink = lowLinks.get(neighbour);
        if (lowLink === undefined || neighbourLowLink === undefined) {
          throw new Error(`missing Tarjan index for ${domain} or ${neighbour}`);
        }
        lowLinks.set(domain, Math.min(lowLink, neighbourLowLink));
      } else if (onStack.has(neighbour)) {
        const lowLink = lowLinks.get(domain);
        if (lowLink === undefined) throw new Error(`missing Tarjan low-link for ${domain}`);
        lowLinks.set(domain, Math.min(lowLink, neighbourIndex));
      }
    }

    if (lowLinks.get(domain) !== indices.get(domain)) return;
    const component: string[] = [];
    while (true) {
      const member = stack.pop();
      if (member === undefined) throw new Error("Tarjan stack underflow");
      onStack.delete(member);
      component.push(member);
      if (member === domain) break;
    }
    if (component.length > 1) components.push(component.sort());
  };

  for (const domain of [...adjacency.keys()].sort()) {
    if (!indices.has(domain)) visit(domain);
  }
  return components.sort((left, right) => sccKey(left).localeCompare(sccKey(right)));
}

function readGolden(): BoundaryGolden {
  return JSON.parse(readFileSync(goldenPath, "utf8")) as BoundaryGolden;
}

function sccId(members: readonly string[]): string {
  return `SCC-${members.map((member) => member.toUpperCase()).join("-")}`;
}

function regeneratedGolden(graph: ReturnType<typeof inspectGraph>, golden: BoundaryGolden): BoundaryGolden {
  return {
    schema_version: 2,
    requirement: "F-033",
    allowed_edge_count: graph.edges.length,
    allowed_edges: graph.edges,
    reciprocal_scc_count: graph.reciprocalSccs.length,
    temporary_computed_imports: golden.temporary_computed_imports,
    temporary_reciprocal_edges: graph.reciprocalSccs.map((members) => ({
      id: sccId(members),
      members,
      reason: `Extracted from SCC analysis: these ${members.length} domains form a cyclic dependency to be addressed`,
    })),
    // ADR-0003 calls for extraction along existing seams, not a horizontal layer model.
    // A layer order needs an explicit architectural decision before this test can enforce one.
    layer_order_needed: layerOrderNeeded,
  };
}

function maybeRegenerateGolden(graph: ReturnType<typeof inspectGraph>, golden: BoundaryGolden): BoundaryGolden {
  if (process.env.F033_WRITE_GOLDEN !== "1") return golden;
  if (goldenRegeneratedForRun !== undefined) return goldenRegeneratedForRun;
  const regenerated = regeneratedGolden(graph, golden);
  writeFileSync(goldenPath, `${JSON.stringify(regenerated, null, 2)}\n`, "utf8");
  process.stdout.write(
    `f033-module-boundaries: regenerated golden with ${regenerated.allowed_edge_count} allowed edges, ${regenerated.reciprocal_scc_count} SCCs`,
  );
  process.stdout.write("\n");
  goldenRegeneratedForRun = regenerated;
  return regenerated;
}

describe("F-033 Agent Fabric module boundaries", () => {
  it("resolves every production import and keeps the package API outward-only", () => {
    const graph = inspectGraph();
    const golden = readGolden();
    expect(graph.unresolvedImports, "unresolved production imports").toEqual([]);
    expect(graph.inwardApiImports, "src must not import the package entry point").toEqual([]);
    const computedImports = golden.temporary_computed_imports.map((allowance) => allowance.import);
    expect(graph.computedImports, "new or stale computed production imports").toEqual(computedImports);
    expect(new Set(computedImports).size).toBe(computedImports.length);
    for (const allowance of golden.temporary_computed_imports) {
      expect(allowance.id).toMatch(/^TEMP-[A-Z0-9-]+$/u);
      expect(allowance.reason.trim().length).toBeGreaterThan(0);
      expect(allowance.reason).not.toMatch(/[\r\n]/u);
    }
  });

  it("rejects edges outside the committed domain-to-domain edge set", () => {
    const graph = inspectGraph();
    const golden = maybeRegenerateGolden(graph, readGolden());
    expect(golden.schema_version).toBe(2);
    expect(golden.requirement).toBe("F-033");
    expect(golden.allowed_edge_count).toBe(golden.allowed_edges.length);
    const allowedEdgeKeys = golden.allowed_edges.map(edgeKey);
    expect(new Set(allowedEdgeKeys).size).toBe(allowedEdgeKeys.length);
    expect(graph.edges.every((edge) => allowedEdgeKeys.includes(edgeKey(edge)))).toBe(true);
    const graphEdgeKeys = new Set(graph.edges.map(edgeKey));
    const staleEdges = golden.allowed_edges.filter((edge) => !graphEdgeKeys.has(edgeKey(edge)));
    if (staleEdges.length > 0) {
      process.stdout.write(
        `f033-module-boundaries: stale edges in golden (removed from graph): ${staleEdges.map((edge) => `${edge.from} → ${edge.to}`).join(", ")}. Run: F033_WRITE_GOLDEN=1 npx vitest run\n`,
      );
    }
  });

  it("keeps every cyclic domain component visible as a named temporary allowance", () => {
    const graph = inspectGraph();
    const golden = maybeRegenerateGolden(graph, readGolden());
    const allowanceIds = golden.temporary_reciprocal_edges.map(({ id }) => id);
    expect(new Set(allowanceIds).size).toBe(allowanceIds.length);
    expect(golden.reciprocal_scc_count).toBe(golden.temporary_reciprocal_edges.length);
    for (const allowance of golden.temporary_reciprocal_edges) {
      expect(allowance.id).toBe(sccId(allowance.members));
      expect(allowance.reason.trim().length).toBeGreaterThan(0);
      expect(allowance.reason).not.toMatch(/[\r\n]/u);
      expect([...allowance.members].sort()).toEqual(allowance.members);
    }
    const allowanceSccs = golden.temporary_reciprocal_edges
      .map(({ members }) => sccKey(members))
      .sort();
    expect(new Set(allowanceSccs).size).toBe(allowanceSccs.length);
    expect(graph.reciprocalSccs.map(sccKey)).toEqual(allowanceSccs);
    // No layer direction is asserted until the named architecture decision defines one.
    expect(golden.layer_order_needed).toMatch(/^(ADR-\d+|#\d+)$/u);
  });
});
