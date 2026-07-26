from pathlib import Path
import posixpath
import runpy
import re

import pytest
import yaml


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "tests" / "fixtures" / "disclosure-migration.yaml"
SPEC = ROOT / "docs" / "specs" / "harness" / "disclosure-refactor.md"


# A skill segment is either a static skill name or a dynamic placeholder that
# resolves at runtime (${SKILL}, $skill, $1, {skill}, %s). The classifier fails
# closed on every placeholder. A shell glob such as skills/*/references/
# enumerates every skill uniformly and names no specific skill's internals, so it
# is deliberately not matched here and stays allowed (tooling and prose use it).
SKILL_SEGMENT = (
    r"(?:[a-z0-9]+(?:-[a-z0-9]+)*"
    r"|\$\{[^}/\n]+\}|\$[a-z0-9_]+|\{[^}/\n]+\}|%[0-9]*[a-z])"
)
STATIC_SKILL_NAME = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*", re.IGNORECASE)
PATH_TOKEN = r"[^\s`'\"()\[\]<>]+"

REFERENCE_CANDIDATES = (
    re.compile(
        rf"(?P<path>(?<![a-z0-9_-])skills/{PATH_TOKEN})",
        re.IGNORECASE,
    ),
    re.compile(
        rf"(?P<path>(?:\.\./)+{PATH_TOKEN})",
        re.IGNORECASE,
    ),
    re.compile(
        rf"(?P<path>(?<![a-z0-9_./-])references?/{PATH_TOKEN})",
        re.IGNORECASE,
    ),
)

REFERENCE_PATHS = (
    re.compile(
        rf"(?P<path>skills/(?P<skill>{SKILL_SEGMENT})/references?(?:/{PATH_TOKEN})?)",
        re.IGNORECASE,
    ),
    re.compile(
        rf"(?P<path>(?:\.\./)+(?P<skill>{SKILL_SEGMENT})/references?(?:/{PATH_TOKEN})?)",
        re.IGNORECASE,
    ),
    re.compile(rf"(?P<path>references?(?:/{PATH_TOKEN})?)", re.IGNORECASE),
)


def _in_tree_contract_files():
    roots = [ROOT / "skills", ROOT / "scripts", ROOT / "tests" / "fixtures"]
    workflows = ROOT / "workflows"
    if workflows.is_dir():
        roots.append(workflows)
    for root in roots:
        if root.is_dir():
            yield from (path for path in root.rglob("*") if path.is_file())
    yield ROOT / "AGENTS.md"
    yield ROOT / "HARNESS.md"


def _cross_skill_reference_violations(relative, text):
    owner = relative.parts[1] if relative.parts[:1] == ("skills",) else None
    violations = []
    seen = set()
    for candidate_pattern in REFERENCE_CANDIDATES:
        for candidate in candidate_pattern.finditer(text):
            raw_path = candidate.group("path")
            normalized = posixpath.normpath(raw_path)
            for pattern in REFERENCE_PATHS:
                match = pattern.fullmatch(normalized)
                if match is None:
                    continue
                skill = match.groupdict().get("skill")
                is_dynamic = skill is not None and STATIC_SKILL_NAME.fullmatch(skill) is None
                is_cross_skill = owner is None if skill is None else skill.casefold() != owner
                if is_dynamic or is_cross_skill:
                    violation = f"{relative}: {raw_path}"
                    if violation not in seen:
                        seen.add(violation)
                        violations.append(violation)
                break
    return violations


def _markdown_table_rows(header):
    lines = SPEC.read_text(encoding="utf-8").splitlines()
    start = next(index for index, line in enumerate(lines) if line.startswith(header))
    rows = []
    for line in lines[start + 2 :]:
        if not line.startswith("|"):
            break
        rows.append(tuple(cell.strip() for cell in line.strip().strip("|").split("|")))
    return rows


def _approved_manifest_rows():
    ambient = [
        {"section": section, "disposition": disposition, "destination": destination}
        for section, disposition, destination in _markdown_table_rows("| Source section |")
    ]

    orchestrate = []
    for filename, disposition, notes in _markdown_table_rows("| File | Verdict |"):
        if disposition == "keep":
            verdict = "keep"
        elif disposition.startswith("slim (") and disposition.endswith(")"):
            verdict = "slim"
            notes = f"{disposition.removeprefix('slim (')[:-1]}; {notes}"
        elif disposition == "merge into verification.md, then delete":
            verdict = "merge-then-delete"
            notes = f"{disposition}; {notes}"
        elif disposition.startswith("archive to "):
            verdict = "archive"
            notes = f"{disposition}; {notes}"
        else:
            raise AssertionError(f"unrecognised approved orchestrate disposition: {disposition}")
        orchestrate.append({"file": filename, "verdict": verdict, "notes": notes})
    return ambient, orchestrate


def test_cross_skill_reference_paths_are_private_to_the_owning_skill():
    """AC-S2: in-tree consumers name a skill, never another skill's internals."""
    violations = []
    for path in _in_tree_contract_files():
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        relative = path.relative_to(ROOT)
        violations.extend(_cross_skill_reference_violations(relative, text))

    assert not violations, "cross-skill reference path(s):\n" + "\n".join(violations)


