import {
  closeSync,
  constants,
  openSync,
  readSync,
  writeSync,
} from "node:fs";

declare const verifiedInteractiveApprovalBrand: unique symbol;

export type DatabaseArchiveAndFreshApproval =
  | Readonly<{
    kind: "interactive-confirmation";
    status: "verified";
    [verifiedInteractiveApprovalBrand]: true;
  }>
  | Readonly<{
    kind: "unattended-approval";
    status: "asserted";
    principal: string;
  }>;

export type DatabaseArchiveAndFreshApprovalReceipt =
  | Readonly<{
    kind: "interactive-confirmation";
    status: "verified";
  }>
  | Extract<DatabaseArchiveAndFreshApproval, { kind: "unattended-approval" }>;

const issuedInteractiveApprovals = new WeakSet<object>();

function issueVerifiedInteractiveApproval(): Extract<
  DatabaseArchiveAndFreshApproval,
  { kind: "interactive-confirmation" }
> {
  const approval = Object.freeze({
    kind: "interactive-confirmation",
    status: "verified",
  }) as Extract<DatabaseArchiveAndFreshApproval, { kind: "interactive-confirmation" }>;
  issuedInteractiveApprovals.add(approval);
  return approval;
}

export function readVerifiedInteractiveApproval(
  prompt: string,
): Readonly<
  | { status: "verified"; approval: DatabaseArchiveAndFreshApproval }
  | { status: "unavailable" }
  | { status: "mismatch" }
> {
  let handle: number;
  try {
    handle = openSync("/dev/tty", constants.O_RDWR | constants.O_NOFOLLOW);
  } catch (error: unknown) {
    if (
      error instanceof Error &&
      "code" in error &&
      ["EACCES", "ENODEV", "ENOENT", "ENXIO", "EIO", "EPERM"].includes(String(error.code))
    ) {
      return { status: "unavailable" };
    }
    throw error;
  }
  try {
    writeSync(handle, prompt);
    const bytes: number[] = [];
    const byte = Buffer.allocUnsafe(1);
    while (bytes.length <= 256) {
      const count = readSync(handle, byte, 0, 1, null);
      if (count === 0) return { status: "unavailable" };
      if (byte[0] === 0x0a) break;
      if (byte[0] !== 0x0d) bytes.push(byte[0] ?? 0);
    }
    return Buffer.from(bytes).toString("utf8") === "ARCHIVE-AND-FRESH"
      ? { status: "verified", approval: issueVerifiedInteractiveApproval() }
      : { status: "mismatch" };
  } catch (error: unknown) {
    if (error instanceof Error && "code" in error && error.code === "EIO") {
      return { status: "unavailable" };
    }
    throw error;
  } finally {
    closeSync(handle);
  }
}

export function consumeApproval(
  approval: DatabaseArchiveAndFreshApproval,
): DatabaseArchiveAndFreshApprovalReceipt {
  if (approval.kind === "interactive-confirmation") {
    if (!issuedInteractiveApprovals.delete(approval)) {
      throw new Error(
        "interactive cutover approval was not issued by controlling-terminal verification",
      );
    }
    return {
      kind: "interactive-confirmation",
      status: "verified",
    };
  }
  const status = approval.status;
  const principal = approval.principal;
  if (
    status !== "asserted" ||
    !/^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,127}$/u.test(principal)
  ) {
    throw new Error(
      "unattended cutover approval requires an asserted named principal using 1-128 identifier characters",
    );
  }
  return Object.freeze({
    kind: "unattended-approval",
    status: "asserted",
    principal,
  });
}
