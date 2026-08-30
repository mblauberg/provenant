import hashlib
from pathlib import Path
import subprocess

import yaml


ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "skills" / "ui-ux-design"


def _component_for(relative: str, components: list[dict]) -> list[str]:
    owners = []
    for component in components:
        exact = set(component.get("exact", []))
        prefixes = tuple(component.get("prefixes", []))
        excluded = set(component.get("exclude", []))
        if relative not in excluded and (relative in exact or relative.startswith(prefixes)):
            owners.append(component["id"])
    return owners


def test_every_shipped_skill_file_has_exactly_one_provenance_component():
    ledger = yaml.safe_load((SKILL / "evals" / "provenance_components.yaml").read_text())
    components = ledger["components"]
    files = [
        path.relative_to(SKILL).as_posix()
        for path in SKILL.rglob("*")
        if path.is_file() and "__pycache__" not in path.parts and path.suffix != ".pyc"
    ]
    for relative in files:
        owners = _component_for(relative, components)
        assert len(owners) == 1, (relative, owners)

    for component in components:
        assert component["origin"] in {"third-party", "harness-original"}
        if component["origin"] == "third-party":
            for key in ("source_url", "source_ref", "licence", "local_licence", "modification"):
                assert component[key]

        for relative in component.get("exact", []):
            assert (SKILL / relative).is_file(), (component["id"], relative)
        for relative in component.get("exclude", []):
            assert (SKILL / relative).is_file(), (component["id"], relative)
            assert _component_for(relative, components), relative


def test_unmodified_modern_screenshot_bundle_matches_its_pinned_digest():
    ledger = yaml.safe_load((SKILL / "evals" / "provenance_components.yaml").read_text())
    component = next(
        item for item in ledger["components"] if item["id"] == "modern-screenshot-runtime"
    )
    bundle = SKILL / component["exact"][0]

    assert hashlib.sha256(bundle.read_bytes()).hexdigest() == component["sha256"]


def test_third_party_components_resolve_to_notices_and_local_licences():
    ledger = yaml.safe_load((SKILL / "evals" / "provenance_components.yaml").read_text())
    notices = (ROOT / "THIRD_PARTY_NOTICES.md").read_text()
    for component in ledger["components"]:
        if component["origin"] != "third-party":
            continue
        assert (ROOT / component["local_licence"]).is_file()
        assert component["source_url"].split("/tree/")[0] in notices
        revisions = [
            word.strip('".,;()')
            for word in component["source_ref"].split()
            if len(word.strip('".,;()')) == 40
        ]
        assert not revisions or all(revision in notices for revision in revisions)


def test_modified_impeccable_sources_have_local_modification_notices():
    ledger = yaml.safe_load((SKILL / "evals" / "provenance_components.yaml").read_text())
    component = next(item for item in ledger["components"] if item["id"] == "impeccable-modified-distribution")
    modified = subprocess.run(
        [
            "git",
            "diff",
            "--name-only",
            component["modification_baseline"],
            "--",
            "skills/ui-ux-design",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    modified = {
        Path(relative).relative_to("skills/ui-ux-design").as_posix()
        for relative in modified
        if (ROOT / relative).is_file()
    }
    derived_modified = {
        relative
        for relative in modified
        if _component_for(relative, ledger["components"])
        == ["impeccable-modified-distribution"]
    }
    marker_exact = set(component["marker_required_exact"])
    marker_prefixes = tuple(component["marker_required_prefixes"])
    covered = {
        relative for relative in derived_modified
        if relative in marker_exact or relative.startswith(marker_prefixes)
    }
    assert covered == derived_modified
    for relative in derived_modified:
        assert "Modified for Provenant" in (SKILL / relative).read_text()[:500], relative


def test_harness_original_runtime_and_test_files_are_not_overattributed_to_impeccable():
    ledger = yaml.safe_load((SKILL / "evals" / "provenance_components.yaml").read_text())
    components = ledger["components"]
    for relative in (
        "SKILL.md",
        "scripts/contained-source.mjs",
        "scripts/jsx-tag-scanner.mjs",
        "scripts/live-server-startup.mjs",
        "tests/live-server-startup.test.mjs",
    ):
        assert _component_for(relative, components) == ["harness-original-material"]
