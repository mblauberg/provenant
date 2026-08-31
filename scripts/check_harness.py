#!/usr/bin/env python3
"""Static checks for the global agent harness."""

from __future__ import annotations

import re
from pathlib import Path
import sys

import yaml


ROOT = Path(__file__).resolve().parents[1]
DESCRIPTION_LIMIT_CHARS = 1_024
MARKDOWN_LINK_PATTERN = re.compile(r"\[[^]]+\]\(([^)]+)\)")
ISSUE_FORM_TYPES = {"checkboxes", "dropdown", "input", "markdown", "textarea", "upload"}
ISSUE_FORM_TOP_LEVEL_KEYS = {
    "assignees", "body", "description", "labels", "name", "projects", "title", "type"
}
ISSUE_FORM_BODY_KEYS = {"attributes", "id", "type", "validations"}
ISSUE_FORM_ID_PATTERN = re.compile(r"^[A-Za-z0-9_-]+$")


def _display_path(path: Path) -> Path:
    try:
        return path.relative_to(ROOT)
    except ValueError:
        return path


def _markdown_anchors(path: Path) -> set[str]:
    anchors: set[str] = set()
    fence_character = ""
    fence_length = 0
    for line in path.read_text().splitlines():
        fence = re.match(r"^\s{0,3}(`{3,}|~{3,})", line)
        if fence:
            marker = fence.group(1)
            if not fence_character:
                fence_character, fence_length = marker[0], len(marker)
            elif marker[0] == fence_character and len(marker) >= fence_length:
                fence_character, fence_length = "", 0
            continue
        if fence_character:
            continue
        match = re.match(r"^#{1,6}\s+(.+?)\s*$", line)
        if not match:
            continue
        heading = match.group(1)
        plain = re.sub(r"<[^>]+>", "", heading).lower().strip()
        plain = re.sub(r"[^\w\- ]", "", plain)
        base = plain.replace(" ", "-")
        candidate = base
        suffix = 1
        while candidate in anchors:
            candidate = f"{base}-{suffix}"
            suffix += 1
        anchors.add(candidate)
    return anchors


def markdown_link_errors(paths: list[Path]) -> list[str]:
    """Validate local Markdown file and fragment targets."""

    errors: list[str] = []
    for source in paths:
        for target in MARKDOWN_LINK_PATTERN.findall(source.read_text()):
            if target.startswith(("http://", "https://", "/")):
                continue
            relative, separator, fragment = target.partition("#")
            destination = source.parent / relative if relative else source
            if relative and not destination.exists():
                errors.append(f"{_display_path(source)}: broken link {target}")
            elif separator and fragment and destination.suffix == ".md":
                if fragment not in _markdown_anchors(destination):
                    errors.append(f"{_display_path(source)}: broken link {target}")
    return errors


def issue_form_errors(paths: list[Path]) -> list[str]:
    """Validate the minimal structure GitHub requires from issue forms."""

    errors: list[str] = []
    for path in paths:
        display = _display_path(path)
        try:
            form = yaml.safe_load(path.read_text())
        except yaml.YAMLError as error:
            errors.append(f"{display}: invalid YAML: {error}")
            continue
        if not isinstance(form, dict):
            errors.append(f"{display}: issue form must be a mapping")
            continue
        unsupported = sorted(set(form) - ISSUE_FORM_TOP_LEVEL_KEYS)
        if unsupported:
            errors.append(f"{display}: unsupported top-level keys: {', '.join(unsupported)}")
        for field in ("name", "description"):
            if not isinstance(form.get(field), str) or not form[field].strip():
                errors.append(f"{display}: {field} is required")
        body = form.get("body")
        if not isinstance(body, list) or not body:
            errors.append(f"{display}: body must be a non-empty list")
            continue
        seen_ids: set[str] = set()
        input_items = 0
        for index, item in enumerate(body, start=1):
            if not isinstance(item, dict) or item.get("type") not in ISSUE_FORM_TYPES:
                errors.append(f"{display}: body item {index} has unsupported type")
                continue
            item_type = item["type"]
            unsupported = sorted(set(item) - ISSUE_FORM_BODY_KEYS)
            if unsupported:
                errors.append(
                    f"{display}: body item {index} has unsupported keys: {', '.join(unsupported)}"
                )
            if item_type != "markdown":
                input_items += 1
            attributes = item.get("attributes")
            if not isinstance(attributes, dict):
                errors.append(f"{display}: body item {index} requires attributes")
                continue
            if item_type == "markdown":
                if not isinstance(attributes.get("value"), str) or not attributes["value"].strip():
                    errors.append(f"{display}: markdown item {index} requires a value")
            else:
                item_id = item.get("id")
                if not isinstance(item_id, str) or not item_id.strip():
                    errors.append(f"{display}: body item {index} requires an id")
                else:
                    if ISSUE_FORM_ID_PATTERN.fullmatch(item_id) is None:
                        errors.append(f"{display}: body item {index} has an invalid id")
                    if item_id in seen_ids:
                        errors.append(f"{display}: body item {index} duplicates id {item_id}")
                    seen_ids.add(item_id)
                if not isinstance(attributes.get("label"), str) or not attributes["label"].strip():
                    errors.append(f"{display}: body item {index} requires a label")
            if item_type in {"checkboxes", "dropdown"}:
                options = attributes.get("options")
                if not isinstance(options, list) or not options:
                    errors.append(f"{display}: body item {index} requires options")
                elif item_type == "dropdown" and any(
                    not isinstance(option, str) or not option.strip() for option in options
                ):
                    errors.append(
                        f"{display}: dropdown item {index} requires non-empty string options"
                    )
                elif item_type == "dropdown" and len(options) != len(set(options)):
                    errors.append(f"{display}: dropdown item {index} requires unique options")
                elif item_type == "checkboxes" and any(
                    not isinstance(option, dict)
                    or not isinstance(option.get("label"), str)
                    or not option["label"].strip()
                    for option in options
                ):
                    errors.append(f"{display}: checkbox item {index} requires options with labels")
                elif item_type == "checkboxes":
                    labels = [option["label"] for option in options]
                    if len(labels) != len(set(labels)):
                        errors.append(
                            f"{display}: checkbox item {index} requires unique option labels"
                        )
        if input_items == 0:
            errors.append(f"{display}: body must contain at least one input item")
    return errors


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
        for target in MARKDOWN_LINK_PATTERN.findall(text):
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
    documentation = sorted((ROOT / "docs").rglob("*.md"))
    issue_forms = sorted((ROOT / ".github" / "ISSUE_TEMPLATE").glob("*.yml"))
    issue_forms += sorted((ROOT / "skills" / "setup-repo" / "templates" / "ISSUE_TEMPLATE").glob("*.yml"))
    issue_forms = [path for path in issue_forms if path.name != "config.yml"]
    errors = (
        skill_errors()
        + markdown_link_errors(documentation)
        + issue_form_errors(issue_forms)
        + openai_sidecar_errors()
    )
    if errors:
        for error in errors:
            print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print("PASS: frontmatter, fixtures, links, issue forms and sidecars clean")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
