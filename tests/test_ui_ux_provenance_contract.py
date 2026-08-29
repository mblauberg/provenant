from pathlib import Path

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


def test_ui_ux_notices_are_exact_and_removed_data_stays_historical_only():
    notices = (ROOT / "THIRD_PARTY_NOTICES.md").read_text()
    compact = " ".join(notices.split())
    assert "a39c7b7e6db2778467e43f5ed3a05143c05c07dd" in notices
    assert "does not retain a recoverable upstream Impeccable commit" in compact
    for commit in (
        "20e34c4a587e5eb09fcdf8351fa97b3ad761b31e",
        "d23d7f88a2e21c9e4b1418c7abe420f5c1052ba7",
        "aaf9a82f5efd73e87cc0998edc398e75bfc35901",
        "ca483852de23d48ab4f4ea71da37dad12bd70a95",
        "1c1e97cb9878e236552c772092dda7adcdddbcb2",
        "4c716b516b6b0143f3037631306b3730d2832344",
        "45313ce9f60971134704a17f7b1a64c30909c240",
        "792d6db7411839c62940a6e930161f8e376e817f",
        "539d52c424c18a14626601a0494ff70561b86d8b",
    ):
        assert commit in notices
    assert "research inputs, not redistributed components" in compact
    assert "were removed from the active package" in compact
    assert not list((SKILL / "data").glob("*.csv"))
    assert (ROOT / "LICENSES" / "ui-ux-pro-max-MIT.txt").is_file()
    assert (ROOT / "LICENSES" / "impeccable-APACHE-2.0.txt").is_file()
    assert (ROOT / "LICENSES" / "modern-screenshot-MIT.txt").is_file()
    assert not list(SKILL.rglob("*three*.js"))
    assert not list(SKILL.rglob("*gsap*.js"))

    acknowledgements = (ROOT / "ACKNOWLEDGEMENTS.md").read_text()
    assert "historical" in acknowledgements.lower()
    assert "539d52c424c18a14626601a0494ff70561b86d8b" in acknowledgements
    assert "also includes" not in acknowledgements


def test_modified_impeccable_sources_have_local_markers():
    marker = "Modified from Impeccable for this harness"
    ledger = yaml.safe_load((SKILL / "evals" / "provenance_components.yaml").read_text())
    component = next(item for item in ledger["components"] if item["id"] == "impeccable-modified-distribution")
    paths = [SKILL / relative for relative in component["marker_required_exact"]]
    for prefix in component["marker_required_prefixes"]:
        paths.extend(path for path in (SKILL / prefix).rglob("*") if path.is_file())
    for path in paths:
        assert marker in path.read_text()[:500], path


def test_harness_original_runtime_and_test_files_are_not_overattributed_to_impeccable():
    ledger = yaml.safe_load((SKILL / "evals" / "provenance_components.yaml").read_text())
    components = ledger["components"]
    for relative in (
        "scripts/contained-source.mjs",
        "scripts/live-server-startup.mjs",
        "tests/live-server-startup.test.mjs",
    ):
        assert _component_for(relative, components) == ["harness-evaluation-contracts"]


def test_notice_marker_claim_is_narrow_and_migration_ledger_is_labelled_as_evidence():
    notices = (ROOT / "THIRD_PARTY_NOTICES.md").read_text().lower()
    disposition = (SKILL / "evals" / "reference_disposition.yaml").read_text().lower()
    assert "modified by this consolidation" in notices
    assert "not a claim that every historical file" in notices
    assert "migration evidence" in disposition
    assert all(word in disposition for word in ("not permanent", "product", "doctrine"))
