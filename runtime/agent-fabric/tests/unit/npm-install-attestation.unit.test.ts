import { execFile } from "node:child_process";
import { access, chmod, copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

import {
  checkNpmInstallAttestation,
  NPM_INSTALL_ATTESTATION_RELATIVE_PATH,
  sha256File,
  writeNpmInstallAttestation,
} from "../../src/adapters/npm-install-attestation.ts";

const execFileAsync = promisify(execFile);
const scriptsDirectory = fileURLToPath(new URL("../../scripts", import.meta.url));
const repositoryScriptsDirectory = fileURLToPath(new URL("../../../../scripts", import.meta.url));
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

async function createRecoveryFixture(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "npm-install-recovery-"));
  const packageName = basename(root);
  fixtures.push(root);
  await Promise.all([
    mkdir(join(root, "runtime", "agent-fabric", "scripts", "lib"), { recursive: true }),
    mkdir(join(root, "scripts"), { recursive: true }),
    mkdir(join(root, "node_modules"), { recursive: true }),
  ]);
  await Promise.all([
    writeFile(join(root, "package.json"), `${JSON.stringify({
      name: packageName,
      version: "1.0.0",
      private: true,
    })}\n`),
    writeFile(join(root, "node_modules", "stale.txt"), "stale install\n"),
    copyFile(
      join(scriptsDirectory, "write-npm-ci-attestation.mjs"),
      join(root, "runtime", "agent-fabric", "scripts", "write-npm-ci-attestation.mjs"),
    ),
    copyFile(
      join(scriptsDirectory, "lib", "npm-install-attestation.mjs"),
      join(root, "runtime", "agent-fabric", "scripts", "lib", "npm-install-attestation.mjs"),
    ),
    copyFile(
      join(repositoryScriptsDirectory, "install-agent-fabric-dependencies"),
      join(root, "scripts", "install-agent-fabric-dependencies"),
    ),
  ]);
  await chmod(join(root, "scripts", "install-agent-fabric-dependencies"), 0o755);
  const npmEnvironment = {
    ...process.env,
    NPM_CONFIG_AUDIT: "false",
    NPM_CONFIG_CACHE: join(root, ".npm-cache"),
    NPM_CONFIG_FUND: "false",
    NPM_CONFIG_LOGS_DIR: join(root, ".npm-logs"),
    NPM_CONFIG_OFFLINE: "true",
    NPM_CONFIG_PREFIX: root,
    NPM_CONFIG_UPDATE_NOTIFIER: "false",
  };
  await execFileAsync(
    "npm",
    ["install", "--package-lock-only", "--ignore-scripts", "--no-audit", "--no-fund", "--offline"],
    { cwd: root, env: npmEnvironment },
  );
  await execFileAsync(
    process.execPath,
    [join(root, "runtime", "agent-fabric", "scripts", "write-npm-ci-attestation.mjs"), root],
    { cwd: root },
  );
  const path = join(root, NPM_INSTALL_ATTESTATION_RELATIVE_PATH);
  const attestation = await readAttestation(root);
  attestation.installedTreeSha256 = "stale-receipt";
  await writeFile(path, `${JSON.stringify(attestation, null, 2)}\n`);
  return root;
}

type MalformedCase = {
  name: string;
  reason: "invalid" | "package-sri";
  mutateAttestation?: (attestation: Record<string, unknown>) => void;
  lockfile?: string | Record<string, unknown>;
};

