import { readdirSync, readFileSync } from "node:fs";
import { dirname, extname, join, relative, resolve, sep } from "node:path";

import { ImportType, initSync, parse } from "es-module-lexer";
import { describe, expect, it } from "vitest";

type DomainEdge = Readonly<{ from: string; to: string }>;

type ReciprocalAllowance = Readonly<{
  id: string;
  domains: readonly [string, string];
  reason: string;
}>;

type ComputedImportAllowance = Readonly<{
  id: string;
  import: string;
  reason: string;
}>;

type BoundaryGolden = Readonly<{
  schema_version: 1;
  requirement: "F-033";
  allowed_edges: readonly DomainEdge[];
  temporary_computed_imports: readonly ComputedImportAllowance[];
  temporary_reciprocal_edges: readonly ReciprocalAllowance[];
}>;

const sourceRoot = resolve(import.meta.dirname, "../../src");
const goldenPath = resolve(import.meta.dirname, "../fixtures/f033-module-boundaries.json");
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

function pairKey(domains: readonly [string, string]): string {
  return [...domains].sort().join(" <-> ");
}

const productionFileSet = new Set(productionFiles(sourceRoot));

function inspectGraph(): {
  edges: DomainEdge[];
  computedImports: string[];
  inwardApiImports: string[];
  reciprocalPairs: string[];
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
  const reciprocalPairs = edges
    .filter(({ from, to }) => edgeKeys.has(edgeKey({ from: to, to: from })))
    .map(({ from, to }) => pairKey([from, to]))
    .filter((key, index, keys) => keys.indexOf(key) === index)
    .sort();

  return {
    edges,
    computedImports: computedImports.sort(),
    inwardApiImports: inwardApiImports.sort(),
    reciprocalPairs,
    unresolvedImports: unresolvedImports.sort(),
  };
}

function readGolden(): BoundaryGolden {
  return JSON.parse(readFileSync(goldenPath, "utf8")) as BoundaryGolden;
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

  it("allows only the committed domain-to-domain edge set", () => {
    const graph = inspectGraph();
    const golden = readGolden();
    expect(golden.schema_version).toBe(1);
    expect(golden.requirement).toBe("F-033");
    expect(graph.edges).toEqual(golden.allowed_edges);
  });

  it("keeps every reciprocal edge visible as a named temporary allowance", () => {
    const graph = inspectGraph();
    const golden = readGolden();
    const allowanceIds = golden.temporary_reciprocal_edges.map(({ id }) => id);
    expect(new Set(allowanceIds).size).toBe(allowanceIds.length);
    for (const allowance of golden.temporary_reciprocal_edges) {
      expect(allowance.id).toMatch(/^TEMP-[A-Z0-9-]+$/u);
      expect(allowance.reason.trim().length).toBeGreaterThan(0);
      expect(allowance.reason).not.toMatch(/[\r\n]/u);
    }
    const allowancePairs = golden.temporary_reciprocal_edges
      .map(({ domains }) => pairKey(domains))
      .sort();
    expect(new Set(allowancePairs).size).toBe(allowancePairs.length);
    expect(graph.reciprocalPairs).toEqual(allowancePairs);
  });
});
