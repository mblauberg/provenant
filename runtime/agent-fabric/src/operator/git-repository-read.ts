import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { constants, existsSync } from "node:fs";
import {
  chmod,
  lstat,
  mkdir,
  open,
  readlink,
  realpath,
} from "node:fs/promises";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";

import {
  parseArtifactRef,
  parseIdentifier,
  parseSha256Digest,
  parseTimestamp,
  type GitBranchRecord,
  type GitHead,
  type GitHostedChecks,
  type GitRepositoryBinding,
  type GitLogEntry,
  type GitLogPage,
  type GitOperationState,
  type GitRepositoryProjection,
  type GitRepositoryReadRequest,
  type GitRepositoryReadResult,
  type GitUpstream,
  type GitWorktreeRecord,
  type ProjectionFact,
  type Sha256Digest,
  type Timestamp,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ArtifactRegistry } from "../artifacts/registry.js";
import { ProjectFabricCoreError, type CoreServiceOptions } from "../project-session/contracts.js";
import { canonicalJson, integer, isRow, row, text } from "../persistence/row-codec.js";
import type { AuthenticatedOperatorCredential, OperatorStore } from "./store.js";

const MAX_GIT_OUTPUT_BYTES = 8 * 1024 * 1024;
const MAX_DIFF_ARTIFACT_BYTES = 1024 * 1024;
const MAX_WORKTREE_CONTENT_BYTES = 64 * 1024 * 1024;
const MAX_PATHS = 256;
const MAX_BRANCHES = 128;
const MAX_WORKTREES = 64;
const MAX_LOG_SCAN = 4096;
const GIT_TIMEOUT_MS = 15_000;
const GIT_EXECUTABLE = "/usr/bin/git";
const DIFF_TRUNCATION_MARKER = Buffer.from("\n[agent-fabric: diff truncated at 1048576 bytes]\n", "utf8");
const NATIVE_OBJECT_PATTERN = /^[0-9a-f]{40,64}$/u;
const HOSTED_SECRET_PATTERN = /\b(?:afb|afc|afop)_[A-Za-z0-9_-]{8,}|\bghp_[A-Za-z0-9_]{8,}|\bgithub_pat_[A-Za-z0-9_]{8,}/u;

type GitCommandResult = { stdout: Buffer; truncated: boolean };

type RepositoryIdentity = {
  topLevel: string;
  commonDirectory: string;
  gitDirectory: string;
  objectFormat: "sha1" | "sha256";
};

type ParsedStatus = {
  raw: Buffer;
  headObjectId: string;
  headRef: string | null;
  upstream: { shorthand: string; ahead: number; behind: number } | null;
  staged: string[];
  unstaged: string[];
  untracked: string[];
  conflicted: string[];
};

type CoreObservation = {
  identity: RepositoryIdentity;
  status: ParsedStatus;
  head: GitHead;
  headDigest: Sha256Digest;
  indexDigest: Sha256Digest;
  worktreeDigest: Sha256Digest;
  remoteDigest: Sha256Digest;
  repositoryStateDigest: Sha256Digest;
  operationState: GitOperationState;
  upstream: GitUpstream | null;
};

type FullObservation = {
  core: CoreObservation;
  projectionFenceDigest: Sha256Digest;
  diffBytes: Buffer;
  diffBaseDigest: Sha256Digest;
  diffTargetDigest: Sha256Digest;
  log: GitLogPage;
  branches: { items: GitBranchRecord[]; truncated: boolean };
  worktrees: { items: GitWorktreeRecord[]; truncated: boolean };
};

export type GitHostedChecksBinding = {
  canonicalRepositoryRoot: string;
  canonicalWorktreePath: string;
  repositoryStateDigest: Sha256Digest;
  headObjectDigest: Sha256Digest;
  nativeHeadObjectId: string;
  snapshotRevision: number;
  observedAt: Timestamp;
};

/** Optional hosted facts are read independently after the local Git snapshot is complete. */
export interface GitHostedChecksPort {
  read(
    binding: GitHostedChecksBinding,
  ): Promise<ProjectionFact<GitHostedChecks | null, "github">>;
}

/**
 * The mutation owner uses the same bounded observation algorithm as repository
 * reads so a reviewed projection is a real compare-and-set fence, not a second
 * approximation of Git state.
 */
export async function observeGitRepositoryForMutation(
  repositoryRoot: string,
  worktreePath: string,
): Promise<GitRepositoryBinding> {
  const canonicalRepositoryRoot = await realpath(repositoryRoot);
  const canonicalWorktreePath = await realpath(worktreePath);
  if (canonicalRepositoryRoot !== repositoryRoot || canonicalWorktreePath !== worktreePath) {
    throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "Git mutation target is not canonical");
  }
  const identity = await repositoryIdentityAt(worktreePath);
  const rootIdentity = worktreePath === repositoryRoot ? identity : await repositoryIdentityAt(repositoryRoot);
  if (identity.commonDirectory !== rootIdentity.commonDirectory) {
    throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "Git mutation worktree belongs to another repository");
  }
  const status = parseStatus((await runGit(worktreePath, [
    "status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all", "--ignore-submodules=none",
  ])).stdout);
  const index = (await runGit(worktreePath, ["ls-files", "--stage", "-z"])).stdout;
  const remoteRefs = (await runGit(worktreePath, [
    "for-each-ref", "--format=%(refname)%00%(objectname)", "refs/remotes",
  ])).stdout;
  const localRefs = (await runGit(worktreePath, [
    "for-each-ref", "--format=%(refname)%00%(objectname)", "refs/heads",
  ])).stdout;
  const remoteConfiguration = (await runGit(worktreePath, [
    "config", "--null", "--get-regexp", "^remote\\..*\\.(url|pushurl|fetch)$",
  ], { allowedExitCodes: [1] })).stdout;
  const localConfiguration = (await runGit(worktreePath, ["config", "--local", "--null", "--list"])).stdout;
  const changedPaths = uniqueSorted([
    ...status.staged,
    ...status.unstaged,
    ...status.untracked,
    ...status.conflicted,
  ]);
  const contentDigest = await hashChangedWorktreePaths(worktreePath, changedPaths);
  const head = await gitHead(worktreePath, status);
  const headDigest = sha256Digest(canonicalJson({ head }));
  const indexDigest = sha256Buffers([Buffer.from("git-index-v1\0"), index]);
  const worktreeDigest = sha256Buffers([Buffer.from("git-worktree-v1\0"), status.raw, Buffer.from(contentDigest)]);
  const remoteStateDigest = sha256Buffers([Buffer.from("git-remotes-v1\0"), remoteRefs, remoteConfiguration]);
  const commonInfo = await lstat(identity.commonDirectory);
  const worktrees = await readWorktrees(repositoryRoot, worktreePath);
  const configDigest = sha256Buffers([Buffer.from("git-local-config-v1\0"), localConfiguration]);
  const localRefsDigest = sha256Buffers([Buffer.from("git-local-refs-v1\0"), localRefs]);
  const repositoryStateDigest = sha256Digest(canonicalJson({
    headDigest,
    indexDigest,
    worktreeDigest,
    remoteStateDigest,
    configDigest,
    localRefsDigest,
    worktreeRegistryDigest: worktrees.fenceDigest,
  }));
  return {
    repositoryRoot,
    worktreePath,
    gitCommonDir: identity.commonDirectory,
    commonDirectoryIdentityDigest: sha256Digest(canonicalJson({
      path: identity.commonDirectory,
      device: commonInfo.dev,
      inode: commonInfo.ino,
    })),
    repositoryStateDigest,
    headDigest,
    indexDigest,
    worktreeDigest,
    remoteStateDigest,
    configDigest,
    worktreeRegistryDigest: worktrees.fenceDigest,
  };
}

