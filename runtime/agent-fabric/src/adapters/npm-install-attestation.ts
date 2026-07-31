export {
  checkNpmInstallAttestation,
  createNpmInstallAttestation,
  installedTreeSha256,
  NPM_INSTALL_ATTESTATION_ERROR,
  NPM_INSTALL_ATTESTATION_RELATIVE_PATH,
  NPM_INSTALL_RECOVERY_COMMAND,
  readPackageSriValues,
  sha256File,
  writeNpmInstallAttestation,
} from "../../scripts/lib/npm-install-attestation.mjs";
export type {
  NpmInstallAttestation,
  NpmInstallAttestationMismatch,
} from "../../scripts/lib/npm-install-attestation.mjs";
