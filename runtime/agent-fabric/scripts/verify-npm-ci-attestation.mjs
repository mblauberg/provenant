import { resolve } from "node:path";

import {
  checkNpmInstallAttestation,
  NPM_INSTALL_ATTESTATION_ERROR,
} from "./lib/npm-install-attestation.mjs";

const productRoot = resolve(process.argv[2] ?? new URL("../../..", import.meta.url).pathname);
const mismatch = await checkNpmInstallAttestation(productRoot);
if (mismatch !== undefined) {
  process.stderr.write(`${NPM_INSTALL_ATTESTATION_ERROR}: ${mismatch.message}\n`);
  process.exitCode = 1;
}
