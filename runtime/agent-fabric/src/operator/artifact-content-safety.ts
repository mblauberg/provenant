import { createHash } from "node:crypto";

import type {
  ArtifactContentTransformation,
  ArtifactLineFragment,
} from "@local/agent-fabric-protocol";

const REDACTION = "█";
const FABRIC_BEARER = /\b(?:afb_|afc_|afop_)[A-Za-z0-9_-]{8,}\b/gu;
const PRIVATE_KEY = /-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----/gu;
const AUTHORIZATION = /^(\s*(?:authorization|proxy-authorization)\s*:\s*)[^\r\n]+$/gimu;
const URL_USERINFO = /\b([a-z][a-z0-9+.-]*:\/\/)[^\s/@:]+:[^\s/@]+@/giu;
const PROVIDER_TOKEN = /\b(?:AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{20,}|gh[pousr]_[A-Za-z0-9]{20,}|sk-[A-Za-z0-9_-]{16,}|xox[baprs]-[A-Za-z0-9-]{10,})\b/gu;
const ASSIGNMENT_SECRET = /(\b(?:password|passphrase|token|secret|credential|private[_ -]?key)\b\s*(?:=|:)\s*)(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|[^\s,;]+)/giu;

export type InertArtifactTextResult =
  | { safe: false }
  | {
      safe: true;
      content: string;
      transformation: ArtifactContentTransformation;
      terminalNeutralised: true;
      capabilityValuesRedacted: true;
      credentialValuesRedacted: true;
    };

export type RedactedArtifactTextResult =
  | { safe: false }
  | {
      safe: true;
      content: string;
      transformation: ArtifactContentTransformation;
      capabilityValuesRedacted: true;
      credentialValuesRedacted: true;
    };

function escapeRegex(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
}

function neutraliseTerminal(value: string): string {
  return value
    .replace(/\r\n/gu, "\n")
    .replace(/\r/gu, "\n")
    .replace(/\u001b\][^\u0007]*(?:\u0007|\u001b\\)/gu, " ")
    .replace(/\u001b[P_X^][\s\S]*?\u001b\\/gu, " ")
    .replace(/\u001b\[[0-?]*[ -/]*[@-~]/gu, " ")
    .replace(/\u001b(?:.|$)/gu, " ")
    .replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/gu, " ")
    .replace(/[\u202a-\u202e\u2066-\u2069]/gu, "")
    .replace(/[\u200b-\u200f\u2028\u2029\u2060-\u206f\ufeff]/gu, "")
    .replace(/\p{Cf}/gu, "");
}

function redactArtifactText(
  raw: string,
  terminalSafe: string,
  runtimeKnownSecrets: readonly string[],
): RedactedArtifactTextResult {
  if (/-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----/u.test(raw) && !PRIVATE_KEY.test(raw)) return { safe: false };
  PRIVATE_KEY.lastIndex = 0;
  if (/^(?:\s*(?:authorization|proxy-authorization)\s*:\s*)$/imu.test(raw)) return { safe: false };

  const terminalChanged = terminalSafe !== raw;
  let content = terminalSafe;
  let capabilityChanged = false;
  let credentialChanged = false;
  content = content.replace(FABRIC_BEARER, () => {
    capabilityChanged = true;
    return REDACTION;
  });
  const credentialReplace = (pattern: RegExp, replacer: string | ((...matches: string[]) => string)): void => {
    const next = content.replace(pattern, replacer as never);
    if (next !== content) credentialChanged = true;
    content = next;
  };
  credentialReplace(PRIVATE_KEY, REDACTION);
  credentialReplace(AUTHORIZATION, (_match: string, prefix: string) => `${prefix}${REDACTION}`);
  credentialReplace(URL_USERINFO, (_match: string, scheme: string) => `${scheme}${REDACTION}@`);
  credentialReplace(PROVIDER_TOKEN, REDACTION);
  credentialReplace(ASSIGNMENT_SECRET, (_match: string, prefix: string) => `${prefix}${REDACTION}`);
  for (const secret of [...new Set(runtimeKnownSecrets)].sort((left, right) => right.length - left.length)) {
    if (secret.length < 4) continue;
    credentialReplace(new RegExp(escapeRegex(secret), "gu"), REDACTION);
  }
  if (FABRIC_BEARER.test(content)) return { safe: false };
  FABRIC_BEARER.lastIndex = 0;

  const transformation: ArtifactContentTransformation =
    [terminalChanged, capabilityChanged, credentialChanged].filter(Boolean).length > 1
      ? "combined"
      : terminalChanged
        ? "terminal-neutralised"
        : capabilityChanged
          ? "capability-redacted"
          : credentialChanged
            ? "credential-redacted"
            : "none";
  return {
    safe: true,
    content,
    transformation,
    capabilityValuesRedacted: true,
    credentialValuesRedacted: true,
  };
}

