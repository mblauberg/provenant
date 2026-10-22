"""The model benchmark registry stays well formed and tied to the catalogue."""

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DIR = ROOT / "docs" / "research" / "model-benchmarks"
DATA = json.loads((DIR / "benchmarks.json").read_text())
SCHEMA = json.loads((DIR / "benchmarks.schema.json").read_text())
CATALOGUE = json.loads((ROOT / "config" / "model-routing.json").read_text())
EFFORTS = {"none", "low", "medium", "high", "xhigh", "max", "ultra", "thinking"}


def catalogue_ids():
    ids = {m["id"] for a in CATALOGUE["adapters"].values() for m in a.get("models", [])}
    for family in CATALOGUE["families"].values():
        ids |= set(family.get("per_model_efforts", {}))
    return ids


def check(value, spec, path):
    """The subset of JSON Schema the registry schema uses."""
    spec = {**SCHEMA["$defs"][spec["$ref"].rsplit("/", 1)[1]]} if "$ref" in spec else spec
    if "const" in spec:
        assert value == spec["const"], path
    if "enum" in spec:
        assert value in spec["enum"], path
    kinds = spec.get("type")
    if kinds:
        names = {"string": str, "number": (int, float), "boolean": bool, "array": list, "object": dict, "null": type(None)}
        assert isinstance(value, tuple(t for k in ([kinds] if isinstance(kinds, str) else kinds) for t in (names[k] if isinstance(names[k], tuple) else (names[k],)))), path
    if isinstance(value, str) and "pattern" in spec:
        assert re.search(spec["pattern"], value), path
    if isinstance(value, dict):
        for key in spec.get("required", []):
            assert key in value, f"{path}.{key} missing"
        props = spec.get("properties", {})
        if spec.get("additionalProperties") is False:
            assert set(value) <= set(props), f"{path}: unknown {set(value) - set(props)}"
        for key, sub in props.items():
            if key in value:
                check(value[key], sub, f"{path}.{key}")
    if isinstance(value, list) and "items" in spec:
        for i, item in enumerate(value):
            check(item, spec["items"], f"{path}[{i}]")


def test_registry_matches_schema():
    check(DATA, SCHEMA, "$")


def test_record_ids_are_unique():
    ids = [r["id"] for r in DATA["records"]]
    assert len(ids) == len(set(ids))


def test_efforts_use_the_known_vocabulary():
    assert {r["effort"] for r in DATA["records"]} - EFFORTS - {None} == set()


def test_catalogue_flag_matches_the_catalogue():
    known = catalogue_ids()
    for r in DATA["records"]:
        assert (r["model_id"] in known) == r["catalogue"], r["id"]
        for other in r.get("also_applies_to", []):
            assert other in known, (r["id"], other)
    for g in DATA["gaps"]:
        assert g["model_id"] == "*" or g["model_id"] in known, g["model_id"]
