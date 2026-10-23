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


def test_every_shipped_provenance_file_has_exactly_one_component():
    directory = SKILL
    ledger_path = SKILL / "evals" / "provenance_components.yaml"
    components = yaml.safe_load(ledger_path.read_text())["components"]
    files = [
        path.relative_to(directory).as_posix()
        for path in directory.rglob("*")
        if path.is_file() and "__pycache__" not in path.parts and path.suffix != ".pyc"
    ]

    for relative in files:
        owners = _component_for(relative, components)
        assert len(owners) == 1, (relative, owners)


def test_skill_provenance_component_metadata_and_paths():
    ledger = yaml.safe_load((SKILL / "evals" / "provenance_components.yaml").read_text())
    components = ledger["components"]
    assert _component_for("SKILL.md", components) == ["harness-original-material"]
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


def test_third_party_components_resolve_to_notices_and_local_licences():
    notices = (ROOT / "THIRD_PARTY_NOTICES.md").read_text()
    ledgers = (
        SKILL / "evals" / "provenance_components.yaml",
    )
    for ledger_path in ledgers:
        ledger = yaml.safe_load(ledger_path.read_text())
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
