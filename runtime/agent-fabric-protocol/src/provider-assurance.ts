import { enumeration } from "./codec.js";

export const PROVIDER_IDENTITY_ASSURANCE_VALUES = [
  "full-vendor-identity",
  "partial-signed-helpers",
  "owner-controlled-install-root",
  "lockfile-install-attestation",
] as const;

export type ProviderIdentityAssurance = typeof PROVIDER_IDENTITY_ASSURANCE_VALUES[number];

export const PROVIDER_IDENTITY_ASSURANCE_V1_CODEC = enumeration(PROVIDER_IDENTITY_ASSURANCE_VALUES);

export function supportsCertifyingAnswerBearingLeg(assurance: ProviderIdentityAssurance): boolean {
  switch (assurance) {
    case "full-vendor-identity":
    case "lockfile-install-attestation":
      return true;
    case "partial-signed-helpers":
    case "owner-controlled-install-root":
      return false;
  }
}
