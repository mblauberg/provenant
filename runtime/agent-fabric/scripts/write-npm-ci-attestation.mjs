import { resolve } from "node:path";

import { writeNpmInstallAttestation } from "./lib/npm-install-attestation.mjs";

const productRoot = resolve(process.argv[2] ?? new URL("../../..", import.meta.url).pathname);
const path = await writeNpmInstallAttestation(productRoot);
process.stdout.write(`wrote npm install attestation: ${path}\n`);
