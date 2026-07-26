import { describe, expect, it } from "vitest";

import * as Console from "../src/index.js";

describe("pinned Unicode and terminal-neutral text", () => {
  it("pins grapheme segmentation and cell widths for combining, CJK, and emoji", () => {
    expect(Console.UNICODE_POLICY).toStrictEqual({
      segmentation: "unicode-segmenter@0.17.0",
      width: "string-width@8.2.2",
      ambiguousWidth: "narrow",
    });
    expect(Console.cellWidth("e\u0301")).toBe(1);
    expect(Console.cellWidth("界")).toBe(2);
    expect(Console.cellWidth("👩‍💻")).toBe(2);
    expect([...Console.graphemes("e\u0301界👩‍💻")]).toStrictEqual([
      "e\u0301",
      "界",
      "👩‍💻",
    ]);
    expect(Console.clipCells("e\u0301界👩‍💻Z", 5)).toBe("e\u0301界~ ");
  });

  it("neutralises controls and bidi formatting with visible stable tokens", () => {
    const hostile = "ok\u001b[31m\u0007\u009b\u007f\u202e\u2066\r\n\tend";
    const sanitized = Console.sanitizeDisplayText(hostile, {
      lineBreaks: "visible",
    });

    expect(sanitized).not.toContain("\u001b");
    expect(sanitized).not.toContain("\u009b");
    expect(sanitized).not.toContain("\u202e");
    expect(sanitized).not.toContain("\u2066");
    expect(sanitized).toContain("<ESC>");
    expect(sanitized).toContain("<BEL>");
    expect(sanitized).toContain("<C1-9B>");
    expect(sanitized).toContain("<DEL>");
    expect(sanitized).toContain("<BIDI-U+202E>");
    expect(sanitized).toContain("<BIDI-U+2066>");
    expect(sanitized).toContain("<LF>");
  });

  it("uses the same visible neutralisation for grouped activity text", () => {
    const grouped = Console.sanitizeDisplayText(
      "task group\u001b[31m | member\u202e | detail\u0007",
      { lineBreaks: "visible" },
    );

    expect(grouped).toBe(
      "task group<ESC>[31m | member<BIDI-U+202E> | detail<BEL>",
    );
  });
});
