import type { GitOperationVariant, GitRemoteBinding, Sha256Digest } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { canonicalJson, integer, isRow, nullableText, row, sha256, text } from "../persistence/row-codec.js";

export type TrustedGitExecutionProfile = Readonly<{
  profileId: string;
  revision: number;
  profileDigest: Sha256Digest;
  gitBinaryPath: string;
  gitBinaryVersion: string;
  gitBinaryDigest: Sha256Digest;
  objectFormat: "sha1" | "sha256";
  mergeBackendId: string;
  rebaseBackendId: string;
  environmentDigest: Sha256Digest;
  helperRegistryDigest: Sha256Digest;
  inspectorDigest: Sha256Digest;
}>;

export type TrustedGitRemoteRegistration = Readonly<{
  registrationId: string;
  revision: number;
  generation: number;
  projectId: string;
  remoteName: string;
  transportKind: "local" | "ssh" | "https" | "provider-port";
  targetIdentity: string;
  targetDigest: Sha256Digest;
  adapterId: string;
  adapterContractDigest: Sha256Digest;
  credentialSelectorDigest: Sha256Digest;
}>;

export type TrustedRunGitAllowlist = Readonly<{
  projectSessionId: string;
  coordinationRunId: string;
  authorityRevision: number;
  gitAllowlistEpoch: number;
  gitAllowlistDigest: Sha256Digest;
  allowWorktreeCreation: boolean;
  maximumExpiry: number;
  operationVariants: readonly GitOperationVariant[];
  profiles: readonly Readonly<{ profileId: string; revision: number; digest: Sha256Digest }>[];
  remotes: readonly GitRemoteBinding[];
  refs: readonly string[];
  paths: readonly Readonly<{ repositoryRoot: string; worktreePath: string; canonicalPrefix: string }>[];
}>;

export type TrustedGitConfiguration = Readonly<{
  executionProfiles?: readonly TrustedGitExecutionProfile[];
  remoteRegistrations?: readonly TrustedGitRemoteRegistration[];
  runAllowlists?: readonly TrustedRunGitAllowlist[];
}>;

export function deriveTrustedGitExecutionProfileDigest(
  profile: Omit<TrustedGitExecutionProfile, "profileDigest">,
): Sha256Digest {
  return digest({ domain: "trusted-git-execution-profile-v1", ...profile });
}

export function deriveTrustedGitRemoteTargetDigest(
  remote: Pick<TrustedGitRemoteRegistration,
    "projectId" | "transportKind" | "targetIdentity" | "adapterId" | "adapterContractDigest" | "credentialSelectorDigest"
  >,
): Sha256Digest {
  return digest({
    domain: "trusted-git-remote-target-v1",
    projectId: remote.projectId,
    transportKind: remote.transportKind,
    targetIdentity: remote.targetIdentity,
    adapterId: remote.adapterId,
    adapterContractDigest: remote.adapterContractDigest,
    credentialSelectorDigest: remote.credentialSelectorDigest,
  });
}

export function deriveTrustedRunGitAllowlistDigest(
  allowlist: Omit<TrustedRunGitAllowlist, "gitAllowlistDigest">,
): Sha256Digest {
  return digest({
    domain: "trusted-run-git-allowlist-v1",
    allowlist: canonicalAllowlist(allowlist),
  });
}

/** Trusted daemon-composition boundary. No public protocol operation reaches this owner. */
export class TrustedGitRegistry {
  readonly #database: Database.Database;
  readonly #clock: () => number;

  constructor(database: Database.Database, clock: () => number = Date.now) {
    this.#database = database;
    this.#clock = clock;
  }

