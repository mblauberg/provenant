#!/usr/bin/env python3
"""Static trigger/doctrine guard for orchestrate.

This is not a live behaviour eval. It checks that the skill remains routable,
bounded, progressively disclosed, and backed by the expected reference files.
"""
import argparse
import os
import re
import subprocess
import sys

import yaml

try:
    from scripts.count_skill_words import count_skill_words
except ModuleNotFoundError:  # Direct execution from the skill directory.
    sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "../../..")))
    from scripts.count_skill_words import count_skill_words

HERE = os.path.dirname(os.path.abspath(__file__))
SKILL_DIR = os.path.dirname(HERE)
SKILL_MD = os.path.join(SKILL_DIR, "SKILL.md")
REF_DIR = os.path.join(SKILL_DIR, "references")
SCRIPT_DIR = os.path.join(SKILL_DIR, "scripts")
CASES = os.path.join(HERE, "contract_cases.yaml")
TOPOLOGY_CASES = os.path.join(HERE, "topology_value_cases.yaml")
# This separate router-role ceiling uses the canonical counter; it is not the
# 500-word skill-entrypoint budget enforced by check_harness.py.
ROUTER_ROLE_WORD_LIMIT = 1250
MANIFEST = os.path.join(
    SKILL_DIR, "..", "..", "tests", "fixtures", "disclosure-migration.yaml"
)

STOP = set(
    "the a an of to and or for with this that in on is be use when it as your you "
    "from into not no never any all each only out off by if".split()
)
PRIMARY_TRIGGER_TERMS = {
    "subagents", "subagent", "independent", "second", "cross-family", "red-team",
    "parallel", "high-stakes", "long-running",
}
REQUIRED_SECTIONS = [
    "## Overview",
    "## Rules",
    "## When This Pays",
    "## Adaptive Loop",
    "## Worker Contract",
    "## References",
]


def required_refs_from_manifest(path=MANIFEST):
    try:
        with open(path, encoding="utf-8") as stream:
            raw = yaml.safe_load(stream)
    except (OSError, yaml.YAMLError) as exc:
        raise ValueError(f"disclosure migration manifest is unreadable: {exc}") from exc
    if not isinstance(raw, dict) or raw.get("schema") != "disclosure-migration.v1":
        raise ValueError("disclosure migration manifest identity is invalid")
    rows = raw.get("orchestrate")
    if not isinstance(rows, list) or len(rows) != 18:
        raise ValueError("disclosure migration manifest must have 18 orchestrate rows")
    required = set()
    filenames = set()
    verdicts = {"keep", "slim", "archive", "merge-then-delete"}
    for row in rows:
        if not isinstance(row, dict) or set(row) != {"file", "verdict", "notes"}:
            raise ValueError("disclosure migration manifest has invalid orchestrate rows")
        if any(not isinstance(value, str) or not value for value in row.values()):
            raise ValueError("disclosure migration manifest has invalid orchestrate rows")
        if row["verdict"] not in verdicts:
            raise ValueError("disclosure migration manifest has an invalid verdict")
        if row["file"] in filenames:
            raise ValueError("disclosure migration manifest has duplicate filenames")
        filenames.add(row["file"])
        if row["verdict"] in {"keep", "slim"}:
            required.add(row["file"])
    if not required:
        raise ValueError("disclosure migration manifest has invalid retained references")
    return required


try:
    REQUIRED_REFS = required_refs_from_manifest()
    REQUIRED_REFS_ERROR = ""
except ValueError as exc:
    REQUIRED_REFS = set()
    REQUIRED_REFS_ERROR = str(exc)
CASE_SCHEMA_VERSION = 1
CASE_GROUP_MINIMUMS = {
    "doctrine_invariants": 10,
    "reference_invariants": 10,
}
TOPOLOGY_FACTOR_KEYS = {
    "independent_information",
    "stable_interfaces",
    "non_overlapping_writes",
    "independently_checkable_returns",
    "expected_information_gain",
    "coordination_shared_state_tool_density_cost",
}


def parse_cases(path):
    try:
        raw = yaml.safe_load(open(path, encoding="utf-8"))
    except (OSError, yaml.YAMLError) as exc:
        raise ValueError(f"contract cases are unreadable: {exc}") from exc
    if not isinstance(raw, dict):
        raise ValueError("contract cases root must be a mapping")
    expected_keys = {"schema_version", *CASE_GROUP_MINIMUMS}
    if set(raw) != expected_keys:
        raise ValueError(
            "contract cases require exactly: " + ", ".join(sorted(expected_keys))
        )
    if raw.get("schema_version") != CASE_SCHEMA_VERSION:
        raise ValueError(f"contract cases schema_version must be {CASE_SCHEMA_VERSION}")
    out = {}
    for group, minimum in CASE_GROUP_MINIMUMS.items():
        values = raw.get(group)
        if not isinstance(values, list) or len(values) < minimum:
            raise ValueError(f"{group} requires at least {minimum} cases")
        if any(not isinstance(value, str) or not value.strip() for value in values):
            raise ValueError(f"{group} cases must be non-empty strings")
        normalized = [value.strip() for value in values]
        if len(normalized) != len(set(normalized)):
            raise ValueError(f"{group} cases must be unique")
        out[group] = [{"prompt": value, "why": ""} for value in normalized]
    return out


