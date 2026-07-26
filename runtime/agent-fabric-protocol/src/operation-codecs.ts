import type { JsonSchema } from "./codec.js";
import {
  FABRIC_OPERATIONS,
  isFabricOperation,
  OPERATION_REGISTRY,
  type OperationPrincipalKind,
} from "./operations.js";
import type { OperationInputMap, OperationResultMap, ProtocolOperation } from "./rpc-contract.js";
import type { OperationResultPrincipalContext } from "./operation-codecs/common.js";
import { validateLifecycleResultForInput } from "./operation-codecs/lifecycle.js";
import { validateProviderActionResultForInput } from "./operation-codecs/provider-action.js";
import { validateRunPlanResultForInput } from "./operation-codecs/run-plan.js";
import {
  assertComposedRegistryExhaustive,
  OPERATION_CODECS,
} from "./operation-codecs/registry.js";

export type {
  ObjectWireShape,
  OperationCodecPair,
  OperationResultPrincipalContext,
  ProviderActionResultKind,
  WireShape,
} from "./operation-codecs/common.js";
export {
  OPERATION_CODECS,
  OPERATION_INPUT_SHAPES,
  OPERATION_RESULT_SHAPES,
} from "./operation-codecs/registry.js";

export function operationInputSchemaForPrincipal(
  operation: ProtocolOperation,
  principal: OperationPrincipalKind,
): JsonSchema {
  if (!OPERATION_REGISTRY[operation].principals.includes(principal)) {
    throw new TypeError(`${principal} principal cannot invoke ${operation}`);
  }
  const schema = OPERATION_CODECS[operation].input.schema;
  if (!(new Set<ProtocolOperation>([
    FABRIC_OPERATIONS.membershipBind,
    FABRIC_OPERATIONS.intakeRevise,
    FABRIC_OPERATIONS.scopedGateCreate,
  ])).has(operation)) return schema;
  const variants = schema.oneOf;
  if (!Array.isArray(variants)) throw new Error(`${operation} principal-bound input schema has no variants`);
  const expectedOrigin = principal === "agent" ? "chair" : "operator";
  const matched = variants.find((variant) => {
    if (typeof variant !== "object" || variant === null || Array.isArray(variant)) return false;
    const properties = Reflect.get(variant, "properties");
    if (typeof properties !== "object" || properties === null || Array.isArray(properties)) return false;
    const origin = Reflect.get(properties, "origin");
    return typeof origin === "object" && origin !== null && Reflect.get(origin, "const") === expectedOrigin;
  });
  if (matched === undefined || typeof matched !== "object" || matched === null || Array.isArray(matched)) {
    throw new Error(`${operation} has no ${principal} input schema`);
  }
  return matched as JsonSchema;
}

export function parseOperationInput<Operation extends ProtocolOperation>(
  operation: Operation,
  value: unknown,
): OperationInputMap[Operation] {
  if (!isFabricOperation(operation)) throw new TypeError(`unknown fabric operation: ${String(operation)}`);
  return OPERATION_CODECS[operation].input.parse(value, `${operation}.input`) as OperationInputMap[Operation];
}

type PrincipalBoundOperation =
  | typeof FABRIC_OPERATIONS.membershipBind
  | typeof FABRIC_OPERATIONS.intakeRevise
  | typeof FABRIC_OPERATIONS.scopedGateCreate;

export type OperationInputForPrincipal<
  Operation extends ProtocolOperation,
  Principal extends OperationPrincipalKind,
> = Operation extends PrincipalBoundOperation
  ? Extract<OperationInputMap[Operation], { origin: Principal extends "agent" ? "chair" : "operator" }>
  : OperationInputMap[Operation];

export function parseOperationInputForPrincipal<
  Operation extends ProtocolOperation,
  Principal extends OperationPrincipalKind,
>(
  operation: Operation,
  principal: Principal,
  value: unknown,
): OperationInputForPrincipal<Operation, Principal> {
  if (!OPERATION_REGISTRY[operation].principals.includes(principal)) {
    throw new TypeError(`${principal} principal cannot invoke ${operation}`);
  }
  const parsed = parseOperationInput(operation, value);
  if (([
    FABRIC_OPERATIONS.membershipBind,
    FABRIC_OPERATIONS.intakeRevise,
    FABRIC_OPERATIONS.scopedGateCreate,
  ] as readonly ProtocolOperation[]).includes(operation)) {
    const expectedOrigin = principal === "agent" ? "chair" : "operator";
    if (typeof parsed !== "object" || parsed === null || Reflect.get(parsed, "origin") !== expectedOrigin) {
      throw new TypeError(`${principal} principal cannot submit an ${expectedOrigin === "chair" ? "operator" : "chair"} command`);
    }
  }
  return parsed as OperationInputForPrincipal<Operation, Principal>;
}

export function parseOperationResult<Operation extends ProtocolOperation>(
  operation: Operation,
  value: unknown,
): OperationResultMap[Operation] {
  if (!isFabricOperation(operation)) throw new TypeError(`unknown fabric operation: ${String(operation)}`);
  return OPERATION_CODECS[operation].result.parse(value, `${operation}.result`) as OperationResultMap[Operation];
}

export function parseOperationResultForInput<Operation extends ProtocolOperation>(
  operation: Operation,
  input: OperationInputMap[Operation],
  value: unknown,
  principal?: OperationResultPrincipalContext,
): OperationResultMap[Operation] {
  const result = parseOperationResult(operation, value);
  validateProviderActionResultForInput(operation, input, result);
  validateLifecycleResultForInput(operation, input, result, principal);
  validateRunPlanResultForInput(operation, input, result, principal);
  validateRunProjectionResultForInput(operation, input, result);
  return result;
}

function validateRunProjectionResultForInput<Operation extends ProtocolOperation>(
  operation: Operation,
  input: OperationInputMap[Operation],
  result: OperationResultMap[Operation],
): void {
  if (operation !== FABRIC_OPERATIONS.projectionRunPage) return;
  const request = input as unknown as Record<string, unknown>;
  const response = result as unknown as Record<string, unknown>;
  const requestTarget = request.target as Record<string, unknown>;
  const responseTarget = response.target as Record<string, unknown>;
  const sameTarget = requestTarget.kind === responseTarget.kind &&
    requestTarget.coordinationRunId === responseTarget.coordinationRunId &&
    (
      requestTarget.kind !== "delivery-workstream" ||
      (
        requestTarget.deliveryRunId === responseTarget.deliveryRunId &&
        requestTarget.workstreamId === responseTarget.workstreamId
      )
    );
  if (
    response.projectSessionId !== request.projectSessionId ||
    response.section !== request.section ||
    !sameTarget
  ) {
    throw new TypeError("run projection result addressing does not match request");
  }
  if (response.status === "page" && response.snapshotRevision !== request.snapshotRevision) {
    throw new TypeError("run projection page snapshot revision does not match request");
  }
}

export function assertCodecRegistryExhaustive(): void {
  assertComposedRegistryExhaustive(OPERATION_CODECS);
}
