import { readFile } from "node:fs/promises";
import { join, resolve } from "node:path";

import { FabricError } from "../../errors.js";
import { digestCanonical, type Sha256Digest } from "../canonical/index.js";
import {
  REVIEW_PROFILE_CATALOGUE_DIGEST,
  REVIEW_PROFILE_DOCUMENT_DIGEST,
} from "./catalogue-digest.generated.js";

export const REVIEW_PROFILE_CATALOGUE_MEMBERS = [
  "runtime/agent-fabric/schemas/review-profile.v1.schema.json",
  "config/review-profiles/certifying-review-four-slot-v1.json",
] as const;
export const REVIEW_PROFILE_CATALOGUE_PROFILE_MEMBER =
  "config/review-profiles/certifying-review-four-slot-v1.json";

export type ReviewProfileCatalogueDigestVerification = Readonly<{
  digest: Sha256Digest;
  profileDocumentDigest: Sha256Digest;
  members: typeof REVIEW_PROFILE_CATALOGUE_MEMBERS;
}>;

const REPAIR_COMMAND = "npm run profile:catalogue:pin";

function errorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string"
    ? error.code
    : undefined;
}

async function readCatalogueMember(repositoryRoot: string, member: string): Promise<unknown> {
  let source: string;
  try {
    source = await readFile(join(repositoryRoot, member), "utf8");
  } catch (error: unknown) {
    if (errorCode(error) === "ENOENT") {
      throw new FabricError(
        "NOT_FOUND",
        `review profile catalogue member is absent: ${member}. Restore it, then run: ${REPAIR_COMMAND}`,
        { field: member, cause: error },
      );
    }
    throw new FabricError(
      "ARTIFACT_DIGEST_INVALID",
      `review profile catalogue member could not be read: ${member}. Repair it, then run: ${REPAIR_COMMAND}`,
      { field: member, cause: error },
    );
  }
  try {
    const document: unknown = JSON.parse(source);
    // Parsing alone admits values outside the repository's canonical JSON
    // domain (for example, lone surrogates or non-finite numbers).
    digestCanonical(document);
    return document;
  } catch (error: unknown) {
    throw new FabricError(
      "ARTIFACT_DIGEST_INVALID",
      `review profile catalogue member is not valid canonical JSON: ${member}. Repair it, then run: ${REPAIR_COMMAND}`,
      { field: member, cause: error },
    );
  }
}

export async function computeReviewProfileCatalogueSnapshot(
  repositoryRoot = resolve(import.meta.dirname, "../../../../.."),
): Promise<ReviewProfileCatalogueDigestVerification> {
  const members = [];
  for (const path of REVIEW_PROFILE_CATALOGUE_MEMBERS) {
    members.push({ path, document: await readCatalogueMember(repositoryRoot, path) });
  }
  const profile = members.find(({ path }) => path === REVIEW_PROFILE_CATALOGUE_PROFILE_MEMBER);
  if (profile === undefined) throw new TypeError("review profile catalogue member list omits the profile document");
  return {
    digest: digestCanonical({ schemaVersion: 1, members }),
    profileDocumentDigest: digestCanonical(profile.document),
    members: REVIEW_PROFILE_CATALOGUE_MEMBERS,
  };
}

export async function computeReviewProfileCatalogueDigest(
  repositoryRoot = resolve(import.meta.dirname, "../../../../.."),
): Promise<Sha256Digest> {
  return (await computeReviewProfileCatalogueSnapshot(repositoryRoot)).digest;
}

export async function computeReviewProfileCatalogueMemberDigest(
  member: typeof REVIEW_PROFILE_CATALOGUE_MEMBERS[number],
  repositoryRoot = resolve(import.meta.dirname, "../../../../.."),
): Promise<Sha256Digest> {
  return digestCanonical(await readCatalogueMember(repositoryRoot, member));
}

export async function verifyReviewProfileCatalogueDigest(
  repositoryRoot = resolve(import.meta.dirname, "../../../../.."),
  expectedDigest: Sha256Digest = REVIEW_PROFILE_CATALOGUE_DIGEST,
  expectedProfileDocumentDigest: Sha256Digest = REVIEW_PROFILE_DOCUMENT_DIGEST,
): Promise<ReviewProfileCatalogueDigestVerification> {
  const snapshot = await computeReviewProfileCatalogueSnapshot(repositoryRoot);
  if (snapshot.digest !== expectedDigest) {
    throw new FabricError(
      "ARTIFACT_DIGEST_INVALID",
      `review profile catalogue digest mismatch: expected ${expectedDigest}, observed ${snapshot.digest}. Run: ${REPAIR_COMMAND}`,
      { field: "config/review-profiles/certifying-review-four-slot-v1.json" },
    );
  }
  if (snapshot.profileDocumentDigest !== expectedProfileDocumentDigest) {
    throw new FabricError(
      "ARTIFACT_DIGEST_INVALID",
      `review profile catalogue member digest mismatch: expected ${expectedProfileDocumentDigest}, `
        + `observed ${snapshot.profileDocumentDigest}. Run: ${REPAIR_COMMAND}`,
      { field: REVIEW_PROFILE_CATALOGUE_PROFILE_MEMBER },
    );
  }
  return snapshot;
}