def parse_topology_cases(path):
    try:
        raw = yaml.safe_load(open(path, encoding="utf-8"))
    except (OSError, yaml.YAMLError) as exc:
        raise ValueError(f"topology value cases are unreadable: {exc}") from exc
    if not isinstance(raw, dict) or set(raw) != {
        "schema_version", "target_skill", "cases",
    }:
        raise ValueError(
            "topology value cases require exactly schema_version, target_skill and cases"
        )
    if raw["schema_version"] != 1 or raw["target_skill"] != "orchestrate":
        raise ValueError("topology value case identity is invalid")
    cases = raw["cases"]
    if not isinstance(cases, list) or len(cases) < 9:
        raise ValueError("topology value cases require at least nine cases")

    ids = set()
    prompts = set()
    outcomes = {"parallel": 0, "serial": 0}
    tags = set()
    isolators = {"isolates-value-gate": 0, "isolates-structural-gate": 0}
    specialist_regressions = 0
    bool_factors = TOPOLOGY_FACTOR_KEYS - {
        "expected_information_gain",
        "coordination_shared_state_tool_density_cost",
    }
    for case in cases:
        if not isinstance(case, dict) or set(case) != {
            "id", "prompt", "tags", "factors", "expected_topology",
        }:
            raise ValueError("topology value case keys are invalid")
        case_id = case["id"]
        prompt = case["prompt"]
        if (
            not isinstance(case_id, str) or not case_id.startswith("topology-")
            or case_id in ids
            or not isinstance(prompt, str) or not prompt.strip() or prompt in prompts
        ):
            raise ValueError("topology value case id or prompt is invalid")
        ids.add(case_id)
        prompts.add(prompt)
        if (
            not isinstance(case["tags"], list) or not case["tags"]
            or any(not isinstance(tag, str) or not tag for tag in case["tags"])
        ):
            raise ValueError("topology value case tags are invalid")
        tags.update(case["tags"])

        factors = case["factors"]
        if not isinstance(factors, dict) or set(factors) != TOPOLOGY_FACTOR_KEYS:
            raise ValueError("topology value case factors are invalid")
        if any(type(factors[key]) is not bool for key in bool_factors):
            raise ValueError("topology structural factors must be booleans")
        gain = factors["expected_information_gain"]
        cost = factors["coordination_shared_state_tool_density_cost"]
        if any(type(value) is not int or not 0 <= value <= 3 for value in (gain, cost)):
            raise ValueError("topology value factors must be integers from zero to three")
        structural_gate = all(factors[key] for key in bool_factors)
        value_gate = gain > cost
        expected = "parallel" if structural_gate and value_gate else "serial"
        if case["expected_topology"] != expected:
            raise ValueError(
                f"{case_id} expected_topology violates the decomposition/value gate"
            )
        outcomes[expected] += 1
        if "isolates-value-gate" in case["tags"]:
            if not structural_gate or value_gate or expected != "serial":
                raise ValueError("value-gate isolator does not isolate the value conjunct")
            isolators["isolates-value-gate"] += 1
        if "isolates-structural-gate" in case["tags"]:
            if structural_gate or not value_gate or expected != "serial":
                raise ValueError(
                    "structural-gate isolator does not isolate the structural conjunct"
                )
            isolators["isolates-structural-gate"] += 1
        if "single-specialist-regression" in case["tags"]:
            if (
                "failing-parallel-gate" not in case["tags"]
                or (structural_gate and value_gate)
                or expected != "serial"
                or "one specialist" not in prompt.lower()
            ):
                raise ValueError(
                    "single-specialist regression must fail the parallel gate and stay serial"
                )
            specialist_regressions += 1

    required_tags = {
        "decomposable", "bounded", "tightly-coupled", "shared-error",
        "overlapping-writes", "tool-density", "isolates-value-gate",
        "isolates-structural-gate", "failing-parallel-gate",
        "single-specialist-regression",
    }
    if (
        outcomes["parallel"] < 2 or outcomes["serial"] < 7
        or not required_tags <= tags
        or any(count != 1 for count in isolators.values())
        or specialist_regressions != 1
    ):
        raise ValueError("topology value cases lack required parallel/serial boundary coverage")
    return cases


def token_norm(w):
    aliases = {"verification": "verify", "verifier": "verify", "verified": "verify"}
    w = aliases.get(w, w)
    if len(w) > 4 and w.endswith("s"):
        w = w[:-1]
    return w


def tokens(s):
    return {token_norm(w) for w in re.findall(r"[a-z0-9]+", s.lower())
            if w not in STOP and len(w) > 2}


def norm(s):
    return re.sub(r"\s+", " ", s).lower()


