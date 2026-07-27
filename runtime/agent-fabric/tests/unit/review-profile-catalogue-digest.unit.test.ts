import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

import {
  REVIEW_PROFILE_CATALOGUE_DIGEST,
  REVIEW_PROFILE_DOCUMENT_DIGEST,
} from "../../src/review/profile/catalogue-digest.generated.ts";
import {
  computeReviewProfileCatalogueDigest,
  computeReviewProfileCatalogueMemberDigest,
  REVIEW_PROFILE_CATALOGUE_MEMBERS,
  REVIEW_PROFILE_CATALOGUE_PROFILE_MEMBER,
  verifyReviewProfileCatalogueDigest,
} from "../../src/review/profile/catalogue-digest.ts";
import { pinReviewProfileCatalogueDigest } from "../../scripts/pin-review-profile-catalogue-digest.ts";

const repositoryRoot = fileURLToPath(new URL("../../../..", import.meta.url));
const cleanup: string[] = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map(async (path) => await rm(path, { recursive: true, force: true })));
});

async function catalogueCopy(options: { omit?: string; mutate?: string; reformat?: boolean } = {}): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "review-profile-catalogue-"));
  cleanup.push(root);
  for (const member of REVIEW_PROFILE_CATALOGUE_MEMBERS) {
    if (member === options.omit) continue;
    const target = join(root, member);
    await mkdir(dirname(target), { recursive: true });
    const document: unknown = JSON.parse(await readFile(join(repositoryRoot, member), "utf8"));
    if (member === options.mutate) {
      (document as { profileId: string }).profileId = "silently-rewritten-profile";
    }
    const rendered = options.reformat
      ? `${JSON.stringify(document, null, 4).replaceAll("\n", "\r\n")}\r\n`
      : `${JSON.stringify(document)}\n`;
    await writeFile(target, rendered, "utf8");
  }
  return root;
}

describe("review profile catalogue digest", () => {
  it("pins the schema then profile in one fixed explicit order", () => {
    expect(REVIEW_PROFILE_CATALOGUE_MEMBERS).toStrictEqual([
      "runtime/agent-fabric/schemas/review-profile.v1.schema.json",
      "config/review-profiles/certifying-review-four-slot-v1.json",
    ]);
  });

  it("matches the checked-in catalogue after canonical JSON normalisation", async () => {
    const root = await catalogueCopy({ reformat: true });
    await expect(computeReviewProfileCatalogueDigest(root)).resolves.toBe(REVIEW_PROFILE_CATALOGUE_DIGEST);
    await expect(computeReviewProfileCatalogueMemberDigest(
      REVIEW_PROFILE_CATALOGUE_PROFILE_MEMBER,
      root,
    )).resolves.toBe(REVIEW_PROFILE_DOCUMENT_DIGEST);
    await expect(verifyReviewProfileCatalogueDigest(root)).resolves.toEqual({
      digest: REVIEW_PROFILE_CATALOGUE_DIGEST,
      profileDocumentDigest: REVIEW_PROFILE_DOCUMENT_DIGEST,
      members: REVIEW_PROFILE_CATALOGUE_MEMBERS,
    });
  });

  it("rejects a semantically changed member with a typed digest error and repair command", async () => {
    const root = await catalogueCopy({
      mutate: "config/review-profiles/certifying-review-four-slot-v1.json",
    });
    await expect(verifyReviewProfileCatalogueDigest(root)).rejects.toMatchObject({
      name: "FabricError",
      code: "ARTIFACT_DIGEST_INVALID",
      field: "config/review-profiles/certifying-review-four-slot-v1.json",
      message: expect.stringContaining("npm run profile:catalogue:pin"),
    });
  });

  it("rejects a crossed generated profile-member pin at startup", async () => {
    const root = await catalogueCopy();
    await expect(verifyReviewProfileCatalogueDigest(
      root,
      REVIEW_PROFILE_CATALOGUE_DIGEST,
      "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    )).rejects.toMatchObject({
      name: "FabricError",
      code: "ARTIFACT_DIGEST_INVALID",
      field: REVIEW_PROFILE_CATALOGUE_PROFILE_MEMBER,
      message: expect.stringContaining("npm run profile:catalogue:pin"),
    });
  });

  it("rejects an absent member with a typed not-found error and repair command", async () => {
    const absent = "runtime/agent-fabric/schemas/review-profile.v1.schema.json";
    const root = await catalogueCopy({ omit: absent });
    await expect(verifyReviewProfileCatalogueDigest(root)).rejects.toMatchObject({
      name: "FabricError",
      code: "NOT_FOUND",
      field: absent,
      message: expect.stringContaining("npm run profile:catalogue:pin"),
    });
  });

  it("rejects parseable but non-canonical JSON with a typed digest error and repair command", async () => {
    const invalid = "runtime/agent-fabric/schemas/review-profile.v1.schema.json";
    const root = await catalogueCopy();
    await writeFile(join(root, invalid), "{\"value\":\"\\ud800\"}\n", "utf8");
    await expect(verifyReviewProfileCatalogueDigest(root)).rejects.toMatchObject({
      name: "FabricError",
      code: "ARTIFACT_DIGEST_INVALID",
      field: invalid,
      message: expect.stringContaining("npm run profile:catalogue:pin"),
    });
  });

  it("regenerates the checked-in constant and then accepts the changed catalogue", async () => {
    const root = await catalogueCopy({
      mutate: "config/review-profiles/certifying-review-four-slot-v1.json",
    });
    const outputPath = join(root, "runtime/agent-fabric/src/review/profile/catalogue-digest.generated.ts");
    await mkdir(dirname(outputPath), { recursive: true });
    await writeFile(outputPath, "stale digest\n", "utf8");

    const pinned = await pinReviewProfileCatalogueDigest({ repositoryRoot: root, outputPath });
    expect(pinned.changed).toBe(true);
    expect(await readFile(outputPath, "utf8")).toContain(pinned.digest);
    expect(await readFile(outputPath, "utf8")).toContain(pinned.profileDocumentDigest);
    await expect(verifyReviewProfileCatalogueDigest(
      root,
      pinned.digest,
      pinned.profileDocumentDigest,
    )).resolves.toMatchObject({
      digest: pinned.digest,
      profileDocumentDigest: pinned.profileDocumentDigest,
    });
    await expect(pinReviewProfileCatalogueDigest({ repositoryRoot: root, outputPath })).resolves.toMatchObject({
      changed: false,
      digest: pinned.digest,
    });
  });
});