/** Resolves an exact full ref and hashes the raw Git object, never an abbreviation. */
export async function readGitObjectDigest(
  worktreePath: string,
  refName: string,
): Promise<{ nativeObjectId: string; digest: Sha256Digest }> {
  if (!/^refs\/[A-Za-z0-9._/-]+$/u.test(refName) || refName.includes("..") || refName.endsWith("/")) {
    throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "Git ref is not a fully qualified safe ref");
  }
  const nativeObjectId = (await runGit(worktreePath, ["rev-parse", "--verify", `${refName}^{object}`])).stdout
    .toString("utf8").trim();
  if (!NATIVE_OBJECT_PATTERN.test(nativeObjectId)) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git ref did not resolve to one exact object");
  }
  const digest = requiredObjectDigest(await readObjectDigests(worktreePath, [nativeObjectId]), nativeObjectId);
  return { nativeObjectId, digest };
}

export type GitRepositoryReadServiceOptions = CoreServiceOptions & {
  operatorStore: OperatorStore;
  privateStateRoot: string;
  hostedChecks?: GitHostedChecksPort;
  artifactRegistry?: ArtifactRegistry;
};

export class GitRepositoryReadService {
  readonly #database: Database.Database;
  readonly #operatorStore: OperatorStore;
  readonly #privateStateRoot: string;
  readonly #hostedChecks: GitHostedChecksPort | undefined;
  readonly #clock: () => number;
  readonly #artifactRegistry: ArtifactRegistry;

  constructor(options: GitRepositoryReadServiceOptions) {
    this.#database = options.database;
    this.#operatorStore = options.operatorStore;
    this.#privateStateRoot = resolve(options.privateStateRoot);
    this.#hostedChecks = options.hostedChecks;
    this.#clock = options.clock ?? Date.now;
    this.#artifactRegistry = options.artifactRegistry ?? new ArtifactRegistry(this.#database, this.#clock);
  }

  async read(request: GitRepositoryReadRequest): Promise<GitRepositoryReadResult> {
    const authenticated = this.#authorise(request);
    const initialSnapshotRevision = this.#globalRevision();
    if (request.snapshotRevision !== initialSnapshotRevision) {
      return resnapshot(initialSnapshotRevision);
    }
    const target = await this.#resolveTarget(request, authenticated);
    const privateStateRoot = await realpath(this.#privateStateRoot);
    if (contains(target.repositoryRoot, privateStateRoot)) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "private Git artifacts must be stored outside the repository");
    }
    let observation = await this.#observe(target.repositoryRoot, target.worktreePath, request);
    let finalFence = await this.#observeProjectionFence(target.repositoryRoot, target.worktreePath);
    if (observation.projectionFenceDigest !== finalFence.digest) {
      observation = await this.#observe(target.repositoryRoot, target.worktreePath, request);
      finalFence = await this.#observeProjectionFence(target.repositoryRoot, target.worktreePath);
      if (observation.projectionFenceDigest !== finalFence.digest) {
        throw new ProjectFabricCoreError(
          "PROJECTION_RESNAPSHOT_REQUIRED",
          `repository changed during observation; latest state is ${finalFence.core.repositoryStateDigest}`,
          { repositoryStateDigest: finalFence.core.repositoryStateDigest },
        );
      }
    }
    const postReadSnapshotRevision = this.#globalRevision();
    if (postReadSnapshotRevision !== request.snapshotRevision) return resnapshot(postReadSnapshotRevision);

