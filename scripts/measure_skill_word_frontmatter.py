#!/usr/bin/env python3
"""Measure whether frontmatter inclusion changes skill budget status."""

from __future__ import annotations

import re
from pathlib import Path

from count_skill_words import WORD_COUNT_HARD_LIMIT, count_skill_words


ROOT = Path(__file__).resolve().parents[1]
FRONTMATTER = re.compile(r"^---\n.*?\n---\n", re.DOTALL)


def body_only_count(text: str) -> int:
    return count_skill_words(FRONTMATTER.sub("", text, count=1))


def main() -> int:
    body_over = set()
    whole_over = set()
    for path in sorted((ROOT / "skills").glob("*/SKILL.md")):
        text = path.read_text()
        relative = str(path.relative_to(ROOT))
        if body_only_count(text) > WORD_COUNT_HARD_LIMIT:
            body_over.add(relative)
        if count_skill_words(text) > WORD_COUNT_HARD_LIMIT:
            whole_over.add(relative)

    moved = sorted(body_over ^ whole_over)
    print(f"Body-only files over {WORD_COUNT_HARD_LIMIT}: {len(body_over)}")
    print(f"Whole-file files over {WORD_COUNT_HARD_LIMIT}: {len(whole_over)}")
    if not moved:
        print("No files changed budget status")
    else:
        print("Files moved between under and over budget:")
        for path in moved:
            before = "over" if path in body_over else "under"
            after = "over" if path in whole_over else "under"
            print(f"- {path}: {before} -> {after}")
    print(f"Impact: {len(moved)} files changed status")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
