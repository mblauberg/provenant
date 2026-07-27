import { Fabric, type FabricRuntimeOpenOptions } from "./core/fabric.js";

export { Fabric, FabricClient } from "./core/fabric.js";
export type { FabricOperatorActionPorts, FabricRuntimeOpenOptions } from "./core/fabric.js";
export type {
  LifecycleIntegrityReceiptAuthorityPort,
  LifecycleAuthenticatedReceipt,
  LifecycleAuthenticatedScopeCheckpoint,
} from "./lifecycle/receipt-authority.js";
export {
  LocalLifecycleReceiptAuthority,
  openLocalLifecycleReceiptAuthority,
} from "./lifecycle/local-receipt-authority.js";
export type { LocalLifecycleReceiptAuthorityOptions } from "./lifecycle/local-receipt-authority.js";
export type {
  ProviderActionDispatchRequest,
  TeamCreateInput,
  TeamResult,
} from "./core/contracts.js";
export {
  connectFabricDaemon,
  FabricDaemonClient,
  FabricRemoteError,
  startFabricDaemon,
} from "./daemon/client.js";
export type {
  DaemonStartOptions,
  FabricDaemonHandle,
} from "./daemon/client.js";
export type { HerdrDaemonProcessConfiguration } from "./daemon/herdr-composition.js";
export {
  LocalOperatorConsoleUnavailableError,
  daemonStartUnavailableReason,
  openLocalOperatorConsoleSession,
} from "./operator/local-console-session.js";
export { createOptionalGitHubHostedChecksAdapter } from "./operator/github-hosted-checks.js";
export type {
  GitHubCliHostedChecksOptions,
  GitHubHostedChecksProcessPort,
  OptionalGitHubHostedChecksConfiguration,
} from "./operator/github-hosted-checks.js";
export type {
  GitHostedChecksBinding,
  GitHostedChecksPort,
} from "./operator/git-repository-read.js";
export { FixedGitMutationPort } from "./operator/fixed-git-mutation-port.js";
export type {
  GitMutationDispatchContext,
  GitMutationInspection,
  GitMutationPointOfUse,
  GitMutationPort,
} from "./operator/fixed-git-mutation-port.js";
export {
  TypedGitService,
  deriveGitEffectBindingDigest,
  deriveGitGrantDigest,
  deriveGitResultRecipeDigest,
  derivePreauthorisedGitOperationId,
} from "./operator/typed-git-service.js";
export {
  TrustedGitRegistry,
  deriveTrustedGitExecutionProfileDigest,
  deriveTrustedGitRemoteTargetDigest,
  deriveTrustedRunGitAllowlistDigest,
} from "./operator/trusted-git-registry.js";
export type {
  TrustedGitConfiguration,
  TrustedGitExecutionProfile,
  TrustedGitRemoteRegistration,
  TrustedRunGitAllowlist,
} from "./operator/trusted-git-registry.js";
export type {
  GitConflictInspectorPort,
  TypedGitAdministrativeIntent,
  TypedGitAdministrativeRequest,
  TypedGitEffectRequest,
  TypedGitServiceOptions,
} from "./operator/typed-git-service.js";
export type {
  ExternalEffectEvidencePort,
  RegisteredEffectPort,
} from "./operator/external-effect-service.js";
export { HerdrFabricPorts } from "./integrations/herdr-fabric-ports.js";
export type {
  DirectSteerIntent,
  FabricSteerReference,
  FabricSteerReferenceValidation,
  HerdrActionEvidence,
  HerdrActionRecord,
  HerdrAppliedOperation,
  HerdrEffectReceipt,
  HerdrFabricPortsOptions,
  HerdrRecoverySummary,
} from "./integrations/herdr-fabric-ports.js";
export {
  HerdrDaemonIntegration,
} from "./integrations/herdr-daemon-integration.js";
export type {
  HerdrDaemonActionRequest,
  HerdrDaemonActionResult,
  HerdrDaemonIntegrationConfiguration,
  HerdrDaemonRuntime,
  HerdrDaemonRuntimeFactoryInput,
  HerdrPresencePassResult,
  HerdrDirectSteerRequest,
} from "./integrations/herdr-daemon-integration.js";
export type {
  LocalOperatorConsoleSession,
  LocalOperatorConsoleSessionOptions,
  LocalOperatorConsoleUnavailableReason,
  LocalOperatorConsoleUnavailableCode,
} from "./operator/local-console-session.js";
export { FabricError } from "./errors.js";
export {
  AUTHORITY_ACTION_VOCABULARY,
  FABRIC_OPERATIONS,
  expandAuthorityActions,
} from "./domain/operations.js";
export type { FabricOperation } from "./domain/operations.js";
export { runAdapterConformance } from "./adapters/conformance.js";
export { verifyAdapterCompatibility, type WrapperProvenance } from "./adapters/compatibility.js";
export { startOptionalAdapterLeg } from "./adapters/optional-leg.js";
export { assessAdapterModelPolicy, resolveProviderAdapterSelection, validateAdapterModelSelection } from "./adapters/model-selection.js";
export { resolveExecutionProfile, ExecutionProfileError } from "./profiles/execution.js";
export { createVisibilityCoordinator, VisibilityCoordinator } from "./visibility/coordinator.js";
export {
  resolveModelRouteReceipt,
  selectPreferredModelRouteReceipt,
} from "./routing/model-route.js";
export { FabricReceiptError, verifyFabricReceiptLink } from "./exports/receipt.js";
export { resolveFabricPaths } from "./cli/paths.js";
export { MESSAGE_POLICY } from "./domain/types.js";
export { redactLaunchProviderInput } from "./project-session/provider-input-safety.js";
export type * from "./domain/types.js";

export async function openFabric(options: FabricRuntimeOpenOptions): Promise<Fabric> {
  return new Fabric(options);
}
