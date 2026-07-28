import {
  OPERATOR_ACTIONS,
  assertOperatorCapabilityAuthority,
  parseOperatorCapabilityGrant,
  type JsonValue,
  type OperatorAction,
  type OperatorCapabilityGrant,
  type OperatorMutationContext,
  type OperatorAuthorityBinding,
  type OperatorAttachRequest,
  type OperatorAttachment,
  type OperatorDetachRequest,
  type OperatorHeartbeatRequest,
  type IntegrationInputAttestationRequest,
  type OperatorInputAttestation,
  type ChairBridgeRecoveryIntent,
} from "@local/agent-fabric-protocol";
import { randomBytes } from "node:crypto";
import type Database from "better-sqlite3";
import { isAbsolute, normalize, resolve } from "node:path";

import {
  ProjectFabricCoreError,
  type AuthenticatedOperatorContext,
  type AuthenticatedIntegrationContext,
  type CoreServiceOptions,
} from "../project-session/contracts.js";
import {
  assertExactGateAttestationDigests,
  canonicalStoredGateAttestationDigests,
} from "../gates/attestation-binding.js";
import {
  canonicalJson,
  integer,
  isRow,
  nullableText,
  row,
  sha256,
  text,
  timestampToMillis,
  type Row,
} from "../persistence/row-codec.js";
import {
  openLocalOperatorConsoleTakeoverCapability as openLocalOperatorConsoleTakeoverCapabilityOwner,
} from "./local-takeover-capability.js";

export type OperatorCommandTarget = {
  projectId: string;
  projectSessionId?: string;
  sessionGeneration?: number;
  requiredAction: OperatorAction;
  commandPayload: JsonValue;
};

export type AuthenticatedOperatorCredential = {
  context: AuthenticatedOperatorContext;
  capabilityId: string;
  kind: "project-launch" | "session" | "takeover";
  projectSessionId?: string;
  sessionGeneration?: number;
  actions: OperatorAction[];
};

type CapabilityRow = Row & {
  capability_id: string;
};

const SHA256_DIGEST = /^sha256:[0-9a-f]{64}$/u;
const PROJECT_LAUNCH_ACTIONS = ["read", "launch"] as const;

type ProjectLaunchAction = (typeof PROJECT_LAUNCH_ACTIONS)[number];
type SessionCapabilityAction = Exclude<OperatorAction, "takeover">;

export type LocalOperatorProvisioningInput = {
  canonicalRoot: string;
  trustRecordDigest: string;
  authenticatedSubjectHash: string;
  projectAuthorityGeneration: number;
  principalGeneration: number;
  actions: readonly ProjectLaunchAction[];
  expiresAt: string;
};

type LocalCapabilityMetadata = {
  projectId: string;
  operatorId: string;
  capabilityId: string;
  projectAuthorityGeneration: number;
  principalGeneration: number;
  kind: "project-launch";
  actions: ProjectLaunchAction[];
  issuedAt: string;
  expiresAt: string;
};

export type LocalOperatorProvisioningResult = LocalCapabilityMetadata & (
  | { issued: true; credential: { capabilityId: string; token: string } }
  | { issued: false }
);

export type LocalOperatorConsoleCapabilityInput = Omit<
  LocalOperatorProvisioningInput,
  "principalGeneration"
>;

export type LocalOperatorConsoleCapabilityResult = LocalCapabilityMetadata & {
  issued: true;
  credential: { capabilityId: string; token: string };
};

export type LocalOperatorSessionCapabilityInput = {
  projectId: string;
  canonicalRoot: string;
  trustRecordDigest: string;
  authenticatedSubjectHash: string;
  projectCapability: { capabilityId: string; token: string };
  projectSessionId: string;
  sessionGeneration: number;
  actions: readonly SessionCapabilityAction[];
  expiresAt: string;
  launchEnvelopeExpiresAt: string;
  fresh?: true;
};

type LocalSessionCapabilityMetadata = {
  projectId: string;
  operatorId: string;
  capabilityId: string;
  projectSessionId: string;
  projectAuthorityGeneration: number;
  sessionGeneration: number;
  principalGeneration: number;
  kind: "session";
  actions: SessionCapabilityAction[];
  issuedAt: string;
  expiresAt: string;
};

export type LocalOperatorSessionCapabilityResult = LocalSessionCapabilityMetadata & (
  | { issued: true; credential: { capabilityId: string; token: string } }
  | { issued: false }
);

export type LocalOperatorConsoleSessionCapabilityResult =
  LocalSessionCapabilityMetadata & {
    issued: true;
    credential: { capabilityId: string; token: string };
  };

export type LocalOperatorTakeoverCapabilityInput = {
  projectId: string;
  canonicalRoot: string;
  trustRecordDigest: string;
  authenticatedSubjectHash: string;
  projectCapability: { capabilityId: string; token: string };
  projectSessionId: string;
  expiresAt: string;
};

export type LocalOperatorTakeoverCapabilityResult = {
  projectId: string;
  operatorId: string;
  capabilityId: string;
  projectSessionId: string;
  projectAuthorityGeneration: number;
  sessionGeneration: number;
  principalGeneration: number;
  kind: "takeover";
  actions: ["read", "takeover"];
  issuedAt: string;
  expiresAt: string;
  credential: { capabilityId: string; token: string };
  recoveryIntent: Extract<ChairBridgeRecoveryIntent, { path: "abandon" }>;
};

export type LocalOperatorPrincipalRotationInput = {
  projectId: string;
  operatorId: string;
  canonicalRoot: string;
  trustRecordDigest: string;
  authenticatedSubjectHash: string;
  projectAuthorityGeneration: number;
  expectedPrincipalGeneration: number;
};

export type LocalOperatorPrincipalRotationResult = {
  projectId: string;
  operatorId: string;
  principalGeneration: number;
  revokedCapabilityCount: number;
};

function deterministicIdentifier(prefix: string, value: unknown): string {
  return `${prefix}:${sha256(canonicalJson(value))}`;
}

function exactDigest(value: string, field: string): void {
  if (!SHA256_DIGEST.test(value)) {
    throw new ProjectFabricCoreError("PROTOCOL_INVALID", `${field} must be a lowercase sha256 digest`);
  }
}

function exactGeneration(value: number, field: string): void {
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new ProjectFabricCoreError("PROTOCOL_INVALID", `${field} must be a positive safe integer`);
  }
}

function exactCanonicalRoot(value: string): void {
  if (!isAbsolute(value) || value === "/" || normalize(value) !== value || resolve(value) !== value) {
    throw new ProjectFabricCoreError("PROTOCOL_INVALID", "canonicalRoot must be an exact normalized absolute project root");
  }
}

function futureTimestamp(value: string, now: number, field: string): { millis: number; canonical: string } {
  const millis = Date.parse(value);
  if (!Number.isFinite(millis) || millis <= now) {
    throw new ProjectFabricCoreError("PROTOCOL_INVALID", `${field} must be a future timestamp`);
  }
  return { millis, canonical: new Date(millis).toISOString() };
}

function projectLaunchActions(actions: readonly ProjectLaunchAction[]): ProjectLaunchAction[] {
  if (actions.length === 0 || new Set(actions).size !== actions.length) {
    throw new ProjectFabricCoreError("PROTOCOL_INVALID", "project capability actions must be non-empty and unique");
  }
  if (!actions.every((action) => PROJECT_LAUNCH_ACTIONS.includes(action))) {
    throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "project capability is limited to read and launch");
  }
  return PROJECT_LAUNCH_ACTIONS.filter((action) => actions.includes(action));
}