    const observedAt = parseTimestamp(new Date(this.#clock()).toISOString(), "gitRepository.observedAt");
    const hostedChecks = await this.#readHostedChecks({
      canonicalRepositoryRoot: target.repositoryRoot,
      canonicalWorktreePath: target.worktreePath,
      repositoryStateDigest: observation.core.repositoryStateDigest,
      headObjectDigest: observation.core.head.objectDigest,
      nativeHeadObjectId: observation.core.status.headObjectId,
      snapshotRevision: request.snapshotRevision,
      observedAt,
    });
    const postHostedSnapshotRevision = this.#globalRevision();
    if (postHostedSnapshotRevision !== request.snapshotRevision) return resnapshot(postHostedSnapshotRevision);
    const postHostedFence = await this.#observeProjectionFence(target.repositoryRoot, target.worktreePath);
    if (postHostedFence.digest !== observation.projectionFenceDigest) {
      throw new ProjectFabricCoreError(
        "PROJECTION_RESNAPSHOT_REQUIRED",
        `repository changed during hosted-check observation; latest state is ${postHostedFence.core.repositoryStateDigest}`,
        { repositoryStateDigest: postHostedFence.core.repositoryStateDigest },
      );
    }
    const artifactRef = await this.#writePrivateDiff(observation.diffBytes, {
      projectId: request.projectId,
      projectSessionId: request.projectSessionId ?? null,
    });
    const finalSnapshotRevision = this.#globalRevision();
    if (finalSnapshotRevision !== request.snapshotRevision) return resnapshot(finalSnapshotRevision);
    const returnFence = await this.#observeProjectionFence(target.repositoryRoot, target.worktreePath);
    if (returnFence.digest !== observation.projectionFenceDigest) {
      throw new ProjectFabricCoreError(
        "PROJECTION_RESNAPSHOT_REQUIRED",
        `repository changed before projection return; latest state is ${returnFence.core.repositoryStateDigest}`,
        { repositoryStateDigest: returnFence.core.repositoryStateDigest },
      );
    }
    const returnSnapshotRevision = this.#globalRevision();
    if (returnSnapshotRevision !== request.snapshotRevision) return resnapshot(returnSnapshotRevision);
    const repository: GitRepositoryProjection = {
      freshness: "live",
      source: "git",
      revision: request.snapshotRevision,
      observedAt,
      canonicalRepositoryRoot: target.repositoryRoot,
      canonicalWorktreePath: target.worktreePath,
      repositoryStateDigest: observation.core.repositoryStateDigest,
      head: observation.core.head,
      headDigest: observation.core.headDigest,
      indexDigest: observation.core.indexDigest,
      worktreeDigest: observation.core.worktreeDigest,
      remoteDigest: observation.core.remoteDigest,
      changes: {
        staged: page(observation.core.status.staged),
        unstaged: page(observation.core.status.unstaged),
        untracked: page(observation.core.status.untracked),
        conflicted: page(observation.core.status.conflicted),
      },
      operationState: observation.core.operationState,
      upstream: observation.core.upstream,
      diff: {
        selector: request.diff,
        artifactRef,
        baseDigest: observation.diffBaseDigest,
        targetDigest: observation.diffTargetDigest,
      },
      log: observation.log,
      branches: observation.branches,
      worktrees: observation.worktrees,
      hostedChecks,
    };
    return {
      status: "current",
      projectId: request.projectId,
      projectSessionId: request.projectSessionId ?? null,
      snapshotRevision: request.snapshotRevision,
      readTransactionId: parseIdentifier<"ReadTransactionId">(
        `read_${hexDigest(canonicalJson({
          projectId: request.projectId,
          projectSessionId: request.projectSessionId ?? null,
          snapshotRevision: request.snapshotRevision,
          repositoryStateDigest: repository.repositoryStateDigest,
          target: request.target,
        })).slice(0, 32)}`,
        "gitRepository.readTransactionId",
      ),
      repository,
    };
  }

  async #readHostedChecks(
    binding: GitHostedChecksBinding,
  ): Promise<ProjectionFact<GitHostedChecks | null, "github">> {
    if (this.#hostedChecks === undefined) return unavailableHostedChecks(binding, "hosted checks integration is disabled; local Git observation is independent");
    try {
      const fact = await this.#hostedChecks.read(binding);
      assertHostedChecksFact(fact, binding.headObjectDigest);
      return fact;
    } catch {
      return unavailableHostedChecks(binding, "hosted checks integration failed safely; local Git observation is independent");
    }
  }

  #authorise(request: GitRepositoryReadRequest): AuthenticatedOperatorCredential {
    const authenticated = this.#operatorStore.authenticateCredential(request.credential.token);
    if (authenticated.capabilityId !== request.credential.capabilityId) {
      throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "operator credential identity does not match");
    }
    if (authenticated.context.projectId !== request.projectId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "operator credential is bound to another project");
    }
    if (!authenticated.actions.includes("read")) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator capability lacks read");
    }
    if (authenticated.projectSessionId !== request.projectSessionId) {
      throw new ProjectFabricCoreError(
        "CAPABILITY_FORBIDDEN",
        "operator repository read must preserve the exact capability session binding",
      );
    }
    if (request.target.kind === "session-worktree" && request.projectSessionId === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "session worktree reads require an exact session");
    }
    return authenticated;
  }

  async #resolveTarget(
    request: GitRepositoryReadRequest,
    authenticated: AuthenticatedOperatorCredential,
  ): Promise<{ repositoryRoot: string; worktreePath: string }> {
    const project = row(this.#database.prepare(`
      SELECT canonical_root FROM projects WHERE project_id=?
    `).get(request.projectId), "project");
    const repositoryRoot = text(project, "canonical_root");
    await assertExactRealpath(repositoryRoot, "trusted project root");
    const repositoryIdentity = await repositoryIdentityAt(repositoryRoot);
    if (
      repositoryIdentity.topLevel !== repositoryRoot ||
      !contains(repositoryRoot, repositoryIdentity.commonDirectory) ||
      !contains(repositoryRoot, repositoryIdentity.gitDirectory)
    ) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "trusted project root is not the Git repository root");
    }
    if (request.target.kind === "project-root") {
      return { repositoryRoot, worktreePath: repositoryRoot };
    }
    if (authenticated.projectSessionId === undefined || request.projectSessionId === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "worktree target has no authenticated session");
    }
    const admitted = this.#database.prepare(`
      SELECT w.repository_root, w.worktree_path
        FROM writer_admissions w
        JOIN resource_reservations r ON r.reservation_id=w.reservation_id
       WHERE r.project_session_id=? AND w.repository_root=? AND w.worktree_path=?
         AND w.state='active'
    `).get(request.projectSessionId, repositoryRoot, request.target.canonicalWorktreePath);
    if (!isRow(admitted)) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "worktree is not actively admitted to the selected session");
    }
    const worktreePath = text(admitted, "worktree_path");
    const expectedParent = join(repositoryRoot, ".worktrees");
    if (dirname(worktreePath) !== expectedParent) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "session worktree is outside the canonical repository worktree directory");
    }
    await assertExactRealpath(worktreePath, "session worktree");
    const identity = await repositoryIdentityAt(worktreePath);
    if (identity.topLevel !== worktreePath || identity.commonDirectory !== repositoryIdentity.commonDirectory) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "session worktree does not belong to the trusted repository");
    }
    const registered = await worktreePaths(repositoryRoot);
    if (!registered.includes(worktreePath)) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "session worktree is not registered by Git");
    }
    return { repositoryRoot, worktreePath };
  }

  async #observe(repositoryRoot: string, worktreePath: string, request: GitRepositoryReadRequest): Promise<FullObservation> {
    const core = await this.#observeCore(repositoryRoot, worktreePath);
    if (request.log.cursor !== undefined && request.log.cursor.repositoryStateDigest !== core.repositoryStateDigest) {
      throw new ProjectFabricCoreError(
        "PROJECTION_RESNAPSHOT_REQUIRED",
        "Git log cursor is bound to another repository state",
        { repositoryStateDigest: core.repositoryStateDigest },
      );
    }
    const branchObservation = await readBranches(worktreePath);
    const worktreeObservation = await readWorktrees(repositoryRoot, worktreePath);
    const logObservation = await readLogs(worktreePath);
    const objectIds = new Map<Sha256Digest, string>();
    objectIds.set(core.head.objectDigest, core.status.headObjectId);
    for (const [digest, objectId] of branchObservation.objectIds) objectIds.set(digest, objectId);
    for (const [digest, objectId] of worktreeObservation.objectIds) objectIds.set(digest, objectId);
    for (const [digest, objectId] of logObservation.objectIds) objectIds.set(digest, objectId);

    const parsedLog = paginateLogs(
      logObservation.items,
      core.repositoryStateDigest,
      request.log.cursor?.afterObjectDigest,
      request.log.limit,
    );
    const diff = await readDiff(worktreePath, request.diff, core, objectIds);
    return {
      core,
      projectionFenceDigest: projectionFenceDigest(
        core,
        branchObservation.fenceDigest,
        worktreeObservation.fenceDigest,
      ),
      diffBytes: diff.bytes,
      diffBaseDigest: diff.baseDigest,
      diffTargetDigest: diff.targetDigest,
      log: parsedLog,
      branches: { items: branchObservation.items, truncated: branchObservation.truncated },
      worktrees: { items: worktreeObservation.items, truncated: worktreeObservation.truncated },
    };
  }

  async #observeProjectionFence(
    repositoryRoot: string,
    worktreePath: string,
  ): Promise<{ core: CoreObservation; digest: Sha256Digest }> {
    const core = await this.#observeCore(repositoryRoot, worktreePath);
    const branches = await readBranches(worktreePath);
    const worktrees = await readWorktrees(repositoryRoot, worktreePath);
    return {
      core,
      digest: projectionFenceDigest(core, branches.fenceDigest, worktrees.fenceDigest),
    };
  }

  async #observeCore(repositoryRoot: string, worktreePath: string): Promise<CoreObservation> {
    const identity = await repositoryIdentityAt(worktreePath);
    const repositoryIdentity = worktreePath === repositoryRoot ? identity : await repositoryIdentityAt(repositoryRoot);
    if (identity.commonDirectory !== repositoryIdentity.commonDirectory) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "worktree repository identity changed");
    }
    const status = parseStatus((await runGit(worktreePath, ["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all", "--ignore-submodules=none"])).stdout);
    const index = (await runGit(worktreePath, ["ls-files", "--stage", "-z"])).stdout;
    const remoteRefs = (await runGit(worktreePath, ["for-each-ref", "--format=%(refname)%00%(objectname)", "refs/remotes"])).stdout;
    const remoteConfiguration = (await runGit(
      worktreePath,
      ["config", "--null", "--get-regexp", "^remote\\..*\\.(url|pushurl|fetch)$"],
      { allowedExitCodes: [1] },
    )).stdout;
    const changedPaths = uniqueSorted([
      ...status.staged,
      ...status.unstaged,
      ...status.untracked,
      ...status.conflicted,
    ]);
    const contentDigest = await hashChangedWorktreePaths(worktreePath, changedPaths);
    const head = await gitHead(worktreePath, status);
    const headDigest = sha256Digest(canonicalJson({ head }));
    const indexDigest = sha256Buffers([Buffer.from("git-index-v1\0"), index]);
    const worktreeDigest = sha256Buffers([Buffer.from("git-worktree-v1\0"), status.raw, Buffer.from(contentDigest)]);
    const remoteDigest = sha256Buffers([Buffer.from("git-remotes-v1\0"), remoteRefs, remoteConfiguration]);
    const repositoryStateDigest = sha256Digest(canonicalJson({ headDigest, indexDigest, worktreeDigest, remoteDigest }));
    return {
      identity,
      status,
      head,
      headDigest,
      indexDigest,
      worktreeDigest,
      remoteDigest,
      repositoryStateDigest,
      operationState: await operationState(worktreePath),
      upstream: gitUpstream(status),
    };
  }

  #globalRevision(): number {
    return integer(row(this.#database.prepare(`
      SELECT revision FROM daemon_global_state WHERE singleton=1
    `).get(), "daemon global state"), "revision");
  }

  async #writePrivateDiff(
    bytes: Buffer,
    scope: { projectId: string; projectSessionId: string | null },
  ) {
    const stateRoot = await realpath(this.#privateStateRoot);
    if (stateRoot !== this.#privateStateRoot) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "private state root is not canonical");
    }
    const privateRoot = join(stateRoot, "private");
    const diffRoot = join(privateRoot, "git-diffs");
    await mkdir(privateRoot, { recursive: true, mode: 0o700 });
    await assertExactRealpath(privateRoot, "private artifact root");
    await chmod(privateRoot, 0o700);
    await mkdir(diffRoot, { recursive: true, mode: 0o700 });
    await assertExactRealpath(diffRoot, "private Git diff root");
    await chmod(diffRoot, 0o700);
    const digest = sha256Buffers([bytes]);
    const hex = digest.slice("sha256:".length);
    const filename = `${hex}.patch`;
    const path = join(diffRoot, filename);
    try {
      const handle = await open(
        path,
        constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY | constants.O_NOFOLLOW,
        0o400,
      );
      try {
        await handle.writeFile(bytes);
        await handle.sync();
        await handle.chmod(0o400);
      } finally {
        await handle.close();
      }
    } catch (error: unknown) {
      if (!isErrno(error, "EEXIST")) throw error;
      const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
      try {
        const stored = await handle.stat();
        if (!stored.isFile()) {
          throw new ProjectFabricCoreError("CONFLICT", "content-addressed Git diff artifact is not a regular file");
        }
        const existing = await handle.readFile();
        if (!existing.equals(bytes)) {
          throw new ProjectFabricCoreError("CONFLICT", "content-addressed Git diff artifact conflicts with stored bytes");
        }
        await handle.chmod(0o400);
      } finally {
        await handle.close();
      }
    }
    const registered = this.#artifactRegistry.register({
      projectId: scope.projectId,
      projectSessionId: scope.projectSessionId,
      runId: null,
      taskId: null,
      publisherKind: "fabric",
      publisherRef: "fabric-git-private-diff",
      publisherAgentId: null,
      sourceKind: "git-private-diff",
      evidenceKind: "diff",
      relativePath: `private/git-diffs/${filename}`,
      digest,
    });
    return parseArtifactRef(registered.artifactRef, "gitRepository.diff.artifactRef");
  }
}

function unavailableHostedChecks(
  binding: Pick<GitHostedChecksBinding, "snapshotRevision" | "observedAt">,
  reason: string,
): ProjectionFact<GitHostedChecks | null, "github"> {
  return {
    freshness: "unavailable",
    source: "github",
    revision: binding.snapshotRevision,
    observedAt: parseTimestamp(binding.observedAt, "gitRepository.hostedChecks.observedAt"),
    reason,
  };
}

function assertHostedChecksFact(
  fact: ProjectionFact<GitHostedChecks | null, "github">,
  expectedHeadObjectDigest: Sha256Digest,
): void {
  if (fact.source !== "github" || !Number.isSafeInteger(fact.revision) || fact.revision < 0) {
    throw new TypeError("hosted checks fact identity is invalid");
  }
  parseTimestamp(fact.observedAt, "gitRepository.hostedChecks.observedAt");
  if (fact.freshness === "unavailable") {
    assertHostedText(fact.reason, "hosted checks unavailability reason", 1024);
    return;
  }
  const values = fact.freshness === "conflict" ? fact.candidates : [fact.value];
  if (values.length < 1 || values.length > 16) throw new TypeError("hosted checks fact candidate count is invalid");
  for (const value of values) {
    if (value === null) continue;
    assertHostedText(value.repository, "hosted checks repository", 1024);
    if (value.headObjectDigest !== expectedHeadObjectDigest) {
      throw new TypeError("hosted checks fact is bound to another Git object");
    }
    const counts = [value.total, value.passing, value.failing, value.pending];
    if (counts.some((count) => !Number.isSafeInteger(count) || count < 0 || count > 100)) {
      throw new TypeError("hosted checks counts exceed the bounded projection");
    }
    if (value.passing + value.failing + value.pending !== value.total) {
      throw new TypeError("hosted checks counts are inconsistent");
    }
  }
}

function assertHostedText(value: string, label: string, maximumBytes: number): void {
  const bytes = Buffer.byteLength(value, "utf8");
  if (
    bytes < 1 || bytes > maximumBytes ||
    /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f\u009b]/u.test(value) || HOSTED_SECRET_PATTERN.test(value)
  ) {
    throw new TypeError(`${label} is unsafe or exceeds its bound`);
  }
}

function resnapshot(currentSnapshotRevision: number): GitRepositoryReadResult {
  return { status: "resnapshot-required", reason: "snapshot-mismatch", currentSnapshotRevision };
}

async function repositoryIdentityAt(worktreePath: string): Promise<RepositoryIdentity> {
  const output = (await runGit(worktreePath, [
    "rev-parse",
    "--path-format=absolute",
    "--show-toplevel",
    "--git-common-dir",
    "--git-dir",
    "--show-object-format",
  ])).stdout.toString("utf8").trim().split("\n");
  if (output.length !== 4) throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git repository identity is incomplete");
  const [topLevelValue, commonValue, gitValue, formatValue] = output;
  if (topLevelValue === undefined || commonValue === undefined || gitValue === undefined) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git repository identity is incomplete");
  }
  if (formatValue !== "sha1" && formatValue !== "sha256") {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git object format is unsupported");
  }
  const topLevel = await realpath(topLevelValue);
  const commonDirectory = await realpath(commonValue);
  const gitDirectory = await realpath(gitValue);
  return { topLevel, commonDirectory, gitDirectory, objectFormat: formatValue };
}

function parseStatus(raw: Buffer): ParsedStatus {
  const records = raw.toString("utf8").split("\0");
  let headObjectId = "";
  let headName: string | null = null;
  let upstreamShorthand: string | null = null;
  let ahead = 0;
  let behind = 0;
  const staged = new Set<string>();
  const unstaged = new Set<string>();
  const untracked = new Set<string>();
  const conflicted = new Set<string>();
  for (let index = 0; index < records.length; index += 1) {
    const record = records[index];
    if (record === undefined || record.length === 0) continue;
    if (record.startsWith("# branch.oid ")) headObjectId = record.slice("# branch.oid ".length);
    else if (record.startsWith("# branch.head ")) headName = record.slice("# branch.head ".length);
    else if (record.startsWith("# branch.upstream ")) upstreamShorthand = record.slice("# branch.upstream ".length);
    else if (record.startsWith("# branch.ab ")) {
      const match = /^# branch\.ab \+([0-9]+) -([0-9]+)$/u.exec(record);
      if (match !== null) {
        ahead = Number(match[1]);
        behind = Number(match[2]);
      }
    } else if (record.startsWith("1 ") || record.startsWith("2 ")) {
      const xy = record.slice(2, 4);
      const path = afterSpaces(record, record.startsWith("2 ") ? 9 : 8);
      if (xy[0] !== ".") staged.add(path);
      if (xy[1] !== ".") unstaged.add(path);
      if (record.startsWith("2 ")) index += 1;
    } else if (record.startsWith("u ")) {
      conflicted.add(afterSpaces(record, 10));
    } else if (record.startsWith("? ")) {
      untracked.add(record.slice(2));
    }
  }
  if (!NATIVE_OBJECT_PATTERN.test(headObjectId)) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git repository has no readable HEAD object");
  }
  const headRef = headName === null || headName === "(detached)" ? null : `refs/heads/${headName}`;
  return {
    raw,
    headObjectId,
    headRef,
    upstream: upstreamShorthand === null ? null : { shorthand: upstreamShorthand, ahead, behind },
    staged: uniqueSorted(staged),
    unstaged: uniqueSorted(unstaged),
    untracked: uniqueSorted(untracked),
    conflicted: uniqueSorted(conflicted),
  };
}

async function gitHead(worktreePath: string, status: ParsedStatus): Promise<GitHead> {
  const digestValue = requiredObjectDigest(
    await readObjectDigests(worktreePath, [status.headObjectId]),
    status.headObjectId,
  );
  return status.headRef === null
    ? { detached: true, objectDigest: digestValue }
    : { detached: false, refName: status.headRef, objectDigest: digestValue };
}

function gitUpstream(status: ParsedStatus): GitUpstream | null {
  if (status.upstream === null) return null;
  const separator = status.upstream.shorthand.indexOf("/");
  if (separator <= 0 || separator === status.upstream.shorthand.length - 1) return null;
  const remoteName = status.upstream.shorthand.slice(0, separator);
  const branch = status.upstream.shorthand.slice(separator + 1);
  if (!isProtocolIdentifier(remoteName) || Buffer.byteLength(branch, "utf8") > 1000) return null;
  return { remoteName, branchName: `refs/remotes/${remoteName}/${branch}`, ahead: status.upstream.ahead, behind: status.upstream.behind };
}

async function operationState(worktreePath: string): Promise<GitOperationState> {
  const paths = (await runGit(worktreePath, [
    "rev-parse",
    "--path-format=absolute",
    "--git-path",
    "rebase-merge",
    "--git-path",
    "rebase-apply",
    "--git-path",
    "MERGE_HEAD",
    "--git-path",
    "CHERRY_PICK_HEAD",
    "--git-path",
    "BISECT_LOG",
  ])).stdout.toString("utf8").trim().split("\n");
  if (paths.slice(0, 2).some(existsSync)) return { kind: "rebase" };
  if (paths[2] !== undefined && existsSync(paths[2])) return { kind: "merge" };
  if (paths[3] !== undefined && existsSync(paths[3])) return { kind: "cherry-pick" };
  if (paths[4] !== undefined && existsSync(paths[4])) return { kind: "bisect" };
  return { kind: "clean" };
}

async function readBranches(worktreePath: string) {
  const output = (await runGit(worktreePath, [
    "for-each-ref",
    "--format=%(refname)%00%(objectname)%00%(HEAD)%00%(upstream)",
    "refs/heads",
  ])).stdout.toString("utf8");
  const stored = output.split("\n").filter((entry) => entry.length > 0).map((entry) => {
    const [refName, objectId, current, upstreamRef = ""] = entry.split("\0");
    if (refName === undefined || objectId === undefined || !NATIVE_OBJECT_PATTERN.test(objectId)) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git branch record is invalid");
    }
    return { refName, objectId, current, upstreamRef };
  });
  const digests = await readObjectDigests(worktreePath, stored.map((entry) => entry.objectId));
  const objectIds = new Map<Sha256Digest, string>();
  const all = stored.map((entry): GitBranchRecord => {
    const { refName, objectId, current, upstreamRef } = entry;
    if (Buffer.byteLength(refName, "utf8") > 1024) {
      throw new ProjectFabricCoreError("RESOURCE_EXHAUSTED", "Git branch ref exceeds the protocol bound");
    }
    const match = /^refs\/remotes\/([^/]+)\/(.+)$/u.exec(upstreamRef);
    const digest = requiredObjectDigest(digests, objectId);
    objectIds.set(digest, objectId);
    return {
      refName,
      objectDigest: digest,
      checkedOut: current === "*",
      upstream: match === null || !isProtocolIdentifier(match[1] ?? "")
        ? null
        : { remoteName: match[1] ?? "", branchName: upstreamRef },
    };
  });
  all.sort((left, right) => compareText(left.refName, right.refName));
  return {
    items: all.slice(0, MAX_BRANCHES),
    truncated: all.length > MAX_BRANCHES,
    objectIds,
    fenceDigest: sha256Digest(canonicalJson(all)),
  };
}

async function readWorktrees(repositoryRoot: string, currentPath: string) {
  const output = (await runGit(repositoryRoot, ["worktree", "list", "--porcelain", "-z"])).stdout.toString("utf8");
  const stored: Array<{ path: string; nativeHead: string; branchRef: string | null; locked: boolean }> = [];
  for (const record of output.split("\0\0")) {
    if (record.length === 0) continue;
    const fields = record.split("\0");
    const pathField = fields.find((field) => field.startsWith("worktree "));
    const headField = fields.find((field) => field.startsWith("HEAD "));
    const branchField = fields.find((field) => field.startsWith("branch "));
    if (pathField === undefined || headField === undefined) continue;
    const path = await realpath(pathField.slice("worktree ".length)).catch(() => null);
    const nativeHead = headField.slice("HEAD ".length);
    if (path === null || Buffer.byteLength(path, "utf8") > 4096 || !NATIVE_OBJECT_PATTERN.test(nativeHead)) continue;
    stored.push({
      path,
      nativeHead,
      branchRef: branchField === undefined ? null : branchField.slice("branch ".length),
      locked: fields.some((field) => field === "locked" || field.startsWith("locked ")),
    });
  }
  const digests = await readObjectDigests(repositoryRoot, stored.map((entry) => entry.nativeHead));
  const all: GitWorktreeRecord[] = [];
  const objectIds = new Map<Sha256Digest, string>();
  for (const entry of stored) {
    const { path, nativeHead, branchRef, locked } = entry;
    const digest = requiredObjectDigest(digests, nativeHead);
    objectIds.set(digest, nativeHead);
    all.push({
      canonicalPath: path,
      head: branchRef === null
        ? { detached: true, objectDigest: digest }
        : { detached: false, refName: branchRef, objectDigest: digest },
      current: path === currentPath,
      locked,
    });
  }
  all.sort((left, right) => compareText(left.canonicalPath, right.canonicalPath));
  return {
    items: all.slice(0, MAX_WORKTREES),
    truncated: all.length > MAX_WORKTREES,
    objectIds,
    fenceDigest: sha256Digest(canonicalJson(all)),
  };
}

function projectionFenceDigest(
  core: CoreObservation,
  branchesDigest: Sha256Digest,
  worktreesDigest: Sha256Digest,
): Sha256Digest {
  return sha256Digest(canonicalJson({
    repositoryStateDigest: core.repositoryStateDigest,
    operationState: core.operationState,
    branchesDigest,
    worktreesDigest,
  }));
}

async function worktreePaths(repositoryRoot: string): Promise<string[]> {
  const output = (await runGit(repositoryRoot, ["worktree", "list", "--porcelain", "-z"])).stdout.toString("utf8");
  const paths: string[] = [];
  for (const record of output.split("\0\0")) {
    const pathField = record.split("\0").find((field) => field.startsWith("worktree "));
    if (pathField === undefined) continue;
    const path = await realpath(pathField.slice("worktree ".length)).catch(() => null);
    if (path !== null) paths.push(path);
  }
  return paths;
}

async function readLogs(worktreePath: string) {
  const output = (await runGit(worktreePath, [
    "log",
    `--max-count=${String(MAX_LOG_SCAN + 1)}`,
    "--format=%H%x00%P%x00%s%x00%aI%x00",
    "HEAD",
  ])).stdout.toString("utf8");
  const stored: Array<{ objectId: string; parents: string[]; subject: string; authoredAt: string }> = [];
  const nativeIds = new Set<string>();
  const objectIds = new Map<Sha256Digest, string>();
  for (const line of output.split("\n")) {
    if (line.length === 0) continue;
    const [objectId, parents = "", subject = "", authoredAt] = line.split("\0");
    if (objectId === undefined || authoredAt === undefined || !NATIVE_OBJECT_PATTERN.test(objectId)) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git log record is invalid");
    }
    const parentIds = parents.length === 0 ? [] : parents.split(" ");
    if (parentIds.length > 64) {
      throw new ProjectFabricCoreError("RESOURCE_EXHAUSTED", "Git commit parent list exceeds the protocol bound");
    }
    nativeIds.add(objectId);
    for (const parent of parentIds) nativeIds.add(parent);
    stored.push({ objectId, parents: parentIds, subject, authoredAt });
  }
  const digests = await readObjectDigests(worktreePath, [...nativeIds]);
  const entries = stored.map((entry): GitLogEntry => {
    const digest = requiredObjectDigest(digests, entry.objectId);
    objectIds.set(digest, entry.objectId);
    return {
      objectDigest: digest,
      parentObjectDigests: entry.parents.map((parent) => requiredObjectDigest(digests, parent)),
      subject: truncateUtf8(entry.subject, 1024),
      authorTimestamp: parseTimestamp(entry.authoredAt, "gitRepository.log.authorTimestamp"),
    };
  });
  return { items: entries, objectIds };
}

function paginateLogs(
  logs: GitLogEntry[],
  repositoryStateDigest: Sha256Digest,
  after: Sha256Digest | undefined,
  limit: number,
): GitLogPage {
  let start = 0;
  if (after !== undefined) {
    const index = logs.findIndex((entry) => entry.objectDigest === after);
    if (index < 0) throw new ProjectFabricCoreError("NOT_FOUND", "Git log cursor object is outside the bounded history window");
    start = index + 1;
  }
  const items = logs.slice(start, start + limit);
  const hasMore = start + items.length < logs.length;
  return hasMore
    ? {
        items,
        hasMore: true,
        nextCursor: {
          repositoryStateDigest,
          afterObjectDigest: items.at(-1)?.objectDigest ?? after ?? logs[0]?.objectDigest ?? repositoryStateDigest,
        },
      }
    : { items, hasMore: false, nextCursor: null };
}

async function readDiff(
  worktreePath: string,
  selector: GitRepositoryReadRequest["diff"],
  core: CoreObservation,
  objectIds: ReadonlyMap<Sha256Digest, string>,
): Promise<{ bytes: Buffer; baseDigest: Sha256Digest; targetDigest: Sha256Digest }> {
  let args: string[];
  let baseDigest: Sha256Digest;
  let targetDigest: Sha256Digest;
  if (selector.kind === "working-tree") {
    args = ["diff", "--binary", "--full-index", "--no-ext-diff", "--no-textconv", "--"];
    baseDigest = core.headDigest;
    targetDigest = core.worktreeDigest;
  } else if (selector.kind === "staged") {
    args = ["diff", "--cached", "--binary", "--full-index", "--no-ext-diff", "--no-textconv", "--"];
    baseDigest = core.headDigest;
    targetDigest = core.indexDigest;
  } else {
    const base = findNativeObjectId(selector.baseObjectDigest, objectIds);
    const target = findNativeObjectId(selector.targetObjectDigest, objectIds);
    args = ["diff", "--binary", "--full-index", "--no-ext-diff", "--no-textconv", base, target, "--"];
    baseDigest = selector.baseObjectDigest;
    targetDigest = selector.targetObjectDigest;
  }
  const result = await runGit(worktreePath, args, { maximumBytes: MAX_DIFF_ARTIFACT_BYTES, truncate: true });
  return { bytes: result.stdout, baseDigest, targetDigest };
}

function findNativeObjectId(
  digest: Sha256Digest,
  objectIds: ReadonlyMap<Sha256Digest, string>,
): string {
  const direct = objectIds.get(digest);
  if (direct !== undefined) return direct;
  throw new ProjectFabricCoreError("NOT_FOUND", "Git object digest is outside the bounded repository view");
}

async function readObjectDigests(worktreePath: string, requestedIds: readonly string[]): Promise<Map<string, Sha256Digest>> {
  const objectIds = uniqueSorted(requestedIds);
  for (const objectId of objectIds) {
    if (!NATIVE_OBJECT_PATTERN.test(objectId)) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git object ID is invalid");
    }
  }
  if (objectIds.length === 0) return new Map();
  const output = (await runGit(worktreePath, ["cat-file", "--batch"], {
    stdin: Buffer.from(`${objectIds.join("\n")}\n`, "utf8"),
  })).stdout;
  const digests = new Map<string, Sha256Digest>();
  let offset = 0;
  for (const requestedId of objectIds) {
    const headerEnd = output.indexOf(0x0a, offset);
    if (headerEnd < 0) throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git object batch header is incomplete");
    const header = output.subarray(offset, headerEnd).toString("utf8");
    const [resolvedId, objectType, sizeText, unexpected] = header.split(" ");
    if (
      resolvedId === undefined || objectType === undefined || sizeText === undefined || unexpected !== undefined ||
      !NATIVE_OBJECT_PATTERN.test(resolvedId) || !/^[a-z-]+$/u.test(objectType) || !/^[0-9]+$/u.test(sizeText)
    ) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git object batch record is invalid");
    }
    const size = Number(sizeText);
    if (!Number.isSafeInteger(size) || size < 0) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git object size is invalid");
    }
    const contentStart = headerEnd + 1;
    const contentEnd = contentStart + size;
    if (contentEnd >= output.length || output[contentEnd] !== 0x0a) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git object batch content is incomplete");
    }
    const digest = sha256Buffers([
      Buffer.from(`${objectType} ${String(size)}\0`, "utf8"),
      output.subarray(contentStart, contentEnd),
    ]);
    if (resolvedId.length === 64 && digest !== `sha256:${resolvedId}`) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git SHA-256 object identity does not match its content");
    }
    digests.set(requestedId, digest);
    offset = contentEnd + 1;
  }
  if (offset !== output.length) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git object batch returned unexpected trailing bytes");
  }
  return digests;
}

function requiredObjectDigest(digests: ReadonlyMap<string, Sha256Digest>, objectId: string): Sha256Digest {
  const digest = digests.get(objectId);
  if (digest === undefined) throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git object digest is unavailable");
  return digest;
}

async function hashChangedWorktreePaths(worktreeRoot: string, paths: readonly string[]): Promise<string> {
  const hashParts: Buffer[] = [Buffer.from("git-worktree-content-v1\0")];
  let total = 0;
  for (const path of paths) {
    assertGitRelativePath(path);
    const absolute = resolve(worktreeRoot, path);
    if (!contains(worktreeRoot, absolute)) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "Git path escaped the worktree");
    const parent = await realpath(dirname(absolute)).catch(() => null);
    if (parent === null || !contains(worktreeRoot, parent)) {
      hashParts.push(Buffer.from(`${path}\0missing-parent\0`));
      continue;
    }
    const info = await lstat(absolute).catch(() => null);
    if (info === null) {
      hashParts.push(Buffer.from(`${path}\0missing\0`));
      continue;
    }
    if (info.isSymbolicLink()) {
      const target = await readlink(absolute);
      hashParts.push(Buffer.from(`${path}\0symlink\0${target}\0`));
      continue;
    }
    if (!info.isFile()) {
      hashParts.push(Buffer.from(`${path}\0other\0${String(info.mode)}\0`));
      continue;
    }
    const handle = await open(absolute, constants.O_RDONLY | constants.O_NOFOLLOW);
    const fileHash = createHash("sha256");
    try {
      for await (const chunk of handle.createReadStream({ autoClose: false })) {
        const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
        total += bytes.length;
        if (total > MAX_WORKTREE_CONTENT_BYTES) {
          throw new ProjectFabricCoreError("RESOURCE_EXHAUSTED", "changed worktree content exceeds the read bound");
        }
        fileHash.update(bytes);
      }
    } finally {
      await handle.close();
    }
    hashParts.push(Buffer.from(`${path}\0file\0${fileHash.digest("hex")}\0`));
  }
  return sha256Buffers(hashParts);
}

async function assertExactRealpath(path: string, label: string): Promise<void> {
  if (!isAbsolute(path) || Buffer.byteLength(path, "utf8") > 4096 || path.includes("\0")) {
    throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", `${label} is not an absolute canonical path`);
  }
  const resolved = await realpath(path).catch(() => null);
  if (resolved === null || resolved !== path) {
    throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", `${label} changed or resolves through a symlink`);
  }
}

function page(paths: readonly string[]) {
  const sorted = uniqueSorted(paths);
  return { paths: sorted.slice(0, MAX_PATHS), truncated: sorted.length > MAX_PATHS };
}

function uniqueSorted(values: Iterable<string>): string[] {
  return [...new Set(values)].sort(compareText);
}

function compareText(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function afterSpaces(value: string, count: number): string {
  let cursor = 0;
  for (let index = 0; index < count; index += 1) {
    cursor = value.indexOf(" ", cursor);
    if (cursor < 0) throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "Git status record is incomplete");
    cursor += 1;
  }
  return value.slice(cursor);
}

function assertGitRelativePath(path: string): void {
  if (
    path.length === 0 ||
    Buffer.byteLength(path, "utf8") > 4096 ||
    path.includes("\0") ||
    isAbsolute(path) ||
    path.split(/[\\/]/u).includes("..")
  ) {
    throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "Git returned an unsafe repository path");
  }
}

function contains(root: string, candidate: string): boolean {
  const rel = relative(root, candidate);
  return rel === "" || (rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel));
}

function truncateUtf8(value: string, maximumBytes: number): string {
  const bytes = Buffer.from(value, "utf8");
  if (bytes.length <= maximumBytes) return value;
  let end = maximumBytes - 3;
  while (end > 0 && (bytes[end] ?? 0) >>> 6 === 2) end -= 1;
  return `${bytes.subarray(0, end).toString("utf8")}...`;
}

function sha256Digest(value: string): Sha256Digest {
  return parseSha256Digest(`sha256:${hexDigest(value)}`, "gitRepository.digest");
}

function sha256Buffers(values: readonly Buffer[]): Sha256Digest {
  const hash = createHashSafe();
  for (const value of values) hash.update(value);
  return parseSha256Digest(`sha256:${hash.digest("hex")}`, "gitRepository.digest");
}

function hexDigest(value: string): string {
  const hash = createHashSafe();
  hash.update(value);
  return hash.digest("hex");
}

function createHashSafe() {
  return createHash("sha256");
}

function isProtocolIdentifier(value: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/u.test(value);
}

function isErrno(error: unknown, code: string): error is NodeJS.ErrnoException {
  return error instanceof Error && "code" in error && error.code === code;
}

async function runGit(
  cwd: string,
  operationArguments: readonly string[],
  options: {
    maximumBytes?: number;
    truncate?: boolean;
    allowedExitCodes?: readonly number[];
    stdin?: Buffer;
  } = {},
): Promise<GitCommandResult> {
  const maximumBytes = options.maximumBytes ?? MAX_GIT_OUTPUT_BYTES;
  const fixedArguments = [
    "--no-pager",
    "-c", "core.fsmonitor=false",
    "-c", "core.untrackedCache=false",
    "-c", "core.hooksPath=/dev/null",
    "-c", "diff.external=",
    ...operationArguments,
  ];
  return new Promise((resolveResult, rejectResult) => {
    const child = execFile(GIT_EXECUTABLE, fixedArguments, {
      cwd,
      encoding: null,
      env: fixedGitEnvironment(),
      maxBuffer: maximumBytes,
      timeout: GIT_TIMEOUT_MS,
      windowsHide: true,
      shell: false,
    }, (error, stdout) => {
      const bytes = Buffer.isBuffer(stdout) ? stdout : Buffer.from(stdout);
      if (error === null) {
        resolveResult({ stdout: bytes, truncated: false });
        return;
      }
      const exitCode = typeof error.code === "number" ? error.code : null;
      if (exitCode !== null && options.allowedExitCodes?.includes(exitCode) === true) {
        resolveResult({ stdout: bytes, truncated: false });
        return;
      }
      if (error.code === "ERR_CHILD_PROCESS_STDIO_MAXBUFFER" && options.truncate === true) {
        const retained = Math.max(0, maximumBytes - DIFF_TRUNCATION_MARKER.length);
        resolveResult({
          stdout: Buffer.concat([bytes.subarray(0, retained), DIFF_TRUNCATION_MARKER]),
          truncated: true,
        });
        return;
      }
      if (error.killed) {
        rejectResult(new ProjectFabricCoreError("DEADLINE_EXCEEDED", "fixed Git read timed out"));
        return;
      }
      if (error.code === "ERR_CHILD_PROCESS_STDIO_MAXBUFFER") {
        rejectResult(new ProjectFabricCoreError("RESOURCE_EXHAUSTED", "fixed Git read exceeded its output bound"));
        return;
      }
      rejectResult(new ProjectFabricCoreError("RECOVERY_REQUIRED", "fixed Git read failed"));
    });
    if (options.stdin !== undefined) child.stdin?.end(options.stdin);
  });
}

function fixedGitEnvironment(): NodeJS.ProcessEnv {
  return {
    HOME: "/",
    LANG: "C.UTF-8",
    LC_ALL: "C.UTF-8",
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_TERMINAL_PROMPT: "0",
    GIT_OPTIONAL_LOCKS: "0",
  };
}