  materialize(configuration: TrustedGitConfiguration): { profiles: number; remotes: number; runAllowlists: number } {
    return this.#database.transaction(() => {
      let profiles = 0;
      let remotes = 0;
      let runAllowlists = 0;
      for (const profile of [...configuration.executionProfiles ?? []].sort((left, right) =>
        left.profileId.localeCompare(right.profileId) || left.revision - right.revision)) {
        this.#materializeProfile(profile);
        profiles += 1;
      }
      for (const remote of [...configuration.remoteRegistrations ?? []].sort((left, right) =>
        left.registrationId.localeCompare(right.registrationId) || left.revision - right.revision)) {
        this.#materializeRemote(remote);
        remotes += 1;
      }
      for (const allowlist of [...configuration.runAllowlists ?? []].sort((left, right) =>
        left.projectSessionId.localeCompare(right.projectSessionId) ||
        left.coordinationRunId.localeCompare(right.coordinationRunId) ||
        left.authorityRevision - right.authorityRevision || left.gitAllowlistEpoch - right.gitAllowlistEpoch)) {
        this.#materializeAllowlist(allowlist);
        runAllowlists += 1;
      }
      return { profiles, remotes, runAllowlists };
    })();
  }

  #materializeProfile(profile: TrustedGitExecutionProfile): void {
    if (!profile.gitBinaryPath.startsWith("/") || /[\0\r\n]/u.test(profile.gitBinaryPath)) {
      throw new ProjectFabricCoreError("PROTOCOL_INVALID", "trusted Git executable path is not absolute canonical data");
    }
    if (deriveTrustedGitExecutionProfileDigest(stripProfileDigest(profile)) !== profile.profileDigest) {
      throw new ProjectFabricCoreError("CONFLICT", "trusted Git execution profile digest is invalid");
    }
    assertDigestFields(profile);
    const existing = this.#database.prepare(
      "SELECT * FROM git_execution_profiles WHERE profile_id=? AND revision=?",
    ).get(profile.profileId, profile.revision);
    if (isRow(existing)) {
      if (!sameProfile(existing, profile)) throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "trusted Git profile revision changed");
      return;
    }
    const latest = this.#database.prepare(
      "SELECT MAX(revision) AS revision FROM git_execution_profiles WHERE profile_id=?",
    ).get(profile.profileId);
    if (isRow(latest) && latest.revision !== null && profile.revision !== integer(latest, "revision") + 1) {
      throw new ProjectFabricCoreError("STALE_REVISION", "trusted Git profile revision is not contiguous");
    }
    this.#database.prepare("UPDATE git_execution_profiles SET state='revoked' WHERE profile_id=? AND state='active'")
      .run(profile.profileId);
    this.#database.prepare(`
      INSERT INTO git_execution_profiles(
        profile_id,revision,profile_digest,git_binary_path,git_binary_version,git_binary_digest,
        object_format,merge_backend_id,rebase_backend_id,environment_digest,helper_registry_digest,
        inspector_digest,state,created_at
      ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?, 'active',?)
    `).run(
      profile.profileId, profile.revision, profile.profileDigest, profile.gitBinaryPath,
      profile.gitBinaryVersion, profile.gitBinaryDigest, profile.objectFormat, profile.mergeBackendId,
      profile.rebaseBackendId, profile.environmentDigest, profile.helperRegistryDigest,
      profile.inspectorDigest, this.#clock(),
    );
  }

  #materializeRemote(remote: TrustedGitRemoteRegistration): void {
    if (
      remote.targetIdentity.length === 0 || remote.targetIdentity.length > 4096 ||
      /[\0\r\n?#]/u.test(remote.targetIdentity) || /:\/\/[^/\s]*@/u.test(remote.targetIdentity)
    ) throw new ProjectFabricCoreError("PROTOCOL_INVALID", "trusted Git remote target identity is not bounded secret-free data");
    if (deriveTrustedGitRemoteTargetDigest(remote) !== remote.targetDigest) {
      throw new ProjectFabricCoreError("CONFLICT", "trusted Git remote target digest is invalid");
    }
    assertDigestFields(remote);
    const project = this.#database.prepare("SELECT 1 FROM projects WHERE project_id=?").get(remote.projectId);
    if (!isRow(project)) throw new ProjectFabricCoreError("NOT_FOUND", "trusted Git remote project is absent");
    const existing = this.#database.prepare(
      "SELECT * FROM git_remote_registrations WHERE registration_id=? AND revision=?",
    ).get(remote.registrationId, remote.revision);
    if (isRow(existing)) {
      if (!sameRemote(existing, remote)) throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "trusted Git remote revision changed");
      return;
    }
    const latestValue = this.#database.prepare(`
      SELECT * FROM git_remote_registrations WHERE registration_id=? ORDER BY revision DESC LIMIT 1
    `).get(remote.registrationId);
    if (isRow(latestValue)) {
      if (
        text(latestValue, "project_id") !== remote.projectId ||
        text(latestValue, "remote_name") !== remote.remoteName
      ) throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "trusted Git remote registration identity changed");
      if (remote.revision !== integer(latestValue, "revision") + 1) {
        throw new ProjectFabricCoreError("STALE_REVISION", "trusted Git remote revision is not contiguous");
      }
      const targetChanged = text(latestValue, "target_digest") !== remote.targetDigest;
      const expectedGeneration = integer(latestValue, "generation") + (targetChanged ? 1 : 0);
      if (remote.generation !== expectedGeneration) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "trusted Git remote generation does not fence target rotation");
      }
      this.#database.prepare("UPDATE git_remote_registrations SET state='revoked' WHERE registration_id=? AND state='active'")
        .run(remote.registrationId);
    } else if (remote.revision !== 1 || remote.generation !== 1) {
      throw new ProjectFabricCoreError("STALE_GENERATION", "first trusted Git remote starts at revision and generation one");
    }
    this.#database.prepare(`
      INSERT INTO git_remote_registrations(
        registration_id,revision,generation,project_id,remote_name,transport_kind,target_identity,
        target_digest,adapter_id,adapter_contract_digest,credential_selector_digest,state,created_at
      ) VALUES(?,?,?,?,?,?,?,?,?,?,?, 'active',?)
    `).run(
      remote.registrationId, remote.revision, remote.generation, remote.projectId, remote.remoteName,
      remote.transportKind, remote.targetIdentity, remote.targetDigest, remote.adapterId,
      remote.adapterContractDigest, remote.credentialSelectorDigest, this.#clock(),
    );
  }

  #materializeAllowlist(allowlist: TrustedRunGitAllowlist): void {
    assertDigest(allowlist.gitAllowlistDigest, "trusted Git allow-list digest");
    if (deriveTrustedRunGitAllowlistDigest(stripAllowlistDigest(allowlist)) !== allowlist.gitAllowlistDigest) {
      throw new ProjectFabricCoreError("CONFLICT", "trusted Git allow-list digest is invalid");
    }
    const constraintsJson = canonicalJson({
      operationVariants: sortedUnique(allowlist.operationVariants),
      profiles: sortedUnique(allowlist.profiles.map((value) => canonicalJson(value))),
      remotes: sortedUnique(allowlist.remotes.map((value) => canonicalJson(value))),
      refs: sortedUnique(allowlist.refs),
      paths: sortedUnique(allowlist.paths.map((value) => canonicalJson(value))),
      allowWorktreeCreation: allowlist.allowWorktreeCreation,
    });
    const existing = this.#database.prepare(`
      SELECT * FROM run_git_allowlists WHERE project_session_id=? AND coordination_run_id=?
        AND authority_revision=? AND git_allowlist_epoch=?
    `).get(
      allowlist.projectSessionId, allowlist.coordinationRunId,
      allowlist.authorityRevision, allowlist.gitAllowlistEpoch,
    );
    if (isRow(existing)) {
      if (
        text(existing, "git_allowlist_digest") !== allowlist.gitAllowlistDigest ||
        integer(existing, "allow_worktree_creation") !== (allowlist.allowWorktreeCreation ? 1 : 0) ||
        integer(existing, "maximum_expiry") !== allowlist.maximumExpiry ||
        text(existing, "constraints_json") !== constraintsJson
      ) throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "trusted Git allow-list tuple changed");
      return;
    }
    const run = row(this.#database.prepare(`
      SELECT authority_revision,git_allowlist_epoch,git_allowlist_digest
        FROM runs WHERE project_session_id=? AND run_id=?
    `).get(allowlist.projectSessionId, allowlist.coordinationRunId), "trusted Git allow-list run");
    if (
      integer(run, "authority_revision") !== allowlist.authorityRevision ||
      integer(run, "git_allowlist_epoch") !== allowlist.gitAllowlistEpoch ||
      nullableText(run, "git_allowlist_digest") !== allowlist.gitAllowlistDigest
    ) throw new ProjectFabricCoreError("STALE_GENERATION", "trusted Git allow-list is not the run's approved current authority tuple");
    this.#database.prepare(`
      INSERT INTO run_git_allowlists(
        project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch,
        git_allowlist_digest,allow_worktree_creation,maximum_expiry,constraints_json,created_at
      ) VALUES(?,?,?,?,?,?,?,?,?)
    `).run(
      allowlist.projectSessionId, allowlist.coordinationRunId, allowlist.authorityRevision,
      allowlist.gitAllowlistEpoch, allowlist.gitAllowlistDigest, allowlist.allowWorktreeCreation ? 1 : 0,
      allowlist.maximumExpiry, constraintsJson, this.#clock(),
    );
    const variantInsert = this.#database.prepare("INSERT INTO run_git_allowlist_variants VALUES(?,?,?,?,?)");
    for (const variant of sortedUnique(allowlist.operationVariants)) {
      variantInsert.run(allowlist.projectSessionId, allowlist.coordinationRunId, allowlist.authorityRevision, allowlist.gitAllowlistEpoch, variant);
    }
    const profileInsert = this.#database.prepare("INSERT INTO run_git_allowlist_profiles VALUES(?,?,?,?,?,?,?)");
    for (const profile of allowlist.profiles) {
      profileInsert.run(
        allowlist.projectSessionId, allowlist.coordinationRunId, allowlist.authorityRevision,
        allowlist.gitAllowlistEpoch, profile.profileId, profile.revision, profile.digest,
      );
    }
    const remoteInsert = this.#database.prepare("INSERT INTO run_git_allowlist_remotes VALUES(?,?,?,?,?,?,?,?)");
    for (const remote of allowlist.remotes) {
      remoteInsert.run(
        allowlist.projectSessionId, allowlist.coordinationRunId, allowlist.authorityRevision,
        allowlist.gitAllowlistEpoch, remote.registrationId, remote.revision, remote.generation, remote.targetDigest,
      );
    }
    const refInsert = this.#database.prepare("INSERT INTO run_git_allowlist_refs VALUES(?,?,?,?,?)");
    for (const ref of sortedUnique(allowlist.refs)) {
      refInsert.run(allowlist.projectSessionId, allowlist.coordinationRunId, allowlist.authorityRevision, allowlist.gitAllowlistEpoch, ref);
    }
    const pathInsert = this.#database.prepare("INSERT INTO run_git_allowlist_paths VALUES(?,?,?,?,?,?,?)");
    for (const path of allowlist.paths) {
      pathInsert.run(
        allowlist.projectSessionId, allowlist.coordinationRunId, allowlist.authorityRevision,
        allowlist.gitAllowlistEpoch, path.repositoryRoot, path.worktreePath, path.canonicalPrefix,
      );
    }
  }
}

function canonicalAllowlist(
  allowlist: Omit<TrustedRunGitAllowlist, "gitAllowlistDigest">,
): Record<string, unknown> {
  const sorted = (values: readonly string[]): string[] => [...values].sort();
  return {
    projectSessionId: allowlist.projectSessionId,
    coordinationRunId: allowlist.coordinationRunId,
    authorityRevision: allowlist.authorityRevision,
    gitAllowlistEpoch: allowlist.gitAllowlistEpoch,
    allowWorktreeCreation: allowlist.allowWorktreeCreation,
    maximumExpiry: allowlist.maximumExpiry,
    operationVariants: sorted(allowlist.operationVariants),
    profiles: sorted(allowlist.profiles.map(canonicalJson)),
    remotes: sorted(allowlist.remotes.map(canonicalJson)),
    refs: sorted(allowlist.refs),
    paths: sorted(allowlist.paths.map(canonicalJson)),
  };
}

function stripAllowlistDigest(
  allowlist: TrustedRunGitAllowlist,
): Omit<TrustedRunGitAllowlist, "gitAllowlistDigest"> {
  const { gitAllowlistDigest: _digest, ...value } = allowlist;
  return value;
}

function stripProfileDigest(profile: TrustedGitExecutionProfile): Omit<TrustedGitExecutionProfile, "profileDigest"> {
  const { profileDigest: _digest, ...value } = profile;
  return value;
}

function sameProfile(rowValue: Record<string, unknown>, profile: TrustedGitExecutionProfile): boolean {
  return text(rowValue, "profile_digest") === profile.profileDigest &&
    text(rowValue, "git_binary_path") === profile.gitBinaryPath &&
    text(rowValue, "git_binary_version") === profile.gitBinaryVersion &&
    text(rowValue, "git_binary_digest") === profile.gitBinaryDigest &&
    text(rowValue, "object_format") === profile.objectFormat &&
    text(rowValue, "merge_backend_id") === profile.mergeBackendId &&
    text(rowValue, "rebase_backend_id") === profile.rebaseBackendId &&
    text(rowValue, "environment_digest") === profile.environmentDigest &&
    text(rowValue, "helper_registry_digest") === profile.helperRegistryDigest &&
    text(rowValue, "inspector_digest") === profile.inspectorDigest;
}

function sameRemote(rowValue: Record<string, unknown>, remote: TrustedGitRemoteRegistration): boolean {
  return integer(rowValue, "generation") === remote.generation && text(rowValue, "project_id") === remote.projectId &&
    text(rowValue, "remote_name") === remote.remoteName && text(rowValue, "transport_kind") === remote.transportKind &&
    text(rowValue, "target_identity") === remote.targetIdentity && text(rowValue, "target_digest") === remote.targetDigest &&
    text(rowValue, "adapter_id") === remote.adapterId &&
    text(rowValue, "adapter_contract_digest") === remote.adapterContractDigest &&
    text(rowValue, "credential_selector_digest") === remote.credentialSelectorDigest;
}

function sortedUnique<T extends string>(values: readonly T[]): T[] {
  const result = [...new Set(values)].sort();
  if (result.length !== values.length) throw new ProjectFabricCoreError("CONFLICT", "trusted Git configuration contains duplicates");
  return result;
}

function assertDigestFields(value: Record<string, unknown>): void {
  for (const [key, candidate] of Object.entries(value)) {
    if (/digest$/iu.test(key) && candidate !== null) assertDigest(candidate, key);
  }
}

function assertDigest(value: unknown, label: string): asserts value is Sha256Digest {
  if (typeof value !== "string" || !/^sha256:[0-9a-f]{64}$/u.test(value)) {
    throw new ProjectFabricCoreError("PROTOCOL_INVALID", `${label} is not a canonical SHA-256 digest`);
  }
}

function digest(value: unknown): Sha256Digest {
  return `sha256:${sha256(canonicalJson(value))}` as Sha256Digest;
}