async function writeMalformedCase(
  root: string,
  path: string,
  testCase: MalformedCase,
): Promise<void> {
  if (testCase.name === "invalid JSON attestation") {
    await writeFile(path, "{\n");
    return;
  }
  const attestation = await readAttestation(root);
  testCase.mutateAttestation?.(attestation);
  if (testCase.lockfile !== undefined) {
    const lockfile = join(root, "package-lock.json");
    const content = typeof testCase.lockfile === "string"
      ? testCase.lockfile
      : `${JSON.stringify(testCase.lockfile)}\n`;
    await writeFile(lockfile, content);
    attestation.lockfileSha256 = await sha256File(lockfile);
  }
  await writeFile(path, `${JSON.stringify(attestation, null, 2)}\n`);
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

  it("writes only dependency attestation fields and verifies the unchanged install", async () => {
    const root = await createFixture();
    await rm(join(root, ".git"), { recursive: true, force: true });
    const path = await writeNpmInstallAttestation(root);
    const serialized = await readFile(path, "utf8");

    expect(serialized).not.toContain("productCommit");
    expect(serialized.endsWith("\n")).toBe(true);
    expect(await readAttestation(root)).toMatchObject({
      lockfileSha256: expect.stringMatching(/^[a-f0-9]{64}$/u),
      packageSriValues: { "node_modules/tsx": "sha512-fixture" },
      installedTreeSha256: expect.stringMatching(/^[a-f0-9]{64}$/u),
    });
    await expect(checkNpmInstallAttestation(root)).resolves.toBeUndefined();
  });

  it("recovers a genuinely non-Git product through the advertised helper", async () => {
    const root = await createRecoveryFixture();
    const fakeNpmBin = await mkdtemp(join(tmpdir(), "npm-install-recovery-bin-"));
    fixtures.push(fakeNpmBin);
    const realNpm = (await execFileAsync("which", ["npm"])).stdout.trim();
    const npmArguments = join(root, "npm-arguments");
    const fakeNpm = join(fakeNpmBin, "npm");
    await writeFile(fakeNpm, `#!/bin/sh
set -eu
printf '%s\\n' "$@" > "$NPM_RECOVERY_ARGUMENTS"
[ "\${1:-}" = ci ]
shift
[ "\${1:-}" = --prefix ]
shift
[ "\${1:-}" = "$NPM_RECOVERY_ROOT" ]
shift
exec "$NPM_REAL" ci "$@"
`);
    await chmod(fakeNpm, 0o755);

    expect(await access(join(root, ".git")).then(() => true).catch(() => false)).toBe(false);
    const result = await execFileAsync(
      join(root, "scripts", "install-agent-fabric-dependencies"),
      [],
      {
        cwd: root,
        env: {
          ...process.env,
          NPM_CONFIG_AUDIT: "false",
          NPM_CONFIG_CACHE: join(root, ".npm-cache"),
          NPM_CONFIG_FUND: "false",
          NPM_CONFIG_LOGS_DIR: join(root, ".npm-logs"),
          NPM_CONFIG_OFFLINE: "true",
          NPM_CONFIG_PREFIX: root,
          NPM_RECOVERY_ARGUMENTS: npmArguments,
          NPM_RECOVERY_ROOT: root,
          NPM_REAL: realNpm,
          NPM_CONFIG_UPDATE_NOTIFIER: "false",
          PATH: `${fakeNpmBin}:${process.env.PATH ?? ""}`,
        },
      },
    );

    expect(result.stderr).toBe("");
    expect(await readFile(npmArguments, "utf8")).toBe(`ci\n--prefix\n${root}\n--no-audit\n--no-fund\n`);
    await expect(checkNpmInstallAttestation(root)).resolves.toBeUndefined();
    await expect(readAttestation(root)).resolves.toMatchObject({
      installedTreeSha256: expect.not.stringMatching(/^stale-receipt$/u),
    });
  });

  it("accepts a changed Git HEAD when dependency inputs are unchanged", async () => {
    const root = await createFixture();
    await writeNpmInstallAttestation(root);
    await execFileAsync(
      "git",
      ["-c", "user.name=Attestation Test", "-c", "user.email=test@example.invalid", "commit", "--allow-empty", "-qm", "next"],
      { cwd: root },
    );

    await expect(checkNpmInstallAttestation(root)).resolves.toBeUndefined();
  });

  it("validates an extracted product without Git metadata", async () => {
    const root = await createFixture();
    await writeNpmInstallAttestation(root);
    await rm(join(root, ".git"), { recursive: true, force: true });

    await expect(checkNpmInstallAttestation(root)).resolves.toBeUndefined();
  });

  it("accepts a legacy product commit field without using it", async () => {
    const root = await createFixture();
    const path = await writeNpmInstallAttestation(root);
    const attestation = await readAttestation(root);
    attestation.productCommit = "legacy-provenance-that-must-not-gate";
    await writeFile(path, `${JSON.stringify(attestation, null, 2)}\n`);

    await expect(checkNpmInstallAttestation(root)).resolves.toBeUndefined();
  });

  it.each<MalformedCase>([
    { name: "invalid JSON attestation", reason: "invalid" },
    {
      name: "missing attestation field",
      reason: "invalid",
      mutateAttestation: (attestation) => delete attestation.lockfileSha256,
    },
    {
      name: "scalar attestation field",
      reason: "invalid",
      mutateAttestation: (attestation) => { attestation.packageSriValues = "not-a-map"; },
    },
    {
      name: "scalar attestation digest",
      reason: "invalid",
      mutateAttestation: (attestation) => { attestation.installedTreeSha256 = 42; },
    },
    { name: "invalid JSON lockfile", reason: "package-sri", lockfile: "{\n" },
    { name: "missing lockfile packages map", reason: "package-sri", lockfile: { lockfileVersion: 3 } },
    {
      name: "scalar lockfile packages map",
      reason: "package-sri",
      lockfile: { lockfileVersion: 3, packages: "not-a-map" },
    },
    {
      name: "scalar package integrity",
      reason: "package-sri",
      lockfile: { lockfileVersion: 3, packages: { "node_modules/tsx": { integrity: 42 } } },
    },
    {
      name: "empty package integrity",
      reason: "package-sri",
      lockfile: { lockfileVersion: 3, packages: { "node_modules/tsx": { integrity: "" } } },
    },
  ])("reports $name as $reason", async (testCase) => {
    const root = await createFixture();
    const path = join(root, NPM_INSTALL_ATTESTATION_RELATIVE_PATH);
    await writeNpmInstallAttestation(root);
    await writeMalformedCase(root, path, testCase);

    await expect(checkNpmInstallAttestation(root)).resolves.toMatchObject({ reason: testCase.reason });
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

  it("reports installed execution-tree byte drift, including npm .bin shims", async () => {
    const root = await createFixture();
    await writeNpmInstallAttestation(root);
    await writeFile(join(root, "node_modules", ".bin", "tsx"), "changed shim\n");
    await expect(checkNpmInstallAttestation(root)).resolves.toMatchObject({ reason: "installed-tree" });

    await writeNpmInstallAttestation(root);
    await writeFile(join(root, "node_modules", "tsx", "dist", "loader.mjs"), "export {}; // tampered\n");
    await expect(checkNpmInstallAttestation(root)).resolves.toMatchObject({ reason: "installed-tree" });
  });
});
