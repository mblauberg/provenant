import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { computeRuntimeBuildIdentity } from "../../src/daemon/runtime-build-identity.ts";

const cleanup: string[] = [];

afterEach(async () => {
  await Promise.allSettled(cleanup.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function fixtureRoot(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "fabric-runtime-build-identity-"));
  cleanup.push(root);
  await Promise.all([
    mkdir(join(root, "runtime", "agent-fabric", "src", "daemon"), { recursive: true }),
    mkdir(join(root, "runtime", "agent-fabric", "migrations"), { recursive: true }),
    mkdir(join(root, "runtime", "agent-fabric", "schemas"), { recursive: true }),
    mkdir(join(root, "runtime", "agent-fabric", "scripts", "lib"), { recursive: true }),
    mkdir(join(root, "runtime", "agent-fabric-protocol", "src"), { recursive: true }),
    mkdir(join(root, "runtime", "agent-fabric-protocol", "schemas"), { recursive: true }),
    mkdir(join(root, "runtime", "agent-fabric-protocol", "bin"), { recursive: true }),
    mkdir(join(root, "runtime", "agent-fabric-herdr", "src"), { recursive: true }),
    mkdir(join(root, "runtime", "agent-fabric-herdr", "bin"), { recursive: true }),
    mkdir(join(root, "runtime", "agent-fabric", "dist", "daemon"), { recursive: true }),
    mkdir(join(root, "runtime", "agent-fabric-protocol", "dist"), { recursive: true }),
    mkdir(join(root, "runtime", "agent-fabric-herdr", "dist"), { recursive: true }),
  ]);
  await Promise.all([
    writeFile(join(root, "package.json"), "root"),
    writeFile(join(root, "package-lock.json"), "lock"),
    writeFile(join(root, "runtime", "agent-fabric", "package.json"), "fabric"),
    writeFile(join(root, "runtime", "agent-fabric-protocol", "package.json"), "protocol"),
    writeFile(join(root, "runtime", "agent-fabric-herdr", "package.json"), "herdr"),
    writeFile(join(root, "runtime", "agent-fabric", "src", "daemon", "runtime.ts"), "source"),
    writeFile(join(root, "runtime", "agent-fabric-protocol", "src", "protocol.ts"), "protocol source"),
    writeFile(join(root, "runtime", "agent-fabric-herdr", "src", "index.ts"), "herdr source"),
    writeFile(join(root, "runtime", "agent-fabric-protocol", "bin", "preflight.js"), "protocol preflight"),
    writeFile(join(root, "runtime", "agent-fabric-herdr", "bin", "herdr.js"), "herdr wrapper"),
    writeFile(join(root, "runtime", "agent-fabric", "dist", "daemon", "runtime.js"), "compiled source"),
    writeFile(join(root, "runtime", "agent-fabric-protocol", "dist", "protocol.js"), "compiled protocol"),
    writeFile(join(root, "runtime", "agent-fabric-herdr", "dist", "index.js"), "compiled herdr"),
    writeFile(join(root, "runtime", "agent-fabric", "migrations", "0001.sql"), "create table"),
    writeFile(join(root, "runtime", "agent-fabric", "schemas", "database.json"), "schema"),
    writeFile(join(root, "runtime", "agent-fabric-protocol", "schemas", "protocol.json"), "generated schema"),
    writeFile(join(root, "runtime", "agent-fabric", "scripts", "lib", "helper.mjs"), "helper"),
    writeFile(join(root, "runtime", "agent-fabric", "scripts", "verify-npm-ci-attestation.mjs"), "attestation helper"),
  ]);
  return root;
}

describe("runtime build identity", () => {
  it("hashes every owned runtime/resource surface but excludes docs and tests", async () => {
    const root = await fixtureRoot();
    const initial = await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" });

    await writeFile(join(root, "runtime", "agent-fabric", "migrations", "0001.sql"), "alter table");
    expect(await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" })).not.toBe(initial);

    const afterMigration = await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" });
    await writeFile(join(root, "runtime", "agent-fabric-protocol", "schemas", "protocol.json"), "changed schema");
    expect(await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" })).not.toBe(afterMigration);

    const afterSchema = await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" });
    await writeFile(join(root, "runtime", "agent-fabric", "scripts", "lib", "helper.mjs"), "changed helper");
    expect(await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" })).not.toBe(afterSchema);

    const afterHelper = await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" });
    await writeFile(join(root, "runtime", "agent-fabric", "scripts", "verify-npm-ci-attestation.mjs"), "changed attestation helper");
    expect(await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" })).not.toBe(afterHelper);

    const afterAttestationHelper = await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" });
    await writeFile(join(root, "runtime", "agent-fabric-herdr", "src", "index.ts"), "changed herdr");
    expect(await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" })).not.toBe(afterAttestationHelper);

    const afterHerdr = await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" });
    await mkdir(join(root, "docs"));
    await mkdir(join(root, "tests"));
    await writeFile(join(root, "docs", "note.md"), "unrelated");
    await writeFile(join(root, "tests", "fixture.ts"), "unrelated");
    expect(await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" })).toBe(afterHerdr);
  });

  it("uses the actual source or dist runtime surface without a tracked generated module", async () => {
    const root = await fixtureRoot();
    const sourceIdentity = await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "source" });
    const distIdentity = await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "dist" });
    expect(distIdentity).not.toBe(sourceIdentity);

    await writeFile(join(root, "runtime", "agent-fabric", "dist", "daemon", "runtime.js"), "compiled source changed");
    expect(await computeRuntimeBuildIdentity({ repositoryRoot: root, mode: "dist" })).not.toBe(distIdentity);
  });
});
