import {
  archiveAndFreshDatabase,
  type DatabaseArchiveAndFreshOptions,
  type DatabaseArchiveAndFreshResult,
} from "./database-archive-and-fresh.js";
import { readVerifiedInteractiveApproval } from "./database-cutover-approval.js";

function cliOption(arguments_: string[], name: string): string | undefined {
  const indexes = arguments_.flatMap((value, index) => value === name ? [index] : []);
  if (indexes.length > 1) throw new Error(`${name} may be provided only once`);
  const index = indexes[0];
  if (index === undefined) return undefined;
  const value = arguments_[index + 1];
  if (value === undefined || value.startsWith("--")) throw new Error(`${name} requires a value`);
  return value;
}

export function cutoverFailure(
  databasePath: string,
  error: unknown,
): DatabaseArchiveAndFreshResult {
  const message = error instanceof Error ? error.message : String(error);
  let code: Extract<DatabaseArchiveAndFreshResult, { status: "failed" }>["code"] =
    "ARCHIVE_WRITE_FAILED";
  if (error instanceof Error && "code" in error && error.code === "EEXIST") {
    code = "ARCHIVE_DESTINATION_EXISTS";
  } else if (
    /option|requires|absolute --archive|not issued by controlling-terminal verification/iu
      .test(message)
  ) {
    code = "INVALID_ARGUMENT";
  } else if (/changed|disappeared/iu.test(message)) {
    code = "SOURCE_SET_CHANGED";
  } else if (
    /symbolic link|single-link regular file|not a regular non-symlink|archive destination must not/iu
      .test(message)
  ) {
    code = "SOURCE_SET_INVALID";
  }
  return {
    schemaVersion: 1,
    kind: "agent-fabric-database-archive-and-fresh",
    status: "failed",
    code,
    databasePath,
    message,
    sourcePreserved: true,
  };
}

export function runDatabaseArchiveAndFreshCli(
  arguments_: string[],
  defaultDatabasePath: string,
): Readonly<{ result: DatabaseArchiveAndFreshResult; exitCode: 0 | 1 | 4 }> {
  let databasePath = defaultDatabasePath;
  try {
    const allowed = new Set([
      "--database",
      "--archive",
      "--confirm-source-set",
      "--unattended-approval-asserted-by",
    ]);
    for (let index = 0; index < arguments_.length; index += 2) {
      const name = arguments_[index];
      if (name === undefined || !allowed.has(name)) {
        throw new Error(`unknown database archive-and-fresh option: ${name ?? ""}`);
      }
      if (arguments_[index + 1] === undefined) throw new Error(`${name} requires a value`);
    }
    databasePath = cliOption(arguments_, "--database") ?? defaultDatabasePath;
    const archiveDirectory = cliOption(arguments_, "--archive");
    if (archiveDirectory === undefined) {
      throw new Error("database archive-and-fresh requires --archive ABSOLUTE_NEW_DIRECTORY");
    }
    const confirmSourceSetSha256 = cliOption(arguments_, "--confirm-source-set");
    const unattendedPrincipal = cliOption(arguments_, "--unattended-approval-asserted-by");
    if (unattendedPrincipal !== undefined && confirmSourceSetSha256 === undefined) {
      throw new Error(
        "--unattended-approval-asserted-by requires --confirm-source-set",
      );
    }
    const options: DatabaseArchiveAndFreshOptions = {
      databasePath,
      archiveDirectory,
      ...(confirmSourceSetSha256 === undefined
        ? {}
        : { confirmSourceSetSha256 }),
      ...(unattendedPrincipal === undefined
        ? {}
        : {
          approval: {
            kind: "unattended-approval",
            status: "asserted",
            principal: unattendedPrincipal,
          },
        }),
    };
    let result = archiveAndFreshDatabase(options);
    if (
      result.status === "approval-required" &&
      result.code === "CUTOVER_APPROVAL_REQUIRED" &&
      unattendedPrincipal === undefined
    ) {
      const interactiveApproval = readVerifiedInteractiveApproval(
        "Type ARCHIVE-AND-FRESH to confirm live coordination-state displacement: ",
      );
      if (interactiveApproval.status === "unavailable") {
        result = {
          ...result,
          code: "CUTOVER_INTERACTIVE_CONFIRMATION_REQUIRED",
          message:
            "verified interactive confirmation is required, but the confirmation could not be read from the controlling terminal",
        };
      } else if (interactiveApproval.status === "mismatch") {
        result = {
          ...result,
          code: "CUTOVER_INTERACTIVE_CONFIRMATION_MISMATCH",
          message: "interactive confirmation did not match ARCHIVE-AND-FRESH",
        };
      } else {
        result = archiveAndFreshDatabase({
          ...options,
          approval: interactiveApproval.approval,
        });
      }
    }
    return {
      result,
      exitCode: result.status === "archive-complete-fresh-init-failed" ||
          result.status === "archive-complete-cutover-failed" ||
          result.status === "recovery-required"
        ? 4
        : result.status === "failed" ||
            result.status === "conflict" ||
            result.status === "approval-required"
          ? 1
          : 0,
    };
  } catch (error: unknown) {
    return { result: cutoverFailure(databasePath, error), exitCode: 1 };
  }
}
