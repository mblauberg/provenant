import { FabricError } from "../errors.js";
import {
  checkNpmInstallAttestation,
  NPM_INSTALL_ATTESTATION_ERROR,
} from "./npm-install-attestation.js";

export async function verifyNpmInstallAttestation(productRoot: string): Promise<void> {
  const mismatch = await checkNpmInstallAttestation(productRoot);
  if (mismatch !== undefined) {
    throw new FabricError(NPM_INSTALL_ATTESTATION_ERROR, mismatch.message, { cause: mismatch.cause });
  }
}
