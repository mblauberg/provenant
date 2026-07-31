#!/usr/bin/env python3
"""Render the derived regions of docs/ARCHITECTURE.md from their source constants.

Two marked regions are owned here (issue #548, design council P5):

- ``delivery-state-machine``: the Mermaid state diagram body, rendered from
  ``NORMAL_STATES``, ``SIDE_STATES`` and ``TRANSITIONS`` in
  ``skills/deliver/scripts/validate_delivery.py``.
- ``risk-factor-table``: the factor-to-tier table, rendered from
  ``config/risk-policy.json``.

The markers are HTML comments, so they cannot sit inside the Mermaid fence
(Mermaid only accepts ``%%`` comments and an HTML comment is a parse error).
The region therefore wraps the whole fence, and the hand-written meaning inside
it survives the way the skill catalogue's Area rows do: ``accTitle``,
``accDescr``, ``classDef``, ``class``, the group titles and the edge labels are
read back from the on-disk region and re-emitted verbatim. Only the graph
structure, which states exist, how they group and which edges connect them, is
generated. Editing structure in the document fails ``--check``; editing the
preserved lines round-trips freely.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import re
import sys

try:
    from scripts.lib.projection import ProjectionError, project, split_marked_region
except ModuleNotFoundError:  # run directly: sys.path[0] is scripts/
    from lib.projection import ProjectionError, project, split_marked_region  # type: ignore[no-redef]

ROOT = Path(__file__).resolve().parents[1]
ARCHITECTURE_PATH = ROOT / "docs" / "ARCHITECTURE.md"
RISK_POLICY_PATH = ROOT / "config" / "risk-policy.json"
STATE_MARKER = "delivery-state-machine"
RISK_MARKER = "risk-factor-table"
SOURCE_LABEL = "validate_delivery.py + config/risk-policy.json"

EDGE_LINE = re.compile(r"^\s*(?P<src>[A-Za-z_]\w*) --> (?P<dst>[A-Za-z_]\w*)(?: : (?P<label>.+?))?\s*$")
GROUP_LINE = re.compile(r'^\s*state "(?P<title>.+)" as (?P<alias>\w+) \{\s*$')
CLASS_LINE = re.compile(r"^\s*class (?P<states>[A-Za-z_][\w,]*) (?P<name>\w+)\s*$")


class RenderError(ProjectionError):
    pass


def load_delivery_constants(skills_root: Path):
    """Import the validator by path; its constants are the source of truth."""
    path = skills_root / "deliver" / "scripts" / "validate_delivery.py"
    spec = importlib.util.spec_from_file_location("validate_delivery", path)
    if spec is None or spec.loader is None:
        raise RenderError(f"cannot import {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    normal = tuple(module.NORMAL_STATES)
    side = frozenset(module.SIDE_STATES)
    transitions = {state: set(targets) for state, targets in module.TRANSITIONS.items()}
    if set(transitions) != set(normal):
        raise RenderError("TRANSITIONS and NORMAL_STATES disagree about which states exist")
    stray = sorted(set().union(*transitions.values()) - set(normal))
    if stray:
        raise RenderError(f"TRANSITIONS target unknown state(s): {', '.join(stray)}")
    if side & set(normal):
        raise RenderError("SIDE_STATES overlap NORMAL_STATES")
    return normal, side, transitions


def parse_preserved(region: str, normal: tuple[str, ...], side: frozenset[str]):
    """Read the hand-written lines back out of the on-disk region."""
    known = set(normal) | side
    acc_lines: list[str] = []
    style_lines: list[str] = []
    titles: dict[str, str] = {}
    labels: dict[tuple[str, str], str] = {}
    for line in region.splitlines():
        stripped = line.strip()
        if stripped.startswith(("accTitle:", "accDescr:")):
            acc_lines.append(line)
            continue
        if stripped.startswith("classDef "):
            style_lines.append(line)
            continue
        group = GROUP_LINE.match(line)
        if group:
            titles[group.group("alias")] = group.group("title")
            continue
        assignment = CLASS_LINE.match(line)
        if assignment:
            unknown = sorted(set(assignment.group("states").split(",")) - known)
            if unknown:
                raise RenderError(
                    f"class line styles state(s) the validator does not define: {', '.join(unknown)}"
                )
            style_lines.append(line)
            continue
        edge = EDGE_LINE.match(line)
        if edge and edge.group("label"):
            labels[(edge.group("src"), edge.group("dst"))] = edge.group("label")
    if len(acc_lines) != 2:
        raise RenderError("the state diagram must keep its hand-written accTitle and accDescr")
    for alias in ("run", "aside"):
        if alias not in titles:
            raise RenderError(f'the state diagram must keep its `state "..." as {alias}` group')
    return acc_lines, style_lines, titles, labels


def render_state_region(region: str, normal, side, transitions) -> str:
    acc_lines, style_lines, titles, labels = parse_preserved(region, normal, side)
    stale = sorted(
        f"{src} --> {dst}" for (src, dst) in labels if dst not in transitions.get(src, ())
    )
    if stale:
        raise RenderError(
            "edge label(s) for transitions the validator does not define: " + "; ".join(stale)
        )
    order = {state: index for index, state in enumerate(normal)}
    edges = []
    for src in normal:
        for dst in sorted(transitions[src], key=order.__getitem__):
            label = labels.get((src, dst))
            edges.append(f"        {src} --> {dst}" + (f" : {label}" if label else ""))
    lines = [
        "```mermaid",
        "stateDiagram-v2",
        *acc_lines,
        f"    [*] --> {normal[0]}",
        "",
        f'    state "{titles["run"]}" as run {{',
        *edges,
        "    }",
        "",
        *(f"    {state} --> [*]" for state in normal if not transitions[state]),
        "",
        f'    state "{titles["aside"]}" as aside {{',
        *(f"        {state}" for state in sorted(side)),
        "    }",
        "",
        *style_lines,
        "```",
    ]
    return "\n" + "\n".join(lines) + "\n"


def render_risk_region(policy: dict) -> str:
    tiers = policy["tier_order"]
    factors = policy["factors"]
    lines = [
        "| Factor | " + " | ".join(f"`{tier}`" for tier in tiers) + " |",
        "|---|" + "---|" * len(tiers),
    ]
    for factor, ratings in factors.items():
        cells = [
            ", ".join(f"`{value}`" for value, tier in ratings.items() if tier == column)
            for column in tiers
        ]
        lines.append(f"| {factor.replace('_', ' ')} | " + " | ".join(cells) + " |")
    return "\n\n" + "\n".join(lines) + "\n\n"


def render(text: str, skills_root: Path, risk_policy_path: Path) -> str:
    normal, side, transitions = load_delivery_constants(skills_root)
    head, region, tail = split_marked_region(text, STATE_MARKER, "ARCHITECTURE.md")
    text = head + render_state_region(region, normal, side, transitions) + tail
    head, _, tail = split_marked_region(text, RISK_MARKER, "ARCHITECTURE.md")
    policy = json.loads(risk_policy_path.read_text())
    return head + render_risk_region(policy) + tail


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="report drift and exit non-zero; never writes")
    mode.add_argument("--write", action="store_true", help="rewrite drifted regions in place")
    parser.add_argument("--architecture", type=Path, default=ARCHITECTURE_PATH)
    parser.add_argument("--skills-root", type=Path, default=ROOT / "skills")
    parser.add_argument("--risk-policy", type=Path, default=RISK_POLICY_PATH)
    args = parser.parse_args(argv)
    try:
        rendered = render(args.architecture.read_text(), args.skills_root, args.risk_policy)
        # project() asserts the state-machine markers; assert the risk markers
        # too, so deleting either pair fails rather than skipping the region.
        split_marked_region(rendered, RISK_MARKER, f"rendered {args.architecture.name}")
        report = project(args.architecture, STATE_MARKER, rendered, args.check, source=SOURCE_LABEL)
    except (OSError, KeyError, ProjectionError) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    name = args.architecture.name
    if args.check:
        if report:
            sys.stdout.writelines(report)
            print(
                f"FAIL: {name} derived regions are stale; run scripts/render_doc_projections.py --write",
                file=sys.stderr,
            )
            return 1
        print(f"PASS: {name} state machine and risk-factor table match their sources")
        return 0
    print(f"rendered: {name} " + ("updated from its sources" if report else "already matches its sources"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
