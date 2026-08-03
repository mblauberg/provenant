#!/usr/bin/env python3
"""Static checks for the global agent harness."""

from __future__ import annotations

import re
from pathlib import Path
import sys

import yaml


ROOT = Path(__file__).resolve().parents[1]
DESCRIPTION_LIMIT_CHARS = 1_024


def skill_errors() -> list[str]:
    errors: list[str] = []
    link_pattern = re.compile(r"\[[^]]+\]\(([^)]+)\)")
    for skill in sorted((ROOT / "skills").glob("*/SKILL.md")):
        text = skill.read_text()
        match = re.match(r"^---\n(.*?)\n---\n", text, re.DOTALL)
        if not match:
            errors.append(f"{skill.relative_to(ROOT)}: missing YAML frontmatter")
            continue
        try:
            frontmatter = yaml.safe_load(match.group(1))
        except yaml.YAMLError as exc:
            errors.append(f"{skill.relative_to(ROOT)}: invalid YAML: {exc}")
            continue
        if not isinstance(frontmatter, dict):
            errors.append(f"{skill.relative_to(ROOT)}: frontmatter must be a mapping")
            continue
        if set(frontmatter) != {"name", "description"}:
            errors.append(f"{skill.relative_to(ROOT)}: local frontmatter profile permits only name and description")
        expected = skill.parent.name
        if frontmatter.get("name") != expected:
            errors.append(
                f"{skill.relative_to(ROOT)}: name {frontmatter.get('name')!r} != directory {expected!r}"
            )
        description = frontmatter.get("description")
        if not isinstance(description, str) or not description.strip():
            errors.append(f"{skill.relative_to(ROOT)}: description is required")
        elif len(description) > DESCRIPTION_LIMIT_CHARS:
            errors.append(f"{skill.relative_to(ROOT)}: description exceeds {DESCRIPTION_LIMIT_CHARS} characters")
        fixture = skill.parent / "evals" / "trigger_cases.yaml"
        if not fixture.is_file():
            errors.append(f"{skill.relative_to(ROOT)}: missing evals/trigger_cases.yaml")
        for target in link_pattern.findall(text):
            if target.startswith(("http://", "https://", "#", "/")):
                continue
            relative = target.split("#", 1)[0]
            if relative and not (skill.parent / relative).exists():
                errors.append(f"{skill.relative_to(ROOT)}: broken link {target}")
    return errors


def openai_sidecar_errors() -> list[str]:
    errors: list[str] = []
    for path in sorted((ROOT / "skills").glob("*/agents/openai.yaml")):
        skill = path.parents[1].name
        try:
            data = yaml.safe_load(path.read_text())
        except yaml.YAMLError as exc:
            errors.append(f"{path.relative_to(ROOT)}: invalid YAML: {exc}")
            continue
        interface = data.get("interface") if isinstance(data, dict) else None
        if not isinstance(interface, dict):
            errors.append(f"{path.relative_to(ROOT)}: interface mapping is required")
            continue
        for field in ("display_name", "short_description", "default_prompt"):
            if not isinstance(interface.get(field), str) or not interface[field].strip():
                errors.append(f"{path.relative_to(ROOT)}: interface.{field} is required")
        short_description = interface.get("short_description")
        if isinstance(short_description, str) and short_description.strip() and not 25 <= len(short_description) <= 64:
            errors.append(
                f"{path.relative_to(ROOT)}: interface.short_description must be 25-64 characters"
            )
        if f"${skill}" not in str(interface.get("default_prompt", "")):
            errors.append(f"{path.relative_to(ROOT)}: default_prompt must invoke ${skill}")
        for icon in ("icon_small", "icon_large"):
            value = interface.get(icon)
            if isinstance(value, str) and value.startswith("./") and not (path.parents[1] / value).exists():
                errors.append(f"{path.relative_to(ROOT)}: missing {icon} asset {value}")
    return errors


def main() -> int:
    errors = skill_errors() + openai_sidecar_errors()
    if errors:
        for error in errors:
            print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print("PASS: frontmatter, fixtures, links and sidecars clean")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
