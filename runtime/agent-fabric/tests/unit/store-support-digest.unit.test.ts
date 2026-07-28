import { describe, expect, it } from "vitest";

import { canonicalJson, digest, stringDigest } from "../../src/persistence/row-codec.ts";

describe("row-codec digests", () => {
  it("digest canonicalizes exactly once", () => {
    expect(digest({ a: 1 })).toBe("sha256:015abd7f5cc57a2dd94b7590f04ad8084273905ee33ec5cebeae62276a97f862");
  });

  it("stringDigest hashes the given string without re-canonicalizing", () => {
    expect(stringDigest(canonicalJson({ a: 1 }))).toBe(digest({ a: 1 }));
    expect(stringDigest('{"a":1}')).not.toBe(stringDigest('"{\\"a\\":1}"'));
  });
});
