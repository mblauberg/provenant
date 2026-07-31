#!/usr/bin/env python3
"""Comparison-form drift checks: doc claims must equal their source constants.

These claims are checked by comparison instead of marked-region generation,
deliberately (issue #548):

- ``HARNESS.md`` sits at its 60-line hard cap and 700-word budget
  (``tests/test_harness_contract.py``), so a marker pair has no room, and the
  same contract test forbids any repo-relative ``config/`` or ``scripts/`` path
  in the ambient files, so the region could not even name its source or its
  renderer. The review-pressure table and the machine-checking sentence are
  therefore compared against ``skills/_shared/review_ladder.py`` and
  ``config/risk-policy.json`` in place.
- The repair-budget figures in ``docs/ARCHITECTURE.md`` sit inline inside a
  Mermaid ``accDescr`` line and an edge label, where an HTML comment marker is
  a Mermaid parse error, so the expected phrases are generated from
  ``REPAIR_BUDGETS`` and the document must contain them verbatim.
- The grep-constant floor (design council P5, proposal 3's salvage): every
  state and risk-tier name the sources define appears in the docs, and no
  state-diagram identifier in ``docs/ARCHITECTURE.md`` names a state the
  validator does not define.

A failing check prints the expected, source-derived text, so the fix is a copy
from the report, never archaeology.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
NUMBER_WORDS = {2: "two", 3: "three", 4: "four", 5: "five", 6: "six", 7: "seven"}
TIER_ROW = re.compile(r"^\|\s*`(?P<tier>[a-z]+)`\s*\|")
FENCE = re.compile(r"^\s*(```|~~~)")
EDGE = re.compile(r"^\s*(?P<src>[A-Za-z_]\w*) --> (?P<dst>[A-Za-z_]\w*)")
CLASS_LINE = re.compile(r"^\s*class (?P<states>[A-Za-z_][\w,]*) \w+\s*$")
GROUP_LINE = re.compile(r'^\s*state ".+" as (?P<alias>\w+) \{\s*$')
BARE_STATE = re.compile(r"^\s{8}(?P<state>[a-z_]+)\s*$")


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"FAIL: cannot import {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def number_word(value: int) -> str:
    return NUMBER_WORDS.get(value, str(value))


def grouped_budgets(tiers: tuple[str, ...], budgets: dict[str, int]) -> list[tuple[list[str], int]]:
    """Consecutive tiers sharing a budget, in tier order: [([routine], 2), ...]."""
    segments: list[tuple[list[str], int]] = []
    for tier in tiers:
        if segments and segments[-1][1] == budgets[tier]:
            segments[-1][0].append(tier)
        else:
            segments.append(([tier], budgets[tier]))
    return segments


def expected_edge_label(segments) -> str:
    return "repair by tier: " + ", ".join(f"{'/'.join(names)} {value}" for names, value in segments)


def expected_accdescr_phrase(segments) -> str:
    parts = [f"{' and '.join(names)} {value}" for names, value in segments]
    parts[0] += " cycles"
    if len(parts) == 1:
        return parts[0]
    return ", ".join(parts[:-1]) + ", and " + parts[-1]


def check_review_table(text: str, doc: str, tiers: tuple[str, ...], errors: list[str]) -> None:
    lines = text.splitlines()
    try:
        start = next(i for i, line in enumerate(lines) if line.startswith("| Risk |"))
    except StopIteration:
        errors.append(f"{doc}: review-pressure table (header '| Risk |') is missing")
        return
    rows = []
    for line in lines[start + 1 :]:
        if not line.startswith("|"):
            break
        row = TIER_ROW.match(line)
        if row:
            rows.append(row.group("tier"))
    if tuple(rows) != tiers:
        errors.append(
            f"{doc}: review-pressure table rows are {tuple(rows)}, "
            f"but config/risk-policy.json tier_order is {tiers}"
        )


def state_diagram_identifiers(text: str) -> set[str]:
    """Identifiers used as states inside stateDiagram-v2 fences."""
    used: set[str] = set()
    fenced = False
    diagram = False
    for line in text.splitlines():
        if FENCE.match(line):
            fenced = not fenced
            diagram = False
            continue
        if not fenced:
            continue
        if line.strip() == "stateDiagram-v2":
            diagram = True
            continue
        if not diagram or GROUP_LINE.match(line):
            continue
        edge = EDGE.match(line)
        if edge:
            used.update({edge.group("src"), edge.group("dst")})
        assignment = CLASS_LINE.match(line)
        if assignment:
            used.update(assignment.group("states").split(","))
        bare = BARE_STATE.match(line)
        if bare:
            used.add(bare.group("state"))
    return used


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--architecture", type=Path, default=ROOT / "docs" / "ARCHITECTURE.md")
    parser.add_argument("--harness", type=Path, default=ROOT / "HARNESS.md")
    parser.add_argument("--readme", type=Path, default=ROOT / "README.md")
    parser.add_argument("--skills-root", type=Path, default=ROOT / "skills")
    parser.add_argument("--risk-policy", type=Path, default=ROOT / "config" / "risk-policy.json")
    args = parser.parse_args(argv)

    validator = load_module(
        "validate_delivery", args.skills_root / "deliver" / "scripts" / "validate_delivery.py"
    )
    ladder = load_module("review_ladder", args.skills_root / "_shared" / "review_ladder.py")
    policy = json.loads(args.risk_policy.read_text())

    tiers = tuple(policy["tier_order"])
    errors: list[str] = []

    # The two sources must agree with each other before either gates a doc.
    if tuple(validator.RISKS) != tiers:
        errors.append(
            f"validate_delivery.RISKS {tuple(validator.RISKS)} != risk-policy tier_order {tiers}"
        )
    if set(validator.REPAIR_BUDGETS) != set(tiers):
        errors.append("REPAIR_BUDGETS does not cover exactly the risk tiers")

    architecture = args.architecture.read_text()
    harness = args.harness.read_text()
    readme = args.readme.read_text()

    # Review-pressure tables: rows must be the tiers, in tier order.
    for doc, text in ((args.harness.name, harness), (args.readme.name, readme)):
        check_review_table(text, doc, tiers, errors)

    # The machine-checking sentence must state the ladder's constants.
    for fragment in (
        f"at least {number_word(ladder.TARGETED_LENS_MINIMUM)} distinct targeted lenses",
        f"raises that minimum to {number_word(ladder.TERMINAL_TARGETED_LENS_MINIMUM)}",
        *ladder.TERMINAL_PRESSURE_MARKERS,
    ):
        if fragment not in harness:
            errors.append(
                f"{args.harness.name}: expected review-ladder phrase not found: {fragment!r}"
            )

    # Repair budgets: both ARCHITECTURE.md sites must carry the derived phrase.
    segments = grouped_budgets(tiers, validator.REPAIR_BUDGETS)
    for fragment in (expected_edge_label(segments), expected_accdescr_phrase(segments)):
        if fragment not in architecture:
            errors.append(
                f"{args.architecture.name}: expected repair-budget phrase not found: {fragment!r}"
            )

    # Grep-constant floor: sources name it, the docs must say it.
    states = tuple(validator.NORMAL_STATES) + tuple(sorted(validator.SIDE_STATES))
    for state in states:
        if not re.search(rf"\b{re.escape(state)}\b", architecture):
            errors.append(f"{args.architecture.name}: lifecycle state never mentioned: {state}")
    for doc, text in (
        (args.architecture.name, architecture),
        (args.harness.name, harness),
        (args.readme.name, readme),
    ):
        for tier in tiers:
            if not re.search(rf"\b{re.escape(tier)}\b", text):
                errors.append(f"{doc}: risk tier never mentioned: {tier}")

    # And the docs must not say a state the validator does not define.
    unknown = sorted(state_diagram_identifiers(architecture) - set(states))
    if unknown:
        errors.append(
            f"{args.architecture.name}: state diagram uses identifiers the validator "
            f"does not define: {', '.join(unknown)}"
        )

    if errors:
        for error in errors:
            print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print(
        f"PASS: docs match review_ladder.py, validate_delivery.py and risk-policy.json "
        f"({len(states)} states, {len(tiers)} tiers)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
