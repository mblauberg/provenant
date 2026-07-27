import { randomBytes } from "node:crypto";
import { chmod, mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";

import { digestCanonical, type Sha256Digest } from "../src/review/canonical/index.js";
import {
  DEPLOYED_REVIEW_PROFILE_RELATIVE_PATH,
  resolveDeployedReviewProfileLocation,
} from "../src/review/profile/deployed-catalogue.js";

const repositoryRoot = resolve(import.meta.dirname, "../../..");

async function atomicWrite(path: string, source: string): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  const temporaryPath = join(
    dirname(path),
    `.${process.pid}-${randomBytes(12).toString("hex")}.tmp`,
  );
  const handle = await open(temporaryPath, "wx", 0o644);
  try {
    await handle.writeFile(source, "utf8");
    await handle.sync();
  } finally {
    await handle.close();
  }
  try {
    await rename(temporaryPath, path);
    await chmod(path, 0o644);
  } finally {
    await rm(temporaryPath, { force: true });
  }
}

export async function deployReviewProfileCatalogue(options: {
  readonly agentsHome: string;
  readonly profilePath?: string;
  readonly repositoryRoot?: string;
}): Promise<{
  profilePath: string;
  recordPath: string;
  digest: Sha256Digest;
}> {
  const sourceRoot = resolve(options.repositoryRoot ?? repositoryRoot);
  const sourcePath = join(sourceRoot, DEPLOYED_REVIEW_PROFILE_RELATIVE_PATH);
  const location = resolveDeployedReviewProfileLocation({
    agentsHome: options.agentsHome,
    ...(options.profilePath === undefined ? {} : { profilePath: options.profilePath }),
  });
  const { profilePath, recordPath } = location;
  const source = await readFile(sourcePath, "utf8");
  const sourceDocument: unknown = JSON.parse(source);
  const sourceDigest = digestCanonical(sourceDocument);

  // The checkout can itself be agentsHome. In that topology only the
  // deployment-owned record is written; the source document is already the
  // deployed document and must not be copied over itself.
  if (sourcePath !== profilePath) await atomicWrite(profilePath, source);

  const deployedSource = await readFile(profilePath, "utf8");
  const deployedDocument: unknown = JSON.parse(deployedSource);
  const digest = digestCanonical(deployedDocument);
  if (digest !== sourceDigest) {
    throw new Error(`deployed review profile did not match its source: ${location.profile}`);
  }
  await atomicWrite(recordPath, `${JSON.stringify({
    schemaVersion: 1,
    profile: location.profile,
    digest,
  }, null, 2)}\n`);
  return { profilePath, recordPath, digest };
}

function option(arguments_: string[], name: string): string | undefined {
  const index = arguments_.indexOf(name);
  const value = index === -1 ? undefined : arguments_[index + 1];
  if (index !== -1 && (value === undefined || value.startsWith("--"))) {
    throw new Error(`${name} requires a value`);
  }
  return value;
}

if (process.argv[1] !== undefined && resolve(process.argv[1]) === import.meta.filename) {
  const arguments_ = process.argv.slice(2);
  const agentsHome = option(arguments_, "--agents-home");
  if (agentsHome === undefined || arguments_.length !== 2) {
    throw new Error("usage: npm run profile:catalogue:deploy -- --agents-home ABSOLUTE_PATH");
  }
  const result = await deployReviewProfileCatalogue({ agentsHome });
  process.stdout.write(`deployed ${result.profilePath} ${result.digest} record=${result.recordPath}\n`);
}
