import { execFile } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

import {
  checkNpmInstallAttestation,
  NPM_INSTALL_ATTESTATION_RELATIVE_PATH,
  writeNpmInstallAttestation,
} from "../../src/adapters/npm-install-attestation.ts";

const execFileAsync = promisify(execFile);
const scriptsDirectory = fileURLToPath(new URL("../../scripts", import.meta.url));
const fixtures: string[] = [];

afterEach(async () => {
  await Promise.all(fixtures.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

async function createFixture(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "npm-install-attestation-"));
  fixtures.push(root);
  await Promise.all([
    mkdir(join(root, "runtime", "agent-fabric"), { recursive: true }),
    mkdir(join(root, "node_modules", "tsx", "dist"), { recursive: true }),
    mkdir(join(root, "node_modules", ".bin"), { recursive: true }),
  ]);
  await Promise.all([
    writeFile(join(root, "node_modules", "tsx", "dist", "loader.mjs"), "export {};\n"),
    writeFile(join(root, "node_modules", ".bin", "tsx"), "ignored shim\n"),
    writeFile(
      join(root, "package-lock.json"),
      `${JSON.stringify({
        name: "fixture",
        lockfileVersion: 3,
        packages: {
          "": { name: "fixture" },
          "node_modules/tsx": { version: "1.0.0", integrity: "sha512-fixture" },
        },
      }, null, 2)}\n`,
    ),
  ]);
  await execFileAsync("git", ["init", "-q"], { cwd: root });
  await execFileAsync("git", ["add", "."], { cwd: root });
  await execFileAsync(
    "git",
    ["-c", "user.name=Attestation Test", "-c", "user.email=test@example.invalid", "commit", "-qm", "fixture"],
    { cwd: root },
  );
  return root;
}

async function readAttestation(root: string): Promise<Record<string, unknown>> {
  return JSON.parse(await readFile(join(root, NPM_INSTALL_ATTESTATION_RELATIVE_PATH), "utf8")) as Record<string, unknown>;
}

describe("npm install attestation", () => {
  it("keeps the bare-node implementation in the packed package surface", async () => {
    const packageJson = JSON.parse(
      await readFile(join(scriptsDirectory, "..", "package.json"), "utf8"),
    ) as { files?: string[] };

    expect(packageJson.files).toContain("scripts/lib/npm-install-attestation.mjs");
  });

  it("executes both attestation wrappers with bare node", async () => {
    const root = await createFixture();
    const environment = { ...process.env, NODE_OPTIONS: "" };
    const writeResult = await execFileAsync(
      process.execPath,
      ["--no-strip-types", join(scriptsDirectory, "write-npm-ci-attestation.mjs"), root],
      { cwd: root, env: environment },
    );

    expect(writeResult.stderr).toBe("");
    expect(writeResult.stdout).toContain("wrote npm install attestation:");

    const verifyResult = await execFileAsync(
      process.execPath,
      ["--no-strip-types", join(scriptsDirectory, "verify-npm-ci-attestation.mjs"), root],
      { cwd: root, env: environment },
    );
    expect(verifyResult.stderr).toBe("");
    expect(verifyResult.stdout).toBe("");
  });

  it("writes the required formatted fields and verifies the unchanged install", async () => {
    const root = await createFixture();
    const path = await writeNpmInstallAttestation(root);
    const serialized = await readFile(path, "utf8");

    expect(serialized).toContain('\n  "productCommit":');
    expect(serialized.endsWith("\n")).toBe(true);
    expect(await readAttestation(root)).toMatchObject({
      productCommit: expect.stringMatching(/^[a-f0-9]{40}$/u),
      lockfileSha256: expect.stringMatching(/^[a-f0-9]{64}$/u),
      packageSriValues: { "node_modules/tsx": "sha512-fixture" },
      installedTreeSha256: expect.stringMatching(/^[a-f0-9]{64}$/u),
    });
    await expect(checkNpmInstallAttestation(root)).resolves.toBeUndefined();
  });

  it("reports a product commit change", async () => {
    const root = await createFixture();
    await writeNpmInstallAttestation(root);
    await execFileAsync(
      "git",
      ["-c", "user.name=Attestation Test", "-c", "user.email=test@example.invalid", "commit", "--allow-empty", "-qm", "next"],
      { cwd: root },
    );

    await expect(checkNpmInstallAttestation(root)).resolves.toMatchObject({ reason: "product-commit" });
  });

  it("reports lockfile byte drift", async () => {
    const root = await createFixture();
    await writeNpmInstallAttestation(root);
    const lockfile = join(root, "package-lock.json");
    await writeFile(lockfile, `${await readFile(lockfile, "utf8")}\n`);

    await expect(checkNpmInstallAttestation(root)).resolves.toMatchObject({ reason: "lockfile" });
  });

  it("reports SRI drift recorded in the attestation", async () => {
    const root = await createFixture();
    const path = await writeNpmInstallAttestation(root);
    const attestation = await readAttestation(root);
    attestation.packageSriValues = { "node_modules/tsx": "sha512-tampered" };
    await writeFile(path, `${JSON.stringify(attestation, null, 2)}\n`);

    await expect(checkNpmInstallAttestation(root)).resolves.toMatchObject({ reason: "package-sri" });
  });

  it("reports installed execution-tree byte drift but excludes npm .bin shims", async () => {
    const root = await createFixture();
    await writeNpmInstallAttestation(root);
    await writeFile(join(root, "node_modules", ".bin", "tsx"), "changed ignored shim\n");
    await expect(checkNpmInstallAttestation(root)).resolves.toBeUndefined();

    await writeFile(join(root, "node_modules", "tsx", "dist", "loader.mjs"), "export {}; // tampered\n");
    await expect(checkNpmInstallAttestation(root)).resolves.toMatchObject({ reason: "installed-tree" });
  });
});
