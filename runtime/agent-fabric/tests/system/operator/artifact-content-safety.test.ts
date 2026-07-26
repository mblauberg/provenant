import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";

import {
  inertArtifactText,
  pageArtifactText,
  redactArtifactTextPreservingControls,
} from "../../../src/operator/artifact-content-safety.ts";

const digest = (value: string): string =>
  `sha256:${createHash("sha256").update(value).digest("hex")}`;

describe("artifact content safety and paging", () => {
  it("neutralises terminal controls and completely redacts capability and credential families", () => {
    const privateKey = [
      "-----BEGIN ",
      "PRIVATE KEY-----\nprivate material\n-----END PRIVATE KEY-----",
    ].join("");
    const result = inertArtifactText(
      [
        "\u001b]0;spoof\u0007safe",
        "agent=afb_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
        "operator=afop_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG",
        "Authorization: Bearer provider-secret-value",
        "password=hunter2",
        "https://name:password@example.invalid/path",
        privateKey,
      ].join("\n"),
      ["provider-secret-value"],
    );

    expect(result.safe).toBe(true);
    if (!result.safe) return;
    expect(result.transformation).toBe("combined");
    expect(result.content).not.toMatch(/\u001b|afb_|afop_|provider-secret-value|hunter2|private material|name:password/iu);
    expect(result.content).toContain("safe");
  });

  it("emits monotonic UTF-8 pages that reconstruct the complete rendered digest", () => {
    const rendered = "alpha\nβeta-long-line\nomega";
    const pages: string[] = [];
    let offset = 0;
    let pageIndex = 0;
    while (offset < Buffer.byteLength(rendered, "utf8")) {
      const page = pageArtifactText({ rendered, offset, pageIndex, maximumBytes: 7, maximumLines: 2 });
      expect(Buffer.byteLength(page.content, "utf8")).toBeLessThanOrEqual(7);
      expect(page.pageContentDigest).toBe(digest(page.content));
      expect(page.nextOffset).toBeGreaterThan(offset);
      pages.push(page.content);
      offset = page.nextOffset;
      pageIndex += 1;
    }
    expect(pages.join("")).toBe(rendered);
    expect(digest(pages.join(""))).toBe(digest(rendered));
  });

  it("supports the empty artifact without manufacturing a line", () => {
    const page = pageArtifactText({ rendered: "", offset: 0, pageIndex: 0, maximumBytes: 4, maximumLines: 1 });
    expect(page).toMatchObject({ content: "", nextOffset: 0, lineFragment: "whole" });
  });

  it.each([
    ["CSI", "before\u001b[31mafter", "before after"],
    ["OSC", "before\u001b]0;spoof\u0007after", "before after"],
    ["DCS", "before\u001bPpayload\u001b\\after", "before after"],
    ["APC", "before\u001b_payload\u001b\\after", "before after"],
    ["PM", "before\u001b^payload\u001b\\after", "before after"],
    ["SOS", "before\u001bXpayload\u001b\\after", "before after"],
    ["C0", "before\u0008after", "before after"],
    ["C1", "before\u009bafter", "before after"],
    ["carriage return", "before\rafter", "before\nafter"],
    ["bidi override", "before\u202eafter", "beforeafter"],
    ["format control", "before\u2060after", "beforeafter"],
  ] as const)("neutralises the %s terminal family", (_name, value, expected) => {
    const result = inertArtifactText(value);
    expect(result.safe).toBe(true);
    if (!result.safe) return;
    expect(result.transformation).toBe("terminal-neutralised");
    expect(result.content).toBe(expected);
    expect(result.content).not.toMatch(/[\r\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069\p{Cf}]/u);
  });

  it.each([
    ["bootstrap bearer", "afb_abcdefghijklmno"],
    ["agent bearer", "afc_abcdefghijklmno"],
    ["operator bearer", "afop_abcdefghijklmno"],
    ["AWS token", `AKIA${"ABCDEFGHIJKLMNOP"}`],
    ["Google token", "AIzaabcdefghijklmnopqrstuvwx"],
    ["GitHub token", `ghp_${"abcdefghijklmnopqrstuvwxyz"}`],
    ["provider key", `sk-${"abcdefghijklmnopqrstuv"}`],
    ["Slack token", "xoxb-1234567890-abcdefghij"],
    ["password assignment", "password=hunter2"],
    ["secret assignment", "secret:classified-value"],
    ["credential assignment", "credential='credential-value'"],
    ["private-key assignment", "private_key=private-key-value"],
    ["authorisation header", "Authorization: Bearer classified-value"],
    ["proxy authorisation", "Proxy-Authorization: Basic classified-value"],
    ["URL userinfo", "https://name:password@example.invalid/path"],
    [
      "private key block",
      ["-----BEGIN ", "PRIVATE KEY-----\nclassified material\n-----END PRIVATE KEY-----"].join(""),
    ],
  ] as const)("fully redacts the %s credential family", (_name, value) => {
    const result = inertArtifactText(
      /authorization:/iu.test(value)
        ? `before\n${value}\nafter`
        : `before ${value} after`,
    );
    expect(result.safe).toBe(true);
    if (!result.safe) return;
    expect(["capability-redacted", "credential-redacted"]).toContain(result.transformation);
    expect(result.content).toContain("before");
    expect(result.content).toContain("after");
    expect(result.content).not.toContain(value);
    expect(result.content).not.toMatch(/afb_|afc_|afop_|hunter2|classified|credential-value|private-key-value|name:password/iu);
  });

  it("redacts runtime-known secrets and rejects unclosed sensitive constructs", () => {
    const runtimeSecret = "runtime-secret-unique-value";
    const redacted = inertArtifactText(`before ${runtimeSecret} after`, [runtimeSecret]);
    expect(redacted.safe).toBe(true);
    if (redacted.safe) expect(redacted.content).toBe("before █ after");
    expect(inertArtifactText(["-----BEGIN ", "PRIVATE KEY-----\nunclosed"].join(""))).toEqual({ safe: false });
    expect(inertArtifactText("Authorization:")).toEqual({ safe: false });
  });

  it("labels whole/start/middle/end boundaries without splitting UTF-8", () => {
    const rendered = "abcdefgh\nβeta\n";
    const pages = [];
    let offset = 0;
    let pageIndex = 0;
    while (offset < Buffer.byteLength(rendered, "utf8")) {
      const page = pageArtifactText({ rendered, offset, pageIndex, maximumBytes: 4, maximumLines: 1 });
      pages.push(page);
      offset = page.nextOffset;
      pageIndex += 1;
    }
    expect(pages.map(({ lineFragment }) => lineFragment)).toEqual([
      "start",
      "middle",
      "end",
      "start",
      "end",
    ]);
    expect(pages.map(({ content }) => content).join("")).toBe(rendered);
    expect(pages.every(({ content }) => Buffer.byteLength(content, "utf8") <= 4)).toBe(true);
  });

  it("never lets the control-preserving path expose what the inert path hides", () => {
    // Every credential pattern and the final withholding gate match against the
    // terminal-safe text, so detecting on the raw string lets one invisible
    // character inside a token defeat all of them. The Console's own sanitiser
    // visualises C0/C1 and bidi but not U+200B/U+00AD, so such a token would
    // render on screen as a readable, copyable capability.
    const split: Record<string, string> = {
      zeroWidth: "capability=afb_​LIVECAPABILITY123456",
      softHyphen: "capability=afb_­LIVECAPABILITY123456",
      bidi: "token=sk-‮ABCDEFGHIJKLMNOPQRSTUVWX",
      joiner: "capability=afb_‍LIVECAPABILITY123456",
    };
    for (const [name, raw] of Object.entries(split)) {
      const inert = inertArtifactText(raw);
      const preserving = redactArtifactTextPreservingControls(raw);
      if (!inert.safe) {
        expect(preserving.safe, name).toBe(false);
        continue;
      }
      // Compare with invisibles stripped: that is what a reader actually sees.
      const visible = preserving.safe
        ? preserving.content.replace(/[​-‏­‪-‮]/gu, "")
        : "";
      expect(/afb_[A-Za-z0-9]|sk-[A-Za-z0-9]/u.test(visible), name).toBe(false);
    }
  });

  it("still preserves controls for text with nothing to redact", () => {
    const result = redactArtifactTextPreservingControls("left‮right");

    expect(result).toMatchObject({ safe: true, transformation: "none" });
    expect(result.safe ? result.content : "").toContain("‮");
  });

  it("honours exact byte and line bounds and rejects invalid continuation offsets", () => {
    const exact = pageArtifactText({ rendered: "abcd", offset: 0, pageIndex: 0, maximumBytes: 4, maximumLines: 1 });
    expect(exact).toMatchObject({ content: "abcd", nextOffset: 4, lineFragment: "whole" });
    const lineBound = pageArtifactText({ rendered: "a\nb\nc", offset: 0, pageIndex: 0, maximumBytes: 12, maximumLines: 1 });
    expect(lineBound).toMatchObject({ content: "a\n", nextOffset: 2, lineFragment: "whole" });
    expect(() => pageArtifactText({ rendered: "β", offset: 1, pageIndex: 0, maximumBytes: 4, maximumLines: 1 }))
      .toThrow(/UTF-8 boundary/iu);
    expect(() => pageArtifactText({ rendered: "abcd", offset: 0, pageIndex: 0, maximumBytes: 3, maximumLines: 1 }))
      .toThrow(/maximumBytes/iu);
  });
});
