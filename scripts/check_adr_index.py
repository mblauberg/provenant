"""Check docs/adr/README.md against the ADR files it indexes.

The index drifted twice before this gate existed: ADR 0008 gained an amendment
and an addendum, and ADR 0013 gained a documented passthrough, while both index
rows still described the original decision. Nothing compared the two.

Scope, stated so the gate is not mistaken for more than it is: this checks the
index against each ADR's own status line and section headings. It cannot tell
whether an ADR's prose still matches the code — that is a different gate.
"""

from __future__ import annotations

from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
ADR_DIR = ROOT / "docs" / "adr"
INDEX = ADR_DIR / "README.md"

# Both heading styles are in use: "# ADR 0001 — Title" and "# ADR 0014: Title".
TITLE = re.compile(r"^#\s+ADR\s+(\d{4})\s*[—:-]\s*(.+?)\s*$", re.M)
# Likewise two status styles: "**Status:** ..." and a "## Status" section.
INLINE_STATUS = re.compile(r"^\*\*Status:\*\*\s*(.+?)(?=\n\n|\n\*\*|\n##)", re.M | re.S)
SECTION_STATUS = re.compile(r"^##\s+Status\s*\n+(.+?)(?=\n\n|\n##)", re.M | re.S)
ROW = re.compile(r"^\|\s*\[(\d{4})\]\(([^)]+)\)\s*\|\s*(.+?)\s*\|\s*(.+?)\s*\|\s*$", re.M)
ADDENDUM = re.compile(r"^#{2,4}\s+Addendum\b", re.M)

# A body marker must be reflected in the index cell. Each entry maps a marker
# found in the ADR to the words the index row may use to describe it.
MARKERS = {
    "amended": ("amend",),
    "superseded": ("supersede",),
    "addendum": ("addendum", "amend"),
}


def failures() -> list[str]:
    found: list[str] = []
    index_text = INDEX.read_text(encoding="utf-8")
    rows = {number: (target, title, status) for number, target, title, status in ROW.findall(index_text)}
    files = sorted(p for p in ADR_DIR.glob("[0-9][0-9][0-9][0-9]-*.md"))

    numbers = []
    for path in files:
        text = path.read_text(encoding="utf-8")
        title_match = TITLE.search(text)
        if title_match is None:
            found.append(f"{path.name}: no parsable '# ADR NNNN — Title' heading")
            continue
        number, title = title_match.groups()
        numbers.append(int(number))

        if number not in rows:
            found.append(f"{path.name}: ADR {number} has no row in docs/adr/README.md")
            continue
        target, row_title, row_status = rows[number]

        if target != path.name:
            found.append(f"ADR {number}: index links '{target}' but the file is '{path.name}'")
        if row_title.strip() != title.strip():
            found.append(f"ADR {number}: index title '{row_title}' != heading title '{title}'")

        status_match = INLINE_STATUS.search(text) or SECTION_STATUS.search(text)
        if status_match is None:
            found.append(f"{path.name}: no '**Status:**' line or '## Status' section")
            continue
        body_status = " ".join(status_match.group(1).split()).lower()
        if ADDENDUM.search(text):
            body_status += " addendum"

        row_status_lower = row_status.lower()
        for marker, accepted in MARKERS.items():
            if marker in body_status and not any(word in row_status_lower for word in accepted):
                found.append(
                    f"ADR {number}: body records '{marker}' but the index row says "
                    f"'{row_status}' — the index must reflect it"
                )

    for number in rows:
        if not (ADR_DIR / rows[number][0]).is_file():
            found.append(f"index row {number} points at missing file '{rows[number][0]}'")

    if numbers:
        expected = set(range(1, max(numbers) + 1))
        for missing in sorted(expected - set(numbers)):
            found.append(f"ADR numbering gap: {missing:04d} is absent")

    return found


def main() -> int:
    problems = failures()
    if problems:
        for problem in problems:
            print(f"adr-index: {problem}", file=sys.stderr)
        return 1
    print(f"adr-index: ok ({len(list(ADR_DIR.glob('[0-9][0-9][0-9][0-9]-*.md')))} ADRs)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
