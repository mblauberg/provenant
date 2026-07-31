export declare const NPM_INSTALL_ATTESTATION_ERROR: "NPM_INSTALL_ATTESTATION_MISMATCH";
export declare const NPM_INSTALL_ATTESTATION_RELATIVE_PATH: string;
export declare const NPM_INSTALL_RECOVERY_COMMAND: "scripts/install-agent-fabric-dependencies";

export type NpmInstallAttestation = {
  productCommit: string;
  lockfileSha256: string;
  packageSriValues: Record<string, string>;
  installedTreeSha256: string;
};

export type NpmInstallAttestationMismatch = {
  reason: "missing" | "invalid" | "product-commit" | "lockfile" | "package-sri" | "installed-tree";
  message: string;
  cause?: unknown;
};

export declare function sha256File(path: string): Promise<string>;
export declare function readPackageSriValues(lockfilePath: string): Promise<Record<string, string>>;
export declare function installedTreeSha256(nodeModulesPath: string): Promise<string>;
export declare function createNpmInstallAttestation(productRoot: string): Promise<NpmInstallAttestation>;
export declare function writeNpmInstallAttestation(productRoot: string): Promise<string>;
export declare function checkNpmInstallAttestation(
  productRoot: string,
): Promise<NpmInstallAttestationMismatch | undefined>;