export function inertArtifactText(
  raw: string,
  runtimeKnownSecrets: readonly string[] = [],
): InertArtifactTextResult {
  const protectedText = redactArtifactText(
    raw,
    neutraliseTerminal(raw),
    runtimeKnownSecrets,
  );
  return protectedText.safe
    ? { ...protectedText, terminalNeutralised: true }
    : protectedText;
}

export function redactArtifactTextPreservingControls(
  raw: string,
  runtimeKnownSecrets: readonly string[] = [],
): RedactedArtifactTextResult {
  return redactArtifactText(raw, raw, runtimeKnownSecrets);
}

function digest(value: string): `sha256:${string}` {
  return `sha256:${createHash("sha256").update(value).digest("hex")}`;
}

export type ArtifactTextPage = {
  content: string;
  pageIndex: number;
  lineFragment: ArtifactLineFragment;
  pageContentDigest: `sha256:${string}`;
  nextOffset: number;
};

export function pageArtifactText(input: {
  rendered: string;
  offset: number;
  pageIndex: number;
  maximumBytes: number;
  maximumLines: number;
}): ArtifactTextPage {
  const bytes = Buffer.from(input.rendered, "utf8");
  if (
    !Number.isSafeInteger(input.offset) || input.offset < 0 || input.offset > bytes.length ||
    (input.offset < bytes.length && (bytes[input.offset] ?? 0) >>> 6 === 2)
  ) throw new TypeError("artifact page offset must be a current UTF-8 boundary");
  if (!Number.isSafeInteger(input.pageIndex) || input.pageIndex < 0) throw new TypeError("page index is invalid");
  if (!Number.isSafeInteger(input.maximumBytes) || input.maximumBytes < 4 || input.maximumBytes > 131_072) {
    throw new TypeError("maximumBytes is invalid");
  }
  if (!Number.isSafeInteger(input.maximumLines) || input.maximumLines < 1 || input.maximumLines > 2_000) {
    throw new TypeError("maximumLines is invalid");
  }
  if (input.offset === bytes.length) {
    return {
      content: "",
      pageIndex: input.pageIndex,
      lineFragment: "whole",
      pageContentDigest: digest(""),
      nextOffset: input.offset,
    };
  }

  let end = Math.min(bytes.length, input.offset + input.maximumBytes);
  while (end < bytes.length && ((bytes[end] ?? 0) & 0xc0) === 0x80) end -= 1;
  if (end <= input.offset) throw new TypeError("maximumBytes cannot advance one UTF-8 code point");

  let newlineCount = 0;
  let lineBoundEnd: number | undefined;
  for (let cursor = input.offset; cursor < end; cursor += 1) {
    if (bytes[cursor] !== 0x0a) continue;
    newlineCount += 1;
    if (newlineCount === input.maximumLines) {
      lineBoundEnd = cursor + 1;
      break;
    }
  }
  if (lineBoundEnd !== undefined) end = lineBoundEnd;
  if (end < bytes.length && lineBoundEnd === undefined) {
    const lastNewline = bytes.subarray(input.offset, end).lastIndexOf(0x0a);
    if (lastNewline >= 0) end = input.offset + lastNewline + 1;
  }
  const startsAtLineBoundary = input.offset === 0 || bytes[input.offset - 1] === 0x0a;
  const endsAtLineBoundary = end === bytes.length || bytes[end - 1] === 0x0a;
  const lineFragment: ArtifactLineFragment = startsAtLineBoundary
    ? endsAtLineBoundary ? "whole" : "start"
    : endsAtLineBoundary ? "end" : "middle";
  const content = bytes.subarray(input.offset, end).toString("utf8");
  return {
    content,
    pageIndex: input.pageIndex,
    lineFragment,
    pageContentDigest: digest(content),
    nextOffset: end,
  };
}

export function artifactLineCount(content: string): number {
  if (content.length === 0) return 0;
  let lines = 1;
  for (const byte of Buffer.from(content, "utf8")) if (byte === 0x0a) lines += 1;
  return lines;
}