def frontmatter(text):
    m = re.search(r"^---\s*\n(.*?)\n---\s*\n", text, re.S)
    return m.group(1) if m else ""


def description_from_frontmatter(fm):
    m = re.search(r"^description:\s*(?:>\s*)?\n?(.*?)(?:\n[a-zA-Z_]+:|\Z)", fm, re.S | re.M)
    if not m:
        return ""
    return re.sub(r"\s+", " ", m.group(1)).strip().strip('"')


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("--cases", default=CASES)
    parser.add_argument("--topology-cases", default=TOPOLOGY_CASES)
    args = parser.parse_args(argv)
    fails = [REQUIRED_REFS_ERROR] if REQUIRED_REFS_ERROR else []
    if not os.path.exists(SKILL_MD):
        print("FAIL: SKILL.md missing")
        return 1

    text = open(SKILL_MD, encoding="utf-8").read()
    fm = frontmatter(text)
    desc = description_from_frontmatter(fm)
    body = text[len(fm):] if fm else text
    try:
        cases = parse_cases(args.cases)
    except ValueError as exc:
        cases = {group: [] for group in CASE_GROUP_MINIMUMS}
        fails.append(str(exc))
    try:
        topology_cases = parse_topology_cases(args.topology_cases)
    except ValueError as exc:
        topology_cases = []
        fails.append(str(exc))

    if not fm:
        fails.append("no YAML frontmatter")
    else:
        fm_keys = [line.split(":", 1)[0].strip() for line in fm.splitlines()
                   if re.match(r"^[A-Za-z_]+:", line)]
        if sorted(fm_keys) != ["description", "name"]:
            fails.append(f"frontmatter keys must be only name+description, got {fm_keys}")
        if len(fm) > 1024:
            fails.append(f"frontmatter too long ({len(fm)} chars)")
        if ": " in desc and not re.search(r'description:\s*"', fm) and "description: >" not in fm:
            fails.append("description contains ': ' but is not quoted or folded")

    if not re.search(r"^name:\s*orchestrate\s*$", fm, re.M):
        fails.append("name missing or wrong")
    if not desc.lower().startswith("use when"):
        fails.append("description should start with 'Use when'")
    if "audit this" in desc.lower() or "be thorough" in desc.lower():
        fails.append("description contains overbroad trigger phrase")

    first_250 = desc[:250].lower()
    if not (PRIMARY_TRIGGER_TERMS & tokens(first_250)):
        fails.append("first 250 description chars lack primary trigger terms")

    word_count = count_skill_words(text)
    if word_count > ROUTER_ROLE_WORD_LIMIT:
        fails.append(f"SKILL.md too long for router role ({word_count} words)")

    for section in REQUIRED_SECTIONS:
        if section not in text:
            fails.append(f"missing required section: {section}")

    for script in ("cf_dispatch.sh", "run_dir_init.sh"):
        path = os.path.join(SCRIPT_DIR, script)
        if not os.path.exists(path):
            fails.append(f"missing script: {script}")
            continue
        result = subprocess.run(["bash", "-n", path], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        if result.returncode != 0:
            fails.append(f"script does not parse: {script}: {result.stderr.strip()}")

    # Prompt/description token overlap is not routing ground truth. Balanced
    # Canonical routing fixtures and repeated blind model trials own selection evidence;
    # this checker retains only deterministic doctrine/reference contracts.

    ntext = norm(text)
    for inv in cases.get("doctrine_invariants", []):
        if norm(inv["prompt"]) not in ntext:
            fails.append(f"SKILL.md missing doctrine invariant: {inv['prompt']!r}")

    refblob = ""
    for required in sorted(REQUIRED_REFS):
        if not os.path.exists(os.path.join(REF_DIR, required)):
            fails.append(f"missing required reference file: {required}")
    if os.path.isdir(REF_DIR):
        for fn in os.listdir(REF_DIR):
            p = os.path.join(REF_DIR, fn)
            if fn.endswith(".md") and os.path.isfile(p):
                refblob += open(p, encoding="utf-8").read() + " "
    dynamic_path = os.path.join(REF_DIR, "dynamic-workflows.md")
    if os.path.exists(dynamic_path):
        dynamic_text = open(dynamic_path, encoding="utf-8").read()
        if "agy --sandbox -p" in dynamic_text and "read-only" in dynamic_text:
            fails.append("dynamic-workflows.md must not imply agy --sandbox certifies read-only verification")
    nref = norm(refblob)
    for inv in cases.get("reference_invariants", []):
        if norm(inv["prompt"]) not in nref and norm(inv["prompt"]) not in ntext:
            fails.append(f"references missing invariant: {inv['prompt']!r}")

    if fails:
        print("SKILL CHECK: FAIL")
        for fail in fails:
            print("  -", fail)
        return 1

    print(
        "SKILL DOCTRINE CHECK: PASS "
        f"({len(cases.get('doctrine_invariants', []))} doctrine, "
        f"{len(cases.get('reference_invariants', []))} reference, "
        f"{len(topology_cases)} topology, "
        f"{word_count} words; routing evidence is external)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
