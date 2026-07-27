import { lstatSync, realpathSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { basename, dirname, join, relative, resolve } from "node:path";

import { FabricError } from "../../errors.js";
import { digestCanonical, type Sha256Digest } from "../canonical/index.js";

export const DEPLOYED_REVIEW_PROFILE_RELATIVE_PATH =
  "config/review-profiles/certifying-review-four-slot-v1.json";
export const DEPLOYED_REVIEW_PROFILE_DIGEST_RECORD =
  "certifying-review-four-slot-v1.deployment-digest.json";

export type DeployedReviewProfileCatalogueReport =
  | Readonly<{
      status: "unverified";
      profile: string;
      record: string;
      repairCommand: string;
    }>
  | Readonly<{
      status: "verified";
      profile: string;
      record: string;
      digest: Sha256Digest;
      repairCommand: string;
    }>;

export type DeployedReviewProfileCatalogueVerification =
  DeployedReviewProfileCatalogueReport & Readonly<{ catalogue: unknown }>;

function errorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string"
    ? error.code
    : undefined;
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\"'\"'")}'`;
}

function relativeToAgentsHome(agentsHome: string, path: string): string {
  const selected = relative(agentsHome, path).replaceAll("\\", "/");
  if (selected.length === 0 || selected === ".." || selected.startsWith("../")) {
    throw new FabricError(
      "ARTIFACT_PATH_FORBIDDEN",
      `deployed review profile must be inside agentsHome: ${path}`,
      { field: path },
    );
  }
  return selected;
}

function deploymentRecordPath(profilePath: string): string {
  const name = basename(profilePath, ".json");
  return join(dirname(profilePath), `${name}.deployment-digest.json`);
}

function canonicalNonSymlinkPath(path: string, field: string): string {
  let cursor = resolve(path);
  const suffix: string[] = [];
  while (true) {
    try {
      if (lstatSync(cursor).isSymbolicLink()) {
        throw new FabricError(
          "ARTIFACT_PATH_FORBIDDEN",
          `deployed review profile path resolves through a symlink: ${field}`,
          { field },
        );
      }
      const canonical = realpathSync.native(cursor);
      if (canonical !== cursor) {
        throw new FabricError(
          "ARTIFACT_PATH_FORBIDDEN",
          `deployed review profile path resolves through a symlink: ${field}`,
          { field },
        );
      }
      return resolve(canonical, ...suffix);
    } catch (error: unknown) {
      if (error instanceof FabricError) throw error;
      if (errorCode(error) !== "ENOENT") {
        throw new FabricError(
          "ARTIFACT_PATH_FORBIDDEN",
          `deployed review profile path has no safe canonical ancestor: ${field}`,
          { field, cause: error },
        );
      }
      const parent = dirname(cursor);
      if (parent === cursor) {
        throw new FabricError(
          "ARTIFACT_PATH_FORBIDDEN",
          `deployed review profile path has no safe canonical ancestor: ${field}`,
          { field, cause: error },
        );
      }
      suffix.unshift(basename(cursor));
      cursor = parent;
    }
  }
}

export type DeployedReviewProfileLocation = Readonly<{
  agentsHome: string;
  profilePath: string;
  recordPath: string;
  profile: string;
  record: string;
}>;

export function resolveDeployedReviewProfileLocation(options: {
  readonly agentsHome: string;
  readonly profilePath?: string;
}): DeployedReviewProfileLocation {
  const requestedAgentsHome = resolve(options.agentsHome);
  let agentsHome: string;
  try {
    agentsHome = realpathSync.native(requestedAgentsHome);
  } catch (error: unknown) {
    throw new FabricError(
      "ARTIFACT_PATH_FORBIDDEN",
      `agentsHome has no canonical directory: ${requestedAgentsHome}`,
      { field: requestedAgentsHome, cause: error },
    );
  }
  const requestedProfilePath = resolve(
    options.profilePath ?? join(requestedAgentsHome, DEPLOYED_REVIEW_PROFILE_RELATIVE_PATH),
  );
  const profile = relativeToAgentsHome(requestedAgentsHome, requestedProfilePath);
  if (profile !== DEPLOYED_REVIEW_PROFILE_RELATIVE_PATH) {
    throw new FabricError(
      "ARTIFACT_PATH_FORBIDDEN",
      `review profile must be the deployed catalogue at ${DEPLOYED_REVIEW_PROFILE_RELATIVE_PATH}`,
      { field: requestedProfilePath },
    );
  }
  const profilePath = canonicalNonSymlinkPath(
    join(agentsHome, profile),
    requestedProfilePath,
  );
  const requestedRecordPath = deploymentRecordPath(requestedProfilePath);
  const recordPath = canonicalNonSymlinkPath(
    join(agentsHome, dirname(profile), basename(requestedRecordPath)),
    requestedRecordPath,
  );
  return {
    agentsHome,
    profilePath,
    recordPath,
    profile,
    record: relativeToAgentsHome(requestedAgentsHome, requestedRecordPath),
  };
}