function sessionCapabilityActions(actions: readonly SessionCapabilityAction[]): SessionCapabilityAction[] {
  if (actions.length === 0 || new Set(actions).size !== actions.length) {
    throw new ProjectFabricCoreError("PROTOCOL_INVALID", "session capability actions must be non-empty and unique");
  }
  const allowed = OPERATOR_ACTIONS.filter((action): action is SessionCapabilityAction => action !== "takeover");
  if (!actions.every((action) => allowed.includes(action))) {
    throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "session capability cannot grant takeover");
  }
  return allowed.filter((action) => actions.includes(action));
}

export class OperatorStore {
  readonly database: Database.Database;
  readonly #clock: () => number;

  constructor(options: CoreServiceOptions) {
    this.database = options.database;
    this.#clock = options.clock ?? Date.now;
  }

  provisionLocalOperator(input: LocalOperatorProvisioningInput): LocalOperatorProvisioningResult {
    exactCanonicalRoot(input.canonicalRoot);
    exactDigest(input.trustRecordDigest, "trustRecordDigest");
    exactDigest(input.authenticatedSubjectHash, "authenticatedSubjectHash");
    exactGeneration(input.projectAuthorityGeneration, "projectAuthorityGeneration");
    exactGeneration(input.principalGeneration, "principalGeneration");
    const now = this.#clock();
    const expiresAt = futureTimestamp(input.expiresAt, now, "expiresAt");
    const actions = projectLaunchActions(input.actions);
    const projectId = deterministicIdentifier("project:local", { canonicalRoot: input.canonicalRoot });
    const operatorId = deterministicIdentifier("operator:local", {
      authenticatedSubjectHash: input.authenticatedSubjectHash,
      projectId,
    });
    const capabilityId = deterministicIdentifier("capability:project-launch", {
      operatorId,
      principalGeneration: input.principalGeneration,
      projectAuthorityGeneration: input.projectAuthorityGeneration,
    });
    const token = `afop_${randomBytes(32).toString("base64url")}`;

    const provision = this.database.transaction((): LocalOperatorProvisioningResult => {
      const existingById = this.database.prepare("SELECT * FROM projects WHERE project_id=?").get(projectId);
      const existingByRoot = this.database.prepare("SELECT * FROM projects WHERE canonical_root=?").get(input.canonicalRoot);
      const existingProject = isRow(existingById) ? existingById : isRow(existingByRoot) ? existingByRoot : undefined;
      if (existingProject === undefined) {
        if (input.projectAuthorityGeneration !== 1) {
          throw new ProjectFabricCoreError("STALE_GENERATION", "new project authority generation must be one");
        }
        this.database.prepare(`
          INSERT INTO projects(
            project_id, canonical_root, trust_record_digest, revision,
            authority_generation, created_at, updated_at
          ) VALUES (?, ?, ?, 1, 1, ?, ?)
        `).run(projectId, input.canonicalRoot, input.trustRecordDigest, now, now);
      } else if (
        text(existingProject, "project_id") !== projectId ||
        text(existingProject, "canonical_root") !== input.canonicalRoot ||
        nullableText(existingProject, "trust_record_digest") !== input.trustRecordDigest
      ) {
        throw new ProjectFabricCoreError("CONFLICT", "trusted project binding conflicts with stored identity");
      } else if (integer(existingProject, "authority_generation") !== input.projectAuthorityGeneration) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "project authority generation changed");
      }

      const currentProject = row(this.database.prepare(`
        SELECT authority_generation FROM projects WHERE project_id=?
      `).get(projectId), "project");
      if (integer(currentProject, "authority_generation") !== input.projectAuthorityGeneration) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "project authority generation changed");
      }

      const principal = this.database.prepare("SELECT * FROM operator_principals WHERE operator_id=?").get(operatorId);
      if (!isRow(principal)) {
        const otherPrincipal = this.database.prepare(`
          SELECT operator_id FROM operator_principals WHERE project_id=? LIMIT 1
        `).get(projectId);
        if (isRow(otherPrincipal)) {
          throw new ProjectFabricCoreError("CONFLICT", "project already has a different local operator identity");
        }
        if (input.principalGeneration !== 1) {
          throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "new operator principal generation must be one");
        }
        this.database.prepare(`
          INSERT INTO operator_principals(
            operator_id, project_id, project_session_id, authenticated_subject_hash,
            project_authority_generation, principal_generation, state, created_at, updated_at
          ) VALUES (?, ?, NULL, ?, ?, 1, 'active', ?, ?)
        `).run(
          operatorId,
          projectId,
          input.authenticatedSubjectHash,
          input.projectAuthorityGeneration,
          now,
          now,
        );
      } else if (
        text(principal, "project_id") !== projectId ||
        text(principal, "authenticated_subject_hash") !== input.authenticatedSubjectHash ||
        integer(principal, "project_authority_generation") !== input.projectAuthorityGeneration ||
        text(principal, "state") !== "active"
      ) {
        throw new ProjectFabricCoreError("CONFLICT", "local operator binding conflicts with stored identity");
      } else if (integer(principal, "principal_generation") !== input.principalGeneration) {
        throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "operator principal generation changed");
      }

      const existingCapability = this.database.prepare(`
        SELECT * FROM operator_capabilities WHERE capability_id=?
      `).get(capabilityId);
      if (isRow(existingCapability)) {
        if (
          text(existingCapability, "operator_id") !== operatorId ||
          text(existingCapability, "project_id") !== projectId ||
          existingCapability.project_session_id !== null ||
          integer(existingCapability, "project_authority_generation") !== input.projectAuthorityGeneration ||
          existingCapability.session_generation !== null ||
          integer(existingCapability, "principal_generation") !== input.principalGeneration ||
          text(existingCapability, "kind") !== "project-launch" ||
          text(existingCapability, "operations_json") !== canonicalJson(actions) ||
          integer(existingCapability, "expires_at") !== expiresAt.millis
        ) {
          throw new ProjectFabricCoreError("CONFLICT", "local provisioning replay changed capability input");
        }
        if (existingCapability.revoked_at !== null) {
          throw new ProjectFabricCoreError("CAPABILITY_REVOKED", "local project capability is revoked");
        }
        return {
          projectId,
          operatorId,
          capabilityId,
          projectAuthorityGeneration: input.projectAuthorityGeneration,
          principalGeneration: input.principalGeneration,
          kind: "project-launch",
          actions,
          issuedAt: new Date(integer(existingCapability, "issued_at")).toISOString(),
          expiresAt: new Date(integer(existingCapability, "expires_at")).toISOString(),
          issued: false,
        };
      }

      const issuedAt = new Date(now).toISOString();
      const grant = parseOperatorCapabilityGrant({
        capabilityId,
        operatorId,
        projectId,
        projectAuthorityGeneration: input.projectAuthorityGeneration,
        principalGeneration: input.principalGeneration,
        issuedAt,
        expiresAt: expiresAt.canonical,
        status: "active",
        kind: "project-launch",
        actions,
      });
      this.issueCapability(grant, token);
      return {
        projectId,
        operatorId,
        capabilityId,
        projectAuthorityGeneration: input.projectAuthorityGeneration,
        principalGeneration: input.principalGeneration,
        kind: "project-launch",
        actions,
        issuedAt,
        expiresAt: expiresAt.canonical,
        issued: true,
        credential: { capabilityId, token },
      };
    });
    return provision.immediate();
  }

  openLocalOperatorConsoleCapability(
    input: LocalOperatorConsoleCapabilityInput,
  ): LocalOperatorConsoleCapabilityResult {
    exactCanonicalRoot(input.canonicalRoot);
    exactDigest(input.trustRecordDigest, "trustRecordDigest");
    exactDigest(input.authenticatedSubjectHash, "authenticatedSubjectHash");
    exactGeneration(input.projectAuthorityGeneration, "projectAuthorityGeneration");
    const requestedExpiry = futureTimestamp(input.expiresAt, this.#clock(), "expiresAt");
    const actions = projectLaunchActions(input.actions);
    const projectId = deterministicIdentifier("project:local", {
      canonicalRoot: input.canonicalRoot,
    });
    const operatorId = deterministicIdentifier("operator:local", {
      authenticatedSubjectHash: input.authenticatedSubjectHash,
      projectId,
    });

    const issue = this.database.transaction((): LocalOperatorConsoleCapabilityResult => {
      const now = this.#clock();
      const existingProject = this.database.prepare(
        "SELECT * FROM projects WHERE project_id=? OR canonical_root=?",
      ).get(projectId, input.canonicalRoot);
      if (!isRow(existingProject)) {
        if (input.projectAuthorityGeneration !== 1) {
          throw new ProjectFabricCoreError(
            "STALE_GENERATION",
            "new project authority generation must be one",
          );
        }
        this.database.prepare(`
          INSERT INTO projects(
            project_id, canonical_root, trust_record_digest, revision,
            authority_generation, created_at, updated_at
          ) VALUES (?, ?, ?, 1, 1, ?, ?)
        `).run(projectId, input.canonicalRoot, input.trustRecordDigest, now, now);
      } else if (
        text(existingProject, "project_id") !== projectId ||
        text(existingProject, "canonical_root") !== input.canonicalRoot ||
        nullableText(existingProject, "trust_record_digest") !== input.trustRecordDigest ||
        integer(existingProject, "authority_generation") !== input.projectAuthorityGeneration
      ) {
        throw new ProjectFabricCoreError(
          "CONFLICT",
          "trusted project binding conflicts with stored identity",
        );
      }

      const existingPrincipal = this.database.prepare(`
        SELECT * FROM operator_principals WHERE operator_id=?
      `).get(operatorId);
      let principalGeneration: number;
      if (!isRow(existingPrincipal)) {
        const otherPrincipal = this.database.prepare(`
          SELECT operator_id FROM operator_principals WHERE project_id=? LIMIT 1
        `).get(projectId);
        if (isRow(otherPrincipal)) {
          throw new ProjectFabricCoreError(
            "CONFLICT",
            "project already has a different local operator identity",
          );
        }
        principalGeneration = 1;
        this.database.prepare(`
          INSERT INTO operator_principals(
            operator_id, project_id, project_session_id, authenticated_subject_hash,
            project_authority_generation, principal_generation, state, created_at, updated_at
          ) VALUES (?, ?, NULL, ?, ?, 1, 'active', ?, ?)
        `).run(
          operatorId,
          projectId,
          input.authenticatedSubjectHash,
          input.projectAuthorityGeneration,
          now,
          now,
        );
      } else {
        if (
          text(existingPrincipal, "project_id") !== projectId ||
          text(existingPrincipal, "authenticated_subject_hash") !== input.authenticatedSubjectHash ||
          integer(existingPrincipal, "project_authority_generation") !== input.projectAuthorityGeneration ||
          text(existingPrincipal, "state") !== "active"
        ) {
          throw new ProjectFabricCoreError(
            "CONFLICT",
            "local operator binding conflicts with stored identity",
          );
        }
        principalGeneration = integer(existingPrincipal, "principal_generation");
      }

      const epochRow = this.database.prepare(`
        SELECT MAX(expires_at) AS expires_at
          FROM operator_capabilities
         WHERE operator_id=? AND project_id=? AND principal_generation=?
           AND kind='project-launch' AND revoked_at IS NULL
      `).get(operatorId, projectId, principalGeneration);
      if (!isRow(epochRow)) throw new Error("local operator capability epoch is unavailable");
      const storedEpoch = epochRow.expires_at;
      let epochExpiry = storedEpoch === null ? null : Number(storedEpoch);
      if (
        epochExpiry !== null &&
        (!Number.isSafeInteger(epochExpiry) || epochExpiry < 1)
      ) {
        throw new Error("local operator capability epoch is invalid");
      }
      if (epochExpiry !== null && epochExpiry <= now) {
        const rotated = this.database.prepare(`
          UPDATE operator_principals
             SET principal_generation=principal_generation+1, updated_at=?
           WHERE operator_id=? AND project_id=? AND principal_generation=? AND state='active'
        `).run(now, operatorId, projectId, principalGeneration);
        if (rotated.changes !== 1) {
          throw new ProjectFabricCoreError(
            "STALE_PRINCIPAL_GENERATION",
            "operator principal generation changed",
          );
        }
        this.database.prepare(`
          UPDATE operator_capabilities SET revoked_at=?
           WHERE operator_id=? AND project_id=?
             AND principal_generation<=? AND revoked_at IS NULL
        `).run(now, operatorId, projectId, principalGeneration);
        principalGeneration += 1;
        epochExpiry = null;
      }

      const expiresAtMillis = Math.min(
        requestedExpiry.millis,
        epochExpiry ?? requestedExpiry.millis,
      );
      if (expiresAtMillis <= now) {
        throw new ProjectFabricCoreError(
          "CAPABILITY_EXPIRED",
          "local operator capability epoch expired",
        );
      }
      const expiresAt = new Date(expiresAtMillis).toISOString();
      const capabilityId = deterministicIdentifier("capability:project-console", {
        operatorId,
        principalGeneration,
        nonce: randomBytes(32).toString("base64url"),
      });
      const token = `afop_${randomBytes(32).toString("base64url")}`;
      const issuedAt = new Date(now).toISOString();
      this.issueCapability(parseOperatorCapabilityGrant({
        capabilityId,
        operatorId,
        projectId,
        projectAuthorityGeneration: input.projectAuthorityGeneration,
        principalGeneration,
        issuedAt,
        expiresAt,
        status: "active",
        kind: "project-launch",
        actions,
      }), token);
      return {
        projectId,
        operatorId,
        capabilityId,
        projectAuthorityGeneration: input.projectAuthorityGeneration,
        principalGeneration,
        kind: "project-launch",
        actions,
        issuedAt,
        expiresAt,
        issued: true,
        credential: { capabilityId, token },
      };
    });
    return issue.immediate();
  }

  issueLocalOperatorSessionCapability(
    input: LocalOperatorSessionCapabilityInput,
  ): LocalOperatorSessionCapabilityResult {
    exactCanonicalRoot(input.canonicalRoot);
    exactDigest(input.trustRecordDigest, "trustRecordDigest");
    exactDigest(input.authenticatedSubjectHash, "authenticatedSubjectHash");
    exactGeneration(input.sessionGeneration, "sessionGeneration");
    if (input.projectCapability.capabilityId.length === 0 || input.projectCapability.token.length === 0) {
      throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "project capability credential is empty");
    }
    const now = this.#clock();
    const expiresAt = futureTimestamp(input.expiresAt, now, "expiresAt");
    const envelopeExpiry = futureTimestamp(input.launchEnvelopeExpiresAt, now, "launchEnvelopeExpiresAt");
    if (expiresAt.millis > envelopeExpiry.millis) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "session capability cannot outlive the reviewed launch envelope");
    }
    const actions = sessionCapabilityActions(input.actions);
    const token = `afop_${randomBytes(32).toString("base64url")}`;

    const issue = this.database.transaction((): LocalOperatorSessionCapabilityResult => {
      const authenticated = this.authenticateCredential(input.projectCapability.token);
      if (
        authenticated.capabilityId !== input.projectCapability.capabilityId ||
        authenticated.kind !== "project-launch" ||
        authenticated.context.projectId !== input.projectId
      ) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "credential is not the exact project capability");
      }
      if (!authenticated.actions.includes("launch")) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "project capability cannot issue session credentials");
      }

      const project = row(this.database.prepare(`
        SELECT canonical_root, trust_record_digest, authority_generation
          FROM projects WHERE project_id=?
      `).get(input.projectId), "project");
      if (
        text(project, "canonical_root") !== input.canonicalRoot ||
        nullableText(project, "trust_record_digest") !== input.trustRecordDigest
      ) {
        throw new ProjectFabricCoreError("CONFLICT", "trusted project binding changed");
      }
      const projectAuthorityGeneration = integer(project, "authority_generation");
      if (projectAuthorityGeneration !== authenticated.context.projectAuthorityGeneration) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "project authority generation changed");
      }

      const principal = row(this.database.prepare(`
        SELECT authenticated_subject_hash, project_authority_generation, principal_generation, state
          FROM operator_principals WHERE operator_id=? AND project_id=?
      `).get(authenticated.context.operatorId, input.projectId), "operator principal");
      if (
        text(principal, "authenticated_subject_hash") !== input.authenticatedSubjectHash ||
        text(principal, "state") !== "active"
      ) {
        throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "local operator subject binding changed");
      }
      const principalGeneration = integer(principal, "principal_generation");
      if (
        integer(principal, "project_authority_generation") !== projectAuthorityGeneration ||
        principalGeneration !== authenticated.context.principalGeneration
      ) {
        throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "operator principal generation changed");
      }

      const session = row(this.database.prepare(`
        SELECT project_id, generation FROM project_sessions WHERE project_session_id=?
      `).get(input.projectSessionId), "project session");
      if (text(session, "project_id") !== input.projectId) {
        throw new ProjectFabricCoreError("WRONG_PROJECT", "project session belongs to another project");
      }
      if (integer(session, "generation") !== input.sessionGeneration) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "project-session generation changed");
      }

      const projectCapability = row(this.database.prepare(`
        SELECT expires_at FROM operator_capabilities WHERE capability_id=?
      `).get(input.projectCapability.capabilityId), "project capability");
      if (expiresAt.millis > integer(projectCapability, "expires_at")) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "session capability cannot outlive the project capability");
      }

      const capabilityId = deterministicIdentifier(
        input.fresh === true ? "capability:session-console" : "capability:session",
        {
          actions,
          expiresAt: expiresAt.canonical,
          launchEnvelopeExpiresAt: envelopeExpiry.canonical,
          operatorId: authenticated.context.operatorId,
          principalGeneration,
          projectAuthorityGeneration,
          projectSessionId: input.projectSessionId,
          sessionGeneration: input.sessionGeneration,
          ...(input.fresh === true
            ? { nonce: randomBytes(32).toString("base64url") }
            : {}),
        },
      );
      const existingCapability = this.database.prepare(`
        SELECT * FROM operator_capabilities WHERE capability_id=?
      `).get(capabilityId);
      if (isRow(existingCapability)) {
        if (
          text(existingCapability, "operator_id") !== authenticated.context.operatorId ||
          text(existingCapability, "project_id") !== input.projectId ||
          nullableText(existingCapability, "project_session_id") !== input.projectSessionId ||
          integer(existingCapability, "project_authority_generation") !== projectAuthorityGeneration ||
          integer(existingCapability, "session_generation") !== input.sessionGeneration ||
          integer(existingCapability, "principal_generation") !== principalGeneration ||
          text(existingCapability, "kind") !== "session" ||
          text(existingCapability, "operations_json") !== canonicalJson(actions) ||
          integer(existingCapability, "expires_at") !== expiresAt.millis
        ) {
          throw new ProjectFabricCoreError("CONFLICT", "session capability identity conflicts with stored input");
        }
        if (existingCapability.revoked_at !== null) {
          throw new ProjectFabricCoreError("CAPABILITY_REVOKED", "session capability is revoked");
        }
        return {
          projectId: input.projectId,
          operatorId: authenticated.context.operatorId,
          capabilityId,
          projectSessionId: input.projectSessionId,
          projectAuthorityGeneration,
          sessionGeneration: input.sessionGeneration,
          principalGeneration,
          kind: "session",
          actions,
          issuedAt: new Date(integer(existingCapability, "issued_at")).toISOString(),
          expiresAt: new Date(integer(existingCapability, "expires_at")).toISOString(),
          issued: false,
        };
      }

      const issuedAt = new Date(now).toISOString();
      const grant = parseOperatorCapabilityGrant({
        capabilityId,
        operatorId: authenticated.context.operatorId,
        projectId: input.projectId,
        projectAuthorityGeneration,
        principalGeneration,
        issuedAt,
        expiresAt: expiresAt.canonical,
        status: "active",
        kind: "session",
        projectSessionId: input.projectSessionId,
        sessionGeneration: input.sessionGeneration,
        actions,
      });
      this.issueCapability(grant, token);
      return {
        projectId: input.projectId,
        operatorId: authenticated.context.operatorId,
        capabilityId,
        projectSessionId: input.projectSessionId,
        projectAuthorityGeneration,
        sessionGeneration: input.sessionGeneration,
        principalGeneration,
        kind: "session",
        actions,
        issuedAt,
        expiresAt: expiresAt.canonical,
        issued: true,
        credential: { capabilityId, token },
      };
    });
    return issue.immediate();
  }

  openLocalOperatorConsoleSessionCapability(
    input: Omit<LocalOperatorSessionCapabilityInput, "fresh">,
  ): LocalOperatorConsoleSessionCapabilityResult {
    const result = this.issueLocalOperatorSessionCapability({ ...input, fresh: true });
    if (!result.issued) {
      throw new ProjectFabricCoreError(
        "CONFLICT",
        "fresh local Console session capability was not issued",
      );
    }
    return result;
  }

  openLocalOperatorConsoleTakeoverCapability(
    input: LocalOperatorTakeoverCapabilityInput,
  ): LocalOperatorTakeoverCapabilityResult {
    return openLocalOperatorConsoleTakeoverCapabilityOwner({
      database: this.database,
      clock: this.#clock,
      authenticateCredential: (token) => this.authenticateCredential(token),
      issueCapability: (grant, token) => this.issueCapability(grant, token),
    }, input);
  }

  rotatePrincipal(input: LocalOperatorPrincipalRotationInput): LocalOperatorPrincipalRotationResult {
    exactCanonicalRoot(input.canonicalRoot);
    exactDigest(input.trustRecordDigest, "trustRecordDigest");
    exactDigest(input.authenticatedSubjectHash, "authenticatedSubjectHash");
    exactGeneration(input.projectAuthorityGeneration, "projectAuthorityGeneration");
    exactGeneration(input.expectedPrincipalGeneration, "expectedPrincipalGeneration");
    const expectedProjectId = deterministicIdentifier("project:local", { canonicalRoot: input.canonicalRoot });
    const expectedOperatorId = deterministicIdentifier("operator:local", {
      authenticatedSubjectHash: input.authenticatedSubjectHash,
      projectId: input.projectId,
    });
    if (input.projectId !== expectedProjectId || input.operatorId !== expectedOperatorId) {
      throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "principal rotation identity does not match its local binding");
    }
    const now = this.#clock();

    const rotate = this.database.transaction((): LocalOperatorPrincipalRotationResult => {
      const project = row(this.database.prepare(`
        SELECT canonical_root, trust_record_digest, authority_generation
          FROM projects WHERE project_id=?
      `).get(input.projectId), "project");
      if (
        text(project, "canonical_root") !== input.canonicalRoot ||
        nullableText(project, "trust_record_digest") !== input.trustRecordDigest
      ) {
        throw new ProjectFabricCoreError("CONFLICT", "trusted project binding changed");
      }
      if (integer(project, "authority_generation") !== input.projectAuthorityGeneration) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "project authority generation changed");
      }

      const principal = row(this.database.prepare(`
        SELECT authenticated_subject_hash, project_authority_generation, principal_generation, state
          FROM operator_principals WHERE operator_id=? AND project_id=?
      `).get(input.operatorId, input.projectId), "operator principal");
      if (
        text(principal, "authenticated_subject_hash") !== input.authenticatedSubjectHash ||
        text(principal, "state") !== "active"
      ) {
        throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "operator principal binding changed");
      }
      if (integer(principal, "project_authority_generation") !== input.projectAuthorityGeneration) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "operator project authority generation changed");
      }
      if (integer(principal, "principal_generation") !== input.expectedPrincipalGeneration) {
        throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "operator principal generation changed");
      }

      const changed = this.database.prepare(`
        UPDATE operator_principals
           SET principal_generation=principal_generation+1, updated_at=?
         WHERE operator_id=? AND project_id=? AND principal_generation=? AND state='active'
      `).run(now, input.operatorId, input.projectId, input.expectedPrincipalGeneration);
      if (changed.changes !== 1) {
        throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "operator principal generation changed");
      }
      const revoked = this.database.prepare(`
        UPDATE operator_capabilities
           SET revoked_at=?
         WHERE operator_id=? AND project_id=?
           AND principal_generation<=? AND revoked_at IS NULL
      `).run(now, input.operatorId, input.projectId, input.expectedPrincipalGeneration);
      return {
        projectId: input.projectId,
        operatorId: input.operatorId,
        principalGeneration: input.expectedPrincipalGeneration + 1,
        revokedCapabilityCount: revoked.changes,
      };
    });
    return rotate.immediate();
  }

  registerPrincipal(input: {
    operatorId: string;
    projectId: string;
    authenticatedSubjectHash: string;
    projectAuthorityGeneration: number;
    principalGeneration?: number;
  }): void {
    const principalGeneration = input.principalGeneration ?? 1;
    const project = row(
      this.database.prepare("SELECT authority_generation FROM projects WHERE project_id=?").get(input.projectId),
      "project",
    );
    if (integer(project, "authority_generation") !== input.projectAuthorityGeneration) {
      throw new ProjectFabricCoreError("STALE_GENERATION", "project authority generation changed");
    }
    const existing = this.database.prepare(`
      SELECT project_id, authenticated_subject_hash, project_authority_generation, principal_generation, state
        FROM operator_principals WHERE operator_id=?
    `).get(input.operatorId);
    if (isRow(existing)) {
      if (
        text(existing, "project_id") !== input.projectId ||
        text(existing, "authenticated_subject_hash") !== input.authenticatedSubjectHash ||
        integer(existing, "project_authority_generation") !== input.projectAuthorityGeneration ||
        integer(existing, "principal_generation") !== principalGeneration ||
        text(existing, "state") !== "active"
      ) {
        throw new ProjectFabricCoreError("CONFLICT", "operator principal registration conflicts with stored identity");
      }
      return;
    }
    const now = this.#clock();
    this.database.prepare(`
      INSERT INTO operator_principals(
        operator_id, project_id, project_session_id, authenticated_subject_hash,
        project_authority_generation, principal_generation, state, created_at, updated_at
      ) VALUES (?, ?, NULL, ?, ?, ?, 'active', ?, ?)
    `).run(
      input.operatorId,
      input.projectId,
      input.authenticatedSubjectHash,
      input.projectAuthorityGeneration,
      principalGeneration,
      now,
      now,
    );
  }

  issueCapability(grant: OperatorCapabilityGrant, token: string): void {
    if (token.length === 0) throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "capability token is empty");
    const principal = row(this.database.prepare(`
      SELECT project_id, project_authority_generation, principal_generation, state
        FROM operator_principals WHERE operator_id=?
    `).get(grant.operatorId), "operator principal");
    if (text(principal, "state") !== "active") {
      throw new ProjectFabricCoreError("CAPABILITY_REVOKED", "operator principal is not active");
    }
    const project = row(this.database.prepare(`
      SELECT authority_generation FROM projects WHERE project_id=?
    `).get(grant.projectId), "project");
    const authorityBinding: OperatorAuthorityBinding = grant.kind === "project-launch"
      ? {
          projectId: grant.projectId,
          projectAuthorityGeneration: integer(project, "authority_generation"),
          principalGeneration: integer(principal, "principal_generation"),
        }
      : {
          projectId: grant.projectId,
          projectAuthorityGeneration: integer(project, "authority_generation"),
          principalGeneration: integer(principal, "principal_generation"),
          projectSessionId: grant.projectSessionId,
          sessionGeneration: integer(row(this.database.prepare(`
            SELECT generation FROM project_sessions WHERE project_session_id=? AND project_id=?
          `).get(grant.projectSessionId, grant.projectId), "project session"), "generation"),
        };
    try {
      assertOperatorCapabilityAuthority(grant, authorityBinding);
    } catch (error: unknown) {
      throw new ProjectFabricCoreError("STALE_GENERATION", error instanceof Error ? error.message : String(error));
    }
    const existing = this.database.prepare(`
      SELECT token_hash FROM operator_capabilities WHERE capability_id=?
    `).get(grant.capabilityId);
    const tokenHash = sha256(token);
    if (isRow(existing)) {
      if (text(existing, "token_hash") !== tokenHash) {
        throw new ProjectFabricCoreError("CONFLICT", "capability ID was reused with another token");
      }
      return;
    }
    const takeover = grant.kind === "takeover" ? grant.takeoverBinding : undefined;
    this.database.prepare(`
      INSERT INTO operator_capabilities(
        capability_id, token_hash, operator_id, project_id, project_session_id,
        project_authority_generation, session_generation, principal_generation,
        kind, operations_json, issued_at, expires_at, revoked_at, handoff_digest,
        old_chair_generation, expected_run_id, expected_run_revision,
        expected_session_revision, cas_target_revision
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?)
    `).run(
      grant.capabilityId,
      tokenHash,
      grant.operatorId,
      grant.projectId,
      grant.kind === "project-launch" ? null : grant.projectSessionId,
      grant.projectAuthorityGeneration,
      grant.kind === "project-launch" ? null : grant.sessionGeneration,
      grant.principalGeneration,
      grant.kind,
      canonicalJson(grant.actions),
      timestampToMillis(grant.issuedAt),
      timestampToMillis(grant.expiresAt),
      takeover?.handoffDigest ?? null,
      takeover?.oldChairGeneration ?? null,
      takeover?.expectedRunId ?? null,
      takeover?.expectedRunRevision ?? null,
      takeover?.expectedSessionRevision ?? null,
      takeover?.targetRevision ?? null,
    );
  }

  revokeCapability(capabilityId: string): void {
    const changed = this.database.prepare(`
      UPDATE operator_capabilities SET revoked_at=? WHERE capability_id=? AND revoked_at IS NULL
    `).run(this.#clock(), capabilityId);
    if (changed.changes !== 1) throw new ProjectFabricCoreError("NOT_FOUND", "capability was not active");
  }

  authenticateCredential(token: string): AuthenticatedOperatorCredential {
    if (token.length === 0) throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "capability token is empty");
    return this.authenticateCredentialHash(sha256(token));
  }

  authenticateCredentialHash(tokenHash: string): AuthenticatedOperatorCredential {
    if (!/^[0-9a-f]{64}$/u.test(tokenHash)) {
      throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "capability token hash is invalid");
    }
    const capability = this.database.prepare(`
      SELECT * FROM operator_capabilities WHERE token_hash=?
    `).get(tokenHash);
    if (!isRow(capability)) throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "capability credential is invalid");
    if (capability.revoked_at !== null) throw new ProjectFabricCoreError("CAPABILITY_REVOKED", "capability is revoked");
    if (integer(capability, "expires_at") <= this.#clock()) {
      throw new ProjectFabricCoreError("CAPABILITY_EXPIRED", "capability is expired");
    }
    const projectId = text(capability, "project_id");
    const operatorId = text(capability, "operator_id");
    const principal = row(this.database.prepare(`
      SELECT project_id, project_authority_generation, principal_generation, state
        FROM operator_principals WHERE operator_id=?
    `).get(operatorId), "operator principal");
    if (text(principal, "state") !== "active") {
      throw new ProjectFabricCoreError("CAPABILITY_REVOKED", "operator principal is revoked");
    }
    if (text(principal, "project_id") !== projectId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "operator principal changed project");
    }
    const projectGeneration = integer(row(this.database.prepare(`
      SELECT authority_generation FROM projects WHERE project_id=?
    `).get(projectId), "project"), "authority_generation");
    if (
      integer(capability, "project_authority_generation") !== projectGeneration ||
      integer(principal, "project_authority_generation") !== projectGeneration
    ) {
      throw new ProjectFabricCoreError("STALE_GENERATION", "project authority generation is stale");
    }
    const principalGeneration = integer(principal, "principal_generation");
    if (integer(capability, "principal_generation") !== principalGeneration) {
      throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "operator principal generation is stale");
    }
    const kind = text(capability, "kind");
    if (kind !== "project-launch" && kind !== "session" && kind !== "takeover") {
      throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "operator capability kind is invalid");
    }
    const projectSessionId = nullableText(capability, "project_session_id");
    const sessionGeneration = capability.session_generation === null
      ? null
      : integer(capability, "session_generation");
    if (kind === "project-launch") {
      if (projectSessionId !== null || sessionGeneration !== null) {
        throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "project capability has a session binding");
      }
    } else {
      if (projectSessionId === null || sessionGeneration === null) {
        throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "session capability has no session binding");
      }
      const currentSessionGeneration = integer(row(this.database.prepare(`
        SELECT generation FROM project_sessions WHERE project_session_id=? AND project_id=?
      `).get(projectSessionId, projectId), "project session"), "generation");
      if (sessionGeneration !== currentSessionGeneration) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "project-session generation is stale");
      }
    }
    const parsedActions: unknown = JSON.parse(text(capability, "operations_json"));
    if (
      !Array.isArray(parsedActions) ||
      parsedActions.length === 0 ||
      !parsedActions.every((action): action is OperatorAction => (
        typeof action === "string" && OPERATOR_ACTIONS.includes(action as OperatorAction)
      ))
    ) {
      throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "operator capability actions are invalid");
    }
    return {
      context: {
        operatorId: operatorId as never,
        projectId: projectId as never,
        projectAuthorityGeneration: projectGeneration,
        principalGeneration,
      },
      capabilityId: text(capability, "capability_id"),
      kind,
      ...(projectSessionId === null ? {} : { projectSessionId }),
      ...(sessionGeneration === null ? {} : { sessionGeneration }),
      actions: [...parsedActions],
    };
  }

  attach(
    context: AuthenticatedOperatorContext,
    request: OperatorAttachRequest,
    daemonInstanceGeneration: number,
  ): OperatorAttachment {
    const clientId = this.#clientId(request.command);
    const session = request.projectSessionId === undefined
      ? undefined
      : row(this.database.prepare(`
          SELECT generation, revision FROM project_sessions WHERE project_session_id=? AND project_id=?
        `).get(request.projectSessionId, request.projectId), "project session");
    return this.executeCommand(
      context,
      request.command,
      {
        projectId: request.projectId,
        ...(request.projectSessionId === undefined ? {} : {
          projectSessionId: request.projectSessionId,
          sessionGeneration: integer(session as Row, "generation"),
        }),
        requiredAction: "read",
        commandPayload: {
          projectId: request.projectId,
          ...(request.projectSessionId === undefined ? {} : { projectSessionId: request.projectSessionId }),
          ...(request.expectedAttachmentGeneration === undefined
            ? {}
            : { expectedAttachmentGeneration: request.expectedAttachmentGeneration }),
          requestedExpiresAt: request.requestedExpiresAt,
          daemonInstanceGeneration,
        },
      },
      () => request.projectSessionId === undefined
        ? this.#projectRevision(request.projectId)
        : {
            revision: integer(session as Row, "revision"),
            value: { projectSessionId: request.projectSessionId, revision: integer(session as Row, "revision") },
          },
      () => {
        const requestedExpiry = timestampToMillis(request.requestedExpiresAt);
        const capability = row(this.database.prepare(`
          SELECT expires_at FROM operator_capabilities WHERE capability_id=?
        `).get(request.command.credential.capabilityId), "operator capability");
        if (requestedExpiry > integer(capability, "expires_at")) {
          throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "attachment cannot outlive its capability");
        }
        const existing = this.database.prepare(`
          SELECT lease_generation, state, project_id, project_session_id
            FROM operator_client_attachments WHERE attachment_id=?
        `).get(clientId);
        const now = this.#clock();
        if (isRow(existing)) {
          if (
            text(existing, "project_id") !== request.projectId ||
            text(existing, "state") !== "active" ||
            integer(existing, "lease_generation") !== request.expectedAttachmentGeneration
          ) {
            throw new ProjectFabricCoreError("STALE_GENERATION", "operator attachment generation changed");
          }
          const priorSession = nullableText(existing, "project_session_id");
          if (priorSession !== null && priorSession !== request.projectSessionId) {
            throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "attachment cannot widen or switch sessions");
          }
          this.database.prepare(`
            UPDATE operator_client_attachments
               SET project_session_id=?, session_generation=?, daemon_instance_generation=?,
                   lease_generation=lease_generation+1, expires_at=?, revision=revision+1,
                   updated_at=?
             WHERE attachment_id=?
          `).run(
            request.projectSessionId ?? null,
            session === undefined ? null : integer(session, "generation"),
            daemonInstanceGeneration,
            requestedExpiry,
            now,
            clientId,
          );
        } else {
          if (request.expectedAttachmentGeneration !== undefined) {
            throw new ProjectFabricCoreError("STALE_GENERATION", "operator attachment does not exist");
          }
          this.database.prepare(`
            INSERT INTO operator_client_attachments(
              attachment_id, operator_id, project_id, project_authority_generation,
              project_session_id, session_generation, daemon_instance_generation,
              lease_generation, state, expires_at, revision, created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, 1, 'active', ?, 1, ?, ?)
          `).run(
            clientId,
            context.operatorId,
            request.projectId,
            context.projectAuthorityGeneration,
            request.projectSessionId ?? null,
            session === undefined ? null : integer(session, "generation"),
            daemonInstanceGeneration,
            requestedExpiry,
            now,
            now,
          );
        }
        return this.#attachment(clientId);
      },
    );
  }

  heartbeat(
    context: AuthenticatedOperatorContext,
    request: OperatorHeartbeatRequest,
  ): OperatorAttachment {
    const clientId = this.#clientId(request.command);
    const current = this.#attachmentRow(clientId);
    const projectSessionId = nullableText(current, "project_session_id");
    return this.executeCommand(
      context,
      request.command,
      {
        projectId: text(current, "project_id"),
        ...(projectSessionId === null ? {} : {
          projectSessionId,
          sessionGeneration: integer(current, "session_generation"),
        }),
        requiredAction: "read",
        commandPayload: {
          attachmentGeneration: request.attachmentGeneration,
          extendUntil: request.extendUntil,
        },
      },
      () => ({ revision: integer(this.#attachmentRow(clientId), "revision"), value: this.#attachment(clientId) }),
      () => {
        if (integer(current, "lease_generation") !== request.attachmentGeneration) {
          throw new ProjectFabricCoreError("STALE_GENERATION", "operator attachment generation changed");
        }
        const expiry = timestampToMillis(request.extendUntil);
        const capability = row(this.database.prepare(`
          SELECT expires_at FROM operator_capabilities WHERE capability_id=?
        `).get(request.command.credential.capabilityId), "operator capability");
        if (expiry > integer(capability, "expires_at")) {
          throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "attachment cannot outlive its capability");
        }
        this.database.prepare(`
          UPDATE operator_client_attachments
             SET lease_generation=lease_generation+1, expires_at=?, revision=revision+1, updated_at=?
           WHERE attachment_id=? AND state='active'
        `).run(expiry, this.#clock(), clientId);
        return this.#attachment(clientId);
      },
    );
  }

  detach(
    context: AuthenticatedOperatorContext,
    request: OperatorDetachRequest,
  ): { detached: true; revision: number } {
    const clientId = this.#clientId(request.command);
    const current = this.#attachmentRow(clientId);
    const projectSessionId = nullableText(current, "project_session_id");
    return this.executeCommand(
      context,
      request.command,
      {
        projectId: text(current, "project_id"),
        ...(projectSessionId === null ? {} : {
          projectSessionId,
          sessionGeneration: integer(current, "session_generation"),
        }),
        requiredAction: "read",
        commandPayload: { attachmentGeneration: request.attachmentGeneration },
      },
      () => ({ revision: integer(this.#attachmentRow(clientId), "revision"), value: this.#attachment(clientId) }),
      () => {
        if (integer(current, "lease_generation") !== request.attachmentGeneration) {
          throw new ProjectFabricCoreError("STALE_GENERATION", "operator attachment generation changed");
        }
        const revision = integer(current, "revision") + 1;
        this.database.prepare(`
          UPDATE operator_client_attachments SET state='detached', revision=?, updated_at=?
           WHERE attachment_id=? AND state='active'
        `).run(revision, this.#clock(), clientId);
        return { detached: true as const, revision };
      },
    );
  }

  recordInputAttestation(
    context: AuthenticatedIntegrationContext,
    request: IntegrationInputAttestationRequest,
  ): OperatorInputAttestation {
    const action = this.database.transaction((): OperatorInputAttestation => {
      if (
        context.integrationId !== request.context.integrationId ||
        context.integrationId !== request.attestation.integrationId ||
        context.principalGeneration !== request.context.expectedIntegrationGeneration ||
        context.principalGeneration !== request.attestation.integrationGeneration
      ) {
        throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "integration identity or generation changed");
      }
      if (context.projectId !== request.attestation.projectId) {
        throw new ProjectFabricCoreError("WRONG_PROJECT", "attestation is bound to another project");
      }
      if (
        request.context.eventId !== request.attestation.providerEvent.inputEventId ||
        request.context.eventDigest !== request.attestation.providerEvent.eventDigest ||
        request.attestation.providerEvent.classification !== "direct-human"
      ) {
        throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "attestation does not match the immutable direct-human event");
      }
      const existing = this.database.prepare(`
        SELECT provider_event_json, exact_utterance, artifact_digests_json,
               expected_gate_revision, interpreted_decision
          FROM operator_input_attestations
         WHERE attestation_id=? OR (project_session_id=? AND provider_message_id=?)
      `).get(
        request.attestation.attestationId,
        request.attestation.projectSessionId,
        request.attestation.providerEvent.providerMessageId,
      );
      if (isRow(existing)) {
        const reconstructed = canonicalJson({
          providerEvent: JSON.parse(text(existing, "provider_event_json")),
          humanUtterance: text(existing, "exact_utterance"),
          expectedGateRevision: integer(existing, "expected_gate_revision"),
          artifactDigests: JSON.parse(text(existing, "artifact_digests_json")),
          interpretedDecision: text(existing, "interpreted_decision"),
        });
        const incoming = canonicalJson({
          providerEvent: request.attestation.providerEvent,
          humanUtterance: request.attestation.humanUtterance,
          expectedGateRevision: request.attestation.gateBinding.expectedGateRevision,
          artifactDigests: request.attestation.gateBinding.artifactDigests,
          interpretedDecision: request.attestation.gateBinding.interpretedDecision,
        });
        if (reconstructed !== incoming) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "attestation identity was reused with changed evidence");
        }
        return request.attestation;
      }
      const gate = row(this.database.prepare(`
        SELECT g.coordination_run_id, g.revision, g.expected_approver_ref,
               g.evidence_refs_json, g.release_binding_json, s.project_id
          FROM scoped_gates g
          JOIN project_sessions s ON s.project_session_id=g.project_session_id
         WHERE g.gate_id=? AND g.project_session_id=?
      `).get(request.attestation.gateBinding.gateId, request.attestation.projectSessionId), "scoped gate");
      if (text(gate, "project_id") !== context.projectId) {
        throw new ProjectFabricCoreError("WRONG_PROJECT", "gate is outside the integration project");
      }
      if (integer(gate, "revision") !== request.attestation.gateBinding.expectedGateRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "gate revision changed");
      }
      assertExactGateAttestationDigests(
        canonicalStoredGateAttestationDigests(
          text(gate, "evidence_refs_json"),
          nullableText(gate, "release_binding_json"),
        ),
        request.attestation.gateBinding.artifactDigests,
      );
      const principal = row(this.database.prepare(`
        SELECT project_id, state FROM operator_principals WHERE operator_id=?
      `).get(request.attestation.operatorId), "operator principal");
      if (text(principal, "project_id") !== context.projectId || text(principal, "state") !== "active") {
        throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "attested operator is not active for the project");
      }
      const expectedApprover = text(gate, "expected_approver_ref");
      if (
        expectedApprover !== request.attestation.operatorId &&
        expectedApprover !== "authenticated-operator" &&
        expectedApprover !== "authenticated-human-operator"
      ) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "attested operator is not the expected approver");
      }
      this.database.prepare(`
        INSERT INTO operator_input_attestations(
          attestation_id, integration_id, integration_generation, operator_id,
          project_id, project_session_id, coordination_run_id, gate_id,
          provider_message_id, exact_utterance, provider_event_json,
          expected_gate_revision, artifact_digests_json, interpreted_decision, recorded_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(
        request.attestation.attestationId,
        context.integrationId,
        context.principalGeneration,
        request.attestation.operatorId,
        context.projectId,
        request.attestation.projectSessionId,
        text(gate, "coordination_run_id"),
        request.attestation.gateBinding.gateId,
        request.attestation.providerEvent.providerMessageId,
        request.attestation.humanUtterance,
        canonicalJson(request.attestation.providerEvent),
        request.attestation.gateBinding.expectedGateRevision,
        canonicalJson(request.attestation.gateBinding.artifactDigests),
        request.attestation.gateBinding.interpretedDecision,
        timestampToMillis(request.attestation.recordedAt),
      );
      return request.attestation;
    });
    return action();
  }

  executeCommand<Result>(
    context: AuthenticatedOperatorContext,
    command: OperatorMutationContext,
    target: OperatorCommandTarget,
    load: () => { revision: number; value: JsonValue },
    mutate: () => Result,
    afterJournal?: (result: Result) => void,
  ): Result {
    const execute = this.database.transaction((): Result => {
      const payload = {
        capabilityId: command.credential.capabilityId,
        commandId: command.commandId,
        expectedRevision: command.expectedRevision,
        actor: command.actor,
        provenance: command.provenance,
        evidenceRefs: command.evidenceRefs,
        target,
      };
      const payloadHash = sha256(canonicalJson(payload));
      const existing = this.database.prepare(`
        SELECT payload_hash, result_json FROM operator_commands
         WHERE operator_id=? AND command_id=?
      `).get(context.operatorId, command.commandId);
      if (isRow(existing)) {
        if (text(existing, "payload_hash") !== payloadHash) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "command ID was reused with changed input");
        }
        this.#authenticate(context, command, target, true);
        const result = JSON.parse(text(existing, "result_json")) as Result;
        afterJournal?.(result);
        return result;
      }

      const capability = this.#authenticate(context, command, target, false);
      const before = load();
      if (before.revision !== command.expectedRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "operator command revision changed", {
          expected: command.expectedRevision,
          actual: before.revision,
          current: before.value,
        });
      }
      const result = mutate();
      const after = load();
      this.database.prepare(`
        INSERT INTO operator_commands(
          operator_id, command_id, capability_id, project_id, project_session_id,
          operation, expected_revision, payload_hash, provenance_json, before_json,
          after_json, evidence_json, result_json, status, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'committed', ?)
      `).run(
        context.operatorId,
        command.commandId,
        text(capability, "capability_id"),
        target.projectId,
        target.projectSessionId ?? null,
        target.requiredAction,
        command.expectedRevision,
        payloadHash,
        canonicalJson(command.provenance),
        canonicalJson(before.value),
        canonicalJson(after.value),
        canonicalJson(command.evidenceRefs),
        canonicalJson(result),
        this.#clock(),
      );
      afterJournal?.(result);
      return result;
    });
    return execute();
  }

  authenticateCommand(
    context: AuthenticatedOperatorContext,
    command: OperatorMutationContext,
    target: OperatorCommandTarget,
  ): void {
    this.#authenticate(context, command, target, false);
  }

  #authenticate(
    context: AuthenticatedOperatorContext,
    command: OperatorMutationContext,
    target: OperatorCommandTarget,
    replay: boolean,
  ): CapabilityRow {
    if (command.actor !== context.operatorId || context.projectId !== target.projectId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "operator connection does not own the command target");
    }
    const capabilityValue = this.database.prepare(`
      SELECT * FROM operator_capabilities WHERE capability_id=? AND token_hash=?
    `).get(command.credential.capabilityId, sha256(command.credential.token));
    if (!isRow(capabilityValue)) throw new ProjectFabricCoreError("AUTHENTICATION_FAILED", "capability credential is invalid");
    const capability = capabilityValue as CapabilityRow;
    if (capability.revoked_at !== null) throw new ProjectFabricCoreError("CAPABILITY_REVOKED", "capability is revoked");
    if (integer(capability, "expires_at") <= this.#clock()) {
      throw new ProjectFabricCoreError("CAPABILITY_EXPIRED", "capability is expired");
    }
    if (text(capability, "operator_id") !== context.operatorId || text(capability, "project_id") !== target.projectId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "capability is bound to another project or operator");
    }
    const principal = row(this.database.prepare(`
      SELECT project_id, project_authority_generation, principal_generation, state
        FROM operator_principals WHERE operator_id=?
    `).get(context.operatorId), "operator principal");
    if (text(principal, "state") !== "active") throw new ProjectFabricCoreError("CAPABILITY_REVOKED", "operator principal is revoked");
    const project = row(this.database.prepare("SELECT authority_generation FROM projects WHERE project_id=?").get(target.projectId), "project");
    const currentProjectGeneration = integer(project, "authority_generation");
    if (
      context.projectAuthorityGeneration !== currentProjectGeneration ||
      integer(principal, "project_authority_generation") !== currentProjectGeneration ||
      integer(capability, "project_authority_generation") !== currentProjectGeneration
    ) throw new ProjectFabricCoreError("STALE_GENERATION", "project authority generation is stale");
    if (
      context.principalGeneration !== integer(principal, "principal_generation") ||
      integer(capability, "principal_generation") !== context.principalGeneration
    ) throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "operator principal generation is stale");
    const capabilitySession = nullableText(capability, "project_session_id");
    if (target.projectSessionId !== undefined) {
      if (capabilitySession !== target.projectSessionId) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "capability is not bound to the target session");
      }
      const session = row(this.database.prepare(`
        SELECT generation FROM project_sessions WHERE project_session_id=? AND project_id=?
      `).get(target.projectSessionId, target.projectId), "project session");
      const generation = integer(session, "generation");
      if (
        capability.session_generation !== target.sessionGeneration ||
        (!replay && generation !== target.sessionGeneration)
      ) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "project-session generation is stale");
      }
    } else if (text(capability, "kind") !== "project-launch") {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "session capability cannot perform a project-only command");
    }
    const operations: unknown = JSON.parse(text(capability, "operations_json"));
    if (!Array.isArray(operations) || !operations.includes(target.requiredAction)) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", `capability lacks ${target.requiredAction}`);
    }
    return capability;
  }

  #projectRevision(projectId: string): { revision: number; value: { projectId: string; revision: number } } {
    const project = row(this.database.prepare("SELECT revision FROM projects WHERE project_id=?").get(projectId), "project");
    const revision = integer(project, "revision");
    return { revision, value: { projectId, revision } };
  }

  #clientId(command: OperatorMutationContext): string {
    if (command.provenance.kind !== "console-direct-input") {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator attachment requires direct Console provenance");
    }
    return command.provenance.clientId;
  }

  #attachmentRow(clientId: string): Row {
    return row(this.database.prepare(`
      SELECT * FROM operator_client_attachments WHERE attachment_id=?
    `).get(clientId), "operator attachment");
  }

  #attachment(clientId: string): OperatorAttachment {
    const stored = this.#attachmentRow(clientId);
    const session = nullableText(stored, "project_session_id");
    return {
      clientId,
      projectId: text(stored, "project_id") as never,
      projectAuthorityGeneration: integer(stored, "project_authority_generation"),
      projectSessionId: session as never,
      generation: integer(stored, "lease_generation"),
      expiresAt: new Date(integer(stored, "expires_at")).toISOString() as never,
    };
  }
}
