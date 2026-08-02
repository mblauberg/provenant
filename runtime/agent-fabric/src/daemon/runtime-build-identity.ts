import { createHash } from "node:crypto";
import { lstat, readdir, readFile } from "node:fs/promises";
import { relative, resolve } from "node:path";

export type RuntimeBuildMode = "source" | "dist";

type TreeSpec = Readonly<{
  path: string;
  extensions: readonly string[];
}>;

const RUNTIME_HELPER_PATHS = [
  "runtime/agent-fabric/scripts/verify-npm-ci-attestation.mjs",
] as const;

const MANIFEST_PATHS = [
  "package.json",
  "package-lock.json",
  "runtime/agent-fabric/package.json",
  "runtime/agent-fabric-protocol/package.json",
  "runtime/agent-fabric-herdr/package.json",
] as const;

function sourceOrDist(mode: RuntimeBuildMode, packageName: string): string {
  // The source launcher executes agent-fabric and Herdr through tsx, while the
  // protocol package also loads its compiled exports. Its source tree is added
  // separately to the source-mode identity below.
  if (mode === "source" && packageName === "agent-fabric-protocol") {
    return `runtime/${packageName}/dist`;
  }
  return `runtime/${packageName}/${mode === "source" ? "src" : "dist"}`;
}

function treeSpecs(mode: RuntimeBuildMode): readonly TreeSpec[] {
  const runtimeExtensions = mode === "source" ? [".ts"] : [".js", ".mjs", ".json"];
  return [
    { path: sourceOrDist(mode, "agent-fabric"), extensions: runtimeExtensions },
    {
      path: sourceOrDist(mode, "agent-fabric-protocol"),
      extensions: [".js", ".mjs", ".json"],
    },
    ...(mode === "source"
      ? [{ path: "runtime/agent-fabric-protocol/src", extensions: [".ts"] }]
      : []),
    { path: sourceOrDist(mode, "agent-fabric-herdr"), extensions: runtimeExtensions },
    { path: "runtime/agent-fabric/migrations", extensions: [".sql"] },
    { path: "runtime/agent-fabric/schemas", extensions: [".json", ".sql"] },
    { path: "runtime/agent-fabric-protocol/schemas", extensions: [".json"] },
    { path: "runtime/agent-fabric/scripts/lib", extensions: [".mjs"] },
    { path: "runtime/agent-fabric-protocol/bin", extensions: [".js"] },
    { path: "runtime/agent-fabric-herdr/bin", extensions: [".js"] },
  ];
}

async function filesInTree(root: string, spec: TreeSpec): Promise<string[]> {
  const directory = resolve(root, spec.path);
  const entries = await readdir(directory, { withFileTypes: true });
  const files: string[] = [];
  for (const entry of entries.sort((left, right) => left.name < right.name ? -1 : left.name > right.name ? 1 : 0)) {
    const path = resolve(directory, entry.name);
    if (entry.isSymbolicLink()) throw new Error(`runtime identity input must not be a symbolic link: ${path}`);
    if (entry.isDirectory()) {
      files.push(...await filesInTree(root, { ...spec, path: relative(root, path) }));
      continue;
    }
    if (!entry.isFile() || !spec.extensions.some((extension) => entry.name.endsWith(extension))) continue;
    files.push(path);
  }
  return files;
}

async function inputFiles(repositoryRoot: string, mode: RuntimeBuildMode): Promise<string[]> {
  const manifestFiles = MANIFEST_PATHS.map((path) => resolve(repositoryRoot, path));
  const runtimeHelperFiles = RUNTIME_HELPER_PATHS.map((path) => resolve(repositoryRoot, path));
  const treeFiles = (await Promise.all(treeSpecs(mode).map((spec) => filesInTree(repositoryRoot, spec)))).flat();
  return [...new Set([...manifestFiles, ...runtimeHelperFiles, ...treeFiles])].sort((left, right) => {
    const leftRelative = relative(repositoryRoot, left);
    const rightRelative = relative(repositoryRoot, right);
    return leftRelative < rightRelative ? -1 : leftRelative > rightRelative ? 1 : 0;
  });
}

/**
 * Hashes only the code and resources that the daemon loads from this
 * workspace. The mode is part of the domain: source and compiled runtimes are
 * different executable surfaces and must not silently attest one another.
 */
export async function computeRuntimeBuildIdentity(input: {
  repositoryRoot: string;
  mode: RuntimeBuildMode;
}): Promise<string> {
  const repositoryRoot = resolve(input.repositoryRoot);
  const digest = createHash("sha256");
  digest.update(`agent-fabric-runtime-build.v2\0${input.mode}\0`);
  for (const path of await inputFiles(repositoryRoot, input.mode)) {
    const relativePath = relative(repositoryRoot, path);
    const info = await lstat(path);
    if (!info.isFile() || info.isSymbolicLink() || info.nlink !== 1) {
      throw new Error(`runtime identity input must be a single-link regular file: ${path}`);
    }
    const bytes = await readFile(path);
    digest.update(relativePath);
    digest.update("\0");
    digest.update(String(bytes.byteLength));
    digest.update("\0");
    digest.update(bytes);
    digest.update("\0");
  }
  return `sha256:${digest.digest("hex")}`;
}

export async function currentRuntimeBuildIdentity(): Promise<string> {
  const sourceMode = import.meta.url.endsWith(".ts");
  const packageRoot = resolve(import.meta.dirname, "../..");
  return await computeRuntimeBuildIdentity({
    repositoryRoot: resolve(packageRoot, "../.."),
    mode: sourceMode ? "source" : "dist",
  });
}