@pytest.mark.parametrize(
    "source, reference, expected_violation",
    (
        ("workflows/cross-verify.js", "Skills/implement/References/run-contract.md", True),
        ("scripts/check-harness", "`skills/implement/references/run-contract.md`", True),
        ("skills/orchestrate/SKILL.md", "../../implement/references/run-contract.md", True),
        ("skills/orchestrate/SKILL.md", ".././implement/references/run-contract.md", True),
        (
            "workflows/cross-verify.js",
            "skills/implement/scripts/../references/run-contract.md",
            True,
        ),
        ("workflows/cross-verify.js", "skills/./implement/references/run-contract.md", True),
        ("scripts/check-harness", "skills/implement/references/", True),
        ("workflows/implement-run.js", "skills/${SKILL}/references/private.md", True),
        ("workflows/implement-run.js", "skills/$SKILL/references/private.md", True),
        ("workflows/implement-run.js", "skills/{skill}/references/private.md", True),
        ("workflows/implement-run.js", "skills/%s/references/private.md", True),
        ("workflows/implement-run.js", "skills/$1/references/private.md", True),
        ("workflows/implement-run.js", "references/run-contract.md", True),
        ("skills/implement/SKILL.md", "skills/IMPLEMENT/references/run-contract.md", False),
        ("skills/implement/SKILL.md", "references/run-contract.md", False),
    ),
)
def test_cross_skill_reference_red_team_cases(source, reference, expected_violation):
    """D3 catches path forms that resolve cross-skill without banning own references."""
    violations = _cross_skill_reference_violations(Path(source), reference)
    assert bool(violations) is expected_violation


def test_cross_skill_reference_scan_roots_are_non_vacuous():
    relative_files = {path.relative_to(ROOT) for path in _in_tree_contract_files()}
    assert {"skills", "scripts", "workflows"} <= {
        relative.parts[0] for relative in relative_files
    }
    assert any(relative.parts[:2] == ("tests", "fixtures") for relative in relative_files)
    assert {Path("AGENTS.md"), Path("HARNESS.md")} <= relative_files