export function deployedReviewProfileRepairCommand(
  agentsHome: string,
  profilePath?: string,
): string {
  const location = resolveDeployedReviewProfileLocation({
    agentsHome,
    ...(profilePath === undefined ? {} : { profilePath }),
  });
  return `npm run profile:catalogue:deploy -- --agents-home ${shellQuote(location.agentsHome)}`;
}

type DeploymentDigestRecord = Readonly<{
  schemaVersion: 1;
  profile: string;
  digest: Sha256Digest;
}>;

function isDeploymentDigestRecord(
  value: unknown,
  profile: string,
): value is DeploymentDigestRecord {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  return "schemaVersion" in value
    && value.schemaVersion === 1
    && "profile" in value
    && value.profile === profile
    && "digest" in value
    && typeof value.digest === "string"
    && /^sha256:[a-f0-9]{64}$/u.test(value.digest);
}

export async function verifyDeployedReviewProfileCatalogue(options: {
  readonly agentsHome: string;
  readonly profilePath?: string;
}): Promise<DeployedReviewProfileCatalogueVerification> {
  const location = resolveDeployedReviewProfileLocation(options);
  const { profilePath, recordPath, profile, record } = location;
  const repairCommand = deployedReviewProfileRepairCommand(location.agentsHome, profilePath);

  let recordSource: string | undefined;
  try {
    recordSource = await readFile(recordPath, "utf8");
  } catch (error: unknown) {
    if (errorCode(error) !== "ENOENT") {
      throw new FabricError(
        "ARTIFACT_DIGEST_INVALID",
        `deployed review profile digest record could not be read: ${record}. Redeploy ${profile} with: ${repairCommand}`,
        { field: record, cause: error },
      );
    }
  }

  let profileSource: string;
  try {
    profileSource = await readFile(profilePath, "utf8");
  } catch (error: unknown) {
    throw new FabricError(
      errorCode(error) === "ENOENT" ? "NOT_FOUND" : "ARTIFACT_DIGEST_INVALID",
      `deployed review profile could not be read: ${profile}. Redeploy it with: ${repairCommand}`,
      { field: profile, cause: error },
    );
  }

  let catalogue: unknown;
  let observedDigest: Sha256Digest;
  try {
    catalogue = JSON.parse(profileSource);
    observedDigest = digestCanonical(catalogue);
  } catch (error: unknown) {
    throw new FabricError(
      "ARTIFACT_DIGEST_INVALID",
      `deployed review profile is not valid canonical JSON: ${profile}. Redeploy it with: ${repairCommand}`,
      { field: profile, cause: error },
    );
  }

  if (recordSource === undefined) {
    return { status: "unverified", profile, record, repairCommand, catalogue };
  }

  let deploymentRecord: unknown;
  try {
    deploymentRecord = JSON.parse(recordSource);
    digestCanonical(deploymentRecord);
  } catch (error: unknown) {
    throw new FabricError(
      "ARTIFACT_DIGEST_INVALID",
      `deployed review profile digest record is not valid canonical JSON: ${record}. Redeploy ${profile} with: ${repairCommand}`,
      { field: record, cause: error },
    );
  }
  if (!isDeploymentDigestRecord(deploymentRecord, profile)) {
    throw new FabricError(
      "ARTIFACT_DIGEST_INVALID",
      `deployed review profile digest record is invalid for ${profile}: ${record}. Redeploy ${profile} with: ${repairCommand}`,
      { field: record },
    );
  }

  const expectedDigest = deploymentRecord.digest;
  if (observedDigest !== expectedDigest) {
    throw new FabricError(
      "ARTIFACT_DIGEST_INVALID",
      `deployed review profile digest mismatch: expected ${expectedDigest}, observed ${observedDigest}. `
        + `Redeploy ${profile} with: ${repairCommand}`,
      { field: profile },
    );
  }
  return { status: "verified", profile, record, digest: observedDigest, repairCommand, catalogue };
}
