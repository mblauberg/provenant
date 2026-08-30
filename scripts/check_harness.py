#!/usr/bin/env python3
"""Static checks for the global agent harness."""

from __future__ import annotations

import re
from pathlib import Path
import sys

import yaml


ROOT = Path(__file__).resolve().parents[1]
DESCRIPTION_LIMIT_CHARS = 1_024


def load_route_cases(skill_dir: Path, valid_skills: set[str]) -> list[dict]:
    """Load canonical and optional regression routes through one strict contract."""

    target_skill = skill_dir.name
    cases: list[dict] = []
    seen_ids: set[str] = set()
    for filename, id_prefix in (("trigger_cases.yaml", "q"), ("regression_cases.yaml", "r")):
        path = skill_dir / "evals" / filename
        if filename == "regression_cases.yaml" and not path.exists():
            continue
        if not path.is_file():
            raise ValueError(f"{path}: missing route fixture")
        try:
            fixture = yaml.safe_load(path.read_text())
        except yaml.YAMLError as error:
            raise ValueError(f"{path}: invalid YAML: {error}") from error
        if not isinstance(fixture, dict) or set(fixture) != {"schema_version", "target_skill", "cases"}:
            raise ValueError(f"{path}: fixture must contain schema_version, target_skill and cases")
        if fixture["schema_version"] != 1 or fixture["target_skill"] != target_skill:
            raise ValueError(f"{path}: fixture version or target_skill mismatch")
        if not isinstance(fixture["cases"], list) or not fixture["cases"]:
            raise ValueError(f"{path}: cases must be a non-empty list")

        for case in fixture["cases"]:
            allowed_keys = {"id", "relation", "prompt", "tags", "expected"}
            if filename == "regression_cases.yaml":
                allowed_keys.add("note")
            if not isinstance(case, dict) or set(case) != allowed_keys:
                raise ValueError(f"{path}: route case has malformed keys")
            case_id = case.get("id")
            relation = case.get("relation")
            prompt = case.get("prompt")
            tags = case.get("tags")
            if (
                not isinstance(case_id, str)
                or re.fullmatch(rf"{id_prefix}\d+", case_id) is None
                or case_id in seen_ids
                or relation not in {"positive", "negative", "boundary"}
                or not isinstance(prompt, str)
                or not prompt.strip()
                or not isinstance(tags, list)
                or not tags
                or any(not isinstance(tag, str) or not tag for tag in tags)
                or len(tags) != len(set(tags))
            ):
                raise ValueError(f"{path}: malformed route case {case_id!r}")
            expected = case.get("expected")
            if not isinstance(expected, dict) or set(expected) != {"primary_skill", "companion_skills"}:
                raise ValueError(f"{path}: malformed expected route for {case_id}")
            primary = expected["primary_skill"]
            companions = expected["companion_skills"]
            if (
                (primary is not None and primary not in valid_skills)
                or not isinstance(companions, list)
                or any(skill not in valid_skills for skill in companions)
                or len(companions) != len(set(companions))
                or primary in companions
            ):
                raise ValueError(f"{path}: invalid skill route for {case_id}")
            if relation == "positive" and primary != target_skill:
                raise ValueError(f"{path}: positive route must use target as primary for {case_id}")
            if relation == "negative" and (primary == target_skill or target_skill in companions):
                raise ValueError(f"{path}: negative route must exclude target for {case_id}")
            if (
                filename == "regression_cases.yaml"
                and relation == "boundary"
                and target_skill not in {primary, *companions}
            ):
                raise ValueError(f"{path}: boundary route must include target for {case_id}")
            if relation == "boundary" and not {"adjacent", "composition"} & set(tags):
                raise ValueError(f"{path}: boundary route needs adjacent or composition tag for {case_id}")
            if primary is None and (companions or "no-skill" not in tags):
                raise ValueError(f"{path}: no-skill route malformed for {case_id}")
            if filename == "regression_cases.yaml" and (
                not isinstance(case.get("note"), str) or not case["note"].strip()
            ):
                raise ValueError(f"{path}: regression note is required for {case_id}")
            seen_ids.add(case_id)
            cases.append(case)
    return cases


def skill_errors() -> list[str]:
    errors: list[str] = []
    link_pattern = re.compile(r"\[[^]]+\]\(([^)]+)\)")
    skill_files = sorted((ROOT / "skills").glob("*/SKILL.md"))
    valid_skills = {skill.parent.name for skill in skill_files}
    for skill in skill_files:
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
        try:
            load_route_cases(skill.parent, valid_skills)
        except ValueError as error:
            errors.append(str(error))
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