def test_disclosure_migration_manifest_is_complete_and_anchored():
    """AC-S3/S4: the manifest is complete and its migrations are active."""
    manifest = yaml.safe_load(MANIFEST.read_text(encoding="utf-8"))
    assert set(manifest) == {"schema", "ambient", "orchestrate"}
    assert manifest["schema"] == "disclosure-migration.v1"

    ambient = manifest["ambient"]
    approved_ambient, approved_orchestrate = _approved_manifest_rows()
    assert ambient == approved_ambient
    assert len(ambient) == 12
    assert all(set(row) == {"section", "disposition", "destination"} for row in ambient)
    assert all(all(isinstance(value, str) and value for value in row.values()) for row in ambient)
    sections = [row["section"] for row in ambient]
    assert len(sections) == len(set(sections)), "ambient manifest has duplicate sections"

    stripped = [row for row in ambient if row["disposition"].startswith("strip")]
    skill_owners = []
    for row in ambient:
        destination = row["destination"]
        if "repo-surface" in destination:
            anchors = re.findall(r"[A-Za-z0-9_.-]+\.md", destination)
            assert anchors, f"repo-surface row has no named anchor: {row}"
            for anchor in anchors:
                assert (ROOT / anchor).is_file(), f"missing repo-surface anchor {anchor}: {row}"
            continue

        owners = re.findall(r"`([a-z0-9]+(?:-[a-z0-9]+)*)`", destination)
        for owner in owners:
            assert (ROOT / "skills" / owner / "SKILL.md").is_file(), (
                f"missing destination skill anchor skills/{owner}/SKILL.md: {row}"
            )
        if row in stripped:
            assert len(owners) == 1, f"stripped skill row has no canonical owner: {row}"
            skill_owners.append(owners[0])

    assert len(skill_owners) == len(set(skill_owners)), (
        f"duplicate canonical owner(s) for stripped rows: {skill_owners}"
    )

    orchestrate = manifest["orchestrate"]
    assert orchestrate == approved_orchestrate
    assert len(orchestrate) == 18
    assert all(set(row) == {"file", "verdict", "notes"} for row in orchestrate)
    assert all(all(isinstance(value, str) and value for value in row.values()) for row in orchestrate)
    files = [row["file"] for row in orchestrate]
    assert len(files) == len(set(files)), "orchestrate manifest has duplicate files"
    assert {row["verdict"] for row in orchestrate} <= {
        "keep",
        "slim",
        "archive",
        "merge-then-delete",
    }
    retained = {
        row["file"] for row in orchestrate if row["verdict"] in {"keep", "slim"}
    }
    archived = [row["file"] for row in orchestrate if row["verdict"] == "archive"]
    merged = [
        row["file"] for row in orchestrate if row["verdict"] == "merge-then-delete"
    ]
    assert len(retained) == 16
    assert len(archived) == len(merged) == 1

    orchestrate_root = ROOT / "skills" / "orchestrate"
    reference_dir = orchestrate_root / "references"
    actual = {path.name for path in reference_dir.glob("*.md")}
    assert actual == retained

    skill = (orchestrate_root / "SKILL.md").read_text(encoding="utf-8")
    reference_loader = skill.split("## References", 1)[1].split(
        "## Adapter-absent path", 1
    )[0]
    loader_entries = set(re.findall(r"`([^`]+\.md)`", reference_loader))
    assert loader_entries == retained

    research = ROOT / "docs" / "research"
    research_index = (research / "README.md").read_text(encoding="utf-8")
    archived_name = archived[0]
    assert not (reference_dir / archived_name).exists()
    assert (research / archived_name).is_file()
    assert re.search(
        rf"\]\({re.escape(archived_name)}\)\n\s+: [^\n]*normative owner[^\n]*`orchestrate`",
        research_index,
        re.IGNORECASE,
    )

    merged_name = merged[0]
    assert not (reference_dir / merged_name).exists()
    verification = (reference_dir / "verification.md").read_text(encoding="utf-8")
    for invariant in (
        "Do not treat panel agreement as ground truth",
        "Use voting only for low-stakes or objective candidate filtering",
        "supported`, `contradicted`, or `needs-evidence",
        "They do not score prose quality or vote on truth",
        "Do not let a reviewer judge its own authored surface",
        "Use a fresh-context reducer for crucial decisions",
        "A council adds pressure, not authority",
    ):
        assert invariant in verification
    assert merged_name not in verification

    contract_cases = yaml.safe_load(
        (orchestrate_root / "evals" / "contract_cases.yaml").read_text(
            encoding="utf-8"
        )
    )
    reference_invariants = set(contract_cases["reference_invariants"])
    assert merged_name not in reference_invariants
    assert "Claude-only Workflow adapter" in reference_invariants
    assert "saved-workflow conventions" in reference_invariants

    checker = runpy.run_path(
        str(orchestrate_root / "evals" / "check_skill_triggers.py")
    )
    assert checker["REQUIRED_REFS"] == retained

    dynamic = (reference_dir / "dynamic-workflows.md").read_text(encoding="utf-8")
    assert "orchestration-contract.md" in dynamic
    assert "saved-workflow conventions" in dynamic
    assert "memory-scratchpad.md" in dynamic
    assert all(
        duplicated not in dynamic
        for duplicated in ("MANIFEST.md", "RUN_RECEIPT.json", "FINAL_GATE.md")
    )

    routing = (reference_dir / "routing-and-tiers.md").read_text(encoding="utf-8")
    assert "Never route by a memorised model name" in routing
    assert "hard-code a dated model ID" not in dynamic

    paired = (reference_dir / "paired-primary.md").read_text(encoding="utf-8")
    assert "[herdr-panes.md](herdr-panes.md)" in paired
    assert "Fabric carries answer-bearing" not in paired

    retrieval = (reference_dir / "retrieval-and-tool-routing.md").read_text(
        encoding="utf-8"
    )
    assert "(orchestration-contract.md#worker-contract)" in retrieval
    assert "source-scope:" not in retrieval

    cli = (reference_dir / "cli-headless.md").read_text(encoding="utf-8")
    runtime_routing = cli.split("## Runtime routing", 1)[1].split(
        "## Output normalisation", 1
    )[0]
    assert "[routing-and-tiers.md](routing-and-tiers.md)" in runtime_routing
    assert "hard-code a dated model ID" not in runtime_routing


@pytest.mark.parametrize(
    "mutation, message",
    (
        (
            lambda manifest: manifest["orchestrate"][0].update(verdict="kepe"),
            "invalid verdict",
        ),
        (
            lambda manifest: manifest["orchestrate"][1].update(
                file=manifest["orchestrate"][0]["file"]
            ),
            "duplicate filenames",
        ),
        (
            lambda manifest: manifest["orchestrate"].pop(),
            "must have 18 orchestrate rows",
        ),
    ),
)
def test_orchestrate_required_refs_reject_malformed_manifest(
    tmp_path, mutation, message
):
    manifest = yaml.safe_load(MANIFEST.read_text(encoding="utf-8"))
    mutation(manifest)
    malformed = tmp_path / "disclosure-migration.yaml"
    malformed.write_text(yaml.safe_dump(manifest), encoding="utf-8")
    checker = runpy.run_path(
        str(ROOT / "skills" / "orchestrate" / "evals" / "check_skill_triggers.py")
    )

    with pytest.raises(ValueError, match=message):
        checker["required_refs_from_manifest"](malformed)


def test_orchestrate_required_refs_reject_unreadable_manifest(tmp_path):
    checker = runpy.run_path(
        str(ROOT / "skills" / "orchestrate" / "evals" / "check_skill_triggers.py")
    )

    with pytest.raises(ValueError, match="manifest is unreadable"):
        checker["required_refs_from_manifest"](tmp_path / "missing.yaml")
