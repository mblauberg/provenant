#!/usr/bin/env python3
"""Scan academic prose (and LaTeX) for style hazards that are cheap to detect.

Thin wrapper around this skill's shared engine (`style_lint.py`): this file
supplies only the academic-specific phrase/pattern overlay (thesis meta-discourse, LaTeX
citation placeholders, unescaped-percent detection) and turns on LaTeX-aware
span handling so math, verbatim environments, and non-prose commands like
\\cite/\\label are skipped rather than flagged.
"""

from __future__ import annotations

import importlib.util
import re
import sys
from pathlib import Path

_STYLE_LINT_PATH = Path(__file__).resolve().parent / "style_lint.py"
_spec = importlib.util.spec_from_file_location("style_lint", _STYLE_LINT_PATH)
assert _spec and _spec.loader
style_lint = importlib.util.module_from_spec(_spec)
sys.modules.setdefault("style_lint", style_lint)
_spec.loader.exec_module(style_lint)


ACADEMIC_EXTRA_PHRASES: dict[str, list[str]] = {
    "Meta-discourse or report-referential": [
        "for this thesis report",
        "this thesis report",
        "the aim of this report is",
        "this write-up",
        "we will now discuss",
        "the present study sets out",
        "as the reader will see",
    ],
}

ACADEMIC_EXTRA_PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    ("LaTeX prose em dash marker", re.compile(r"(?<!-)---(?!-)")),
    ("Generic placeholder", re.compile(r"\b(TODO|TBD|FIXME|insert result|citation needed)\b", re.I)),
    ("Citation placeholder", re.compile(r"\\cite\{(?:todo|TODO|missing|source|citation|ref)\}")),
    ("Likely unescaped percent in prose", re.compile(r"(?<!\\)\d+%")),
]

PHRASES = {**style_lint.BASE_PHRASES, **ACADEMIC_EXTRA_PHRASES}
PATTERNS = style_lint.BASE_PATTERNS + ACADEMIC_EXTRA_PATTERNS


def main() -> int:
    return style_lint.run_cli(__doc__, phrases=PHRASES, patterns=PATTERNS, latex=True)


if __name__ == "__main__":
    raise SystemExit(main())
