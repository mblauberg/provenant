#!/usr/bin/env python3
"""Regenerate the Rust port planning graph and inventory without importing product code."""

from __future__ import annotations

import argparse
import ast
from collections import Counter, defaultdict
import fnmatch
import hashlib
import json
import math
from pathlib import Path
import posixpath
import re
import subprocess

ROOT = Path(__file__).resolve().parents[3]
HOME = Path(__file__).resolve().parent
VERSION = 5
JS_SUFFIXES = {".ts", ".tsx", ".mts", ".cts", ".mjs"}
RETIREMENTS = {
    "scripts/static-security-check.py": "no tracked Python sources remain",
    "scripts/lint-timebomb-fixtures.py": "no tests/*.py and no *.test.ts remain",
}
GATES = {
    "unit": "Target crate's unit tests, fmt and clippy; CI once the crate exists",
    "J1": "tests/golden/: old/new CLI bytes, status, argv and environment; pending J1",
    "J2": "schemas/: old/new record and config validation; pending J2",
    "J3": "Fabric MCP transcript record/replay; pending J3, runner path supplied by its owner",
    "lane": "tests/lane_conformance/: unchanged black-box lifecycle gates, including S2 rows in flight",
    "front-door": "tests/front_door_parity/: retain until S1 deletion criteria pass; then Rust equivalents",
    "differential": "Pinned old/new isolated runs with reviewed divergences.md entries",
}


def source_files():
    tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0")
    result = {}
    for name in sorted(filter(None, tracked)):
        if name.startswith("docs/specs/rust-port/"):
            continue  # Planning tools are bootstrap tooling, not product port units.
        path = ROOT / name
        if path.is_symlink() or not path.is_file():
            continue
        # ps.mjs is a production module imported by TS; other JS entry points
        # remain in the explicit caller audit, outside the requested inventory.
        if path.suffix not in {".py", ".ts", ".tsx", ".mts", ".cts"} and name != "runtime/fabric/src/ps.mjs":
            if path.suffix not in {"", ".template"}:
                continue
            if b"python" not in path.read_bytes()[:80].split(b"\n")[0]:
                continue
        result[name] = path.read_text()
    return result


def is_test(path):
    return bool(re.search(r"(^|/)(tests?|fixtures|Fixtures)(/|$)|\.test\.", path)) or path in {
        "runtime/fabric/smoke.ts", "runtime/fabric/live-sandbox-smoke.py",
        "runtime/fabric/vitest.config.ts",
    }


def crate(path):
    if is_test(path):
        if path.startswith("runtime/fabric/tests/"):
            path = path.replace("/tests/", "/src/").replace(".test.ts", ".ts")
        else:
            stem = Path(path).stem.removeprefix("test_")
            for base in ("skills/orchestrate/scripts", "scripts", "skills/_shared"):
                candidate = f"{base}/{stem}.py"
                if (ROOT / candidate).is_file():
                    path = candidate
                    break
    name = Path(path).stem.replace("-", "_")
    if path.startswith("runtime/fabric/src/"):
        if name == "store" or path.endswith("graph/store.ts"):
            return "fabric-store"
        if name in {"process_identity", "ps"}:
            return "supervisor"
        if name in {"server", "tool_schemas", "export_tool_schemas"}:
            return "fabric-mcp"
        if name == "cli" or name == "landing_cli":
            return "cli"
        return "fabric-runtime"
    if any(x in name for x in ("provider_quota", "quota_")) or "/quota_providers/" in path:
        return "quota"
    if name.startswith("telemetry") or name == "context_usage":
        return "telemetry"
    if name.startswith("model_route") or name in {"exec_routing", "capabilities"}:
        return "routing"
    if name in {"process_info", "stall_watchdog", "bounded_process", "worker_liveness", "ps"}:
        return "supervisor"
    if name in {"agy_home", "instruction_quarantine", "output_custody", "secret_scan", "trusted_git", "lane_protection"}:
        return "confinement"
    if "/adapters/" in path or name in {"provider_exec", "provider_output", "cf_dispatch", "provider_identity"}:
        return "providers"
    if name in {"roots", "workspace_paths", "excludes", "jsonc", "product_root_resolver", "host_identity"}:
        return "core"
    if "record" in name or name in {"attempt_storage", "attempt_evidence", "layout", "delivery_run_shape"}:
        return "records"
    if any(x in name for x in ("profile", "installation", "configure", "install_", "front_door_build")) or name == "write_product_root_pointer":
        return "config"
    if name.startswith("fabric_"):
        return "fabric-runtime"
    if name in {"dispatch_run", "batch_run", "run_controls", "run_lineage", "run_dir_finalize", "run_worker_detached", "memory_admission", "lease", "log_reducer"}:
        return "run"
    if path.startswith("skills/"):
        return "workflow"
    return "cli"


def disposition(path):
    if path in RETIREMENTS:
        return "retired", RETIREMENTS[path]
    if path in {"scripts/provenant", "scripts/provenant.template"}:
        return "dropped", "S1 conditional deletion; Rust front door already exists; keep until trial passes"
    if path.endswith("/python-owner.ts"):
        return "dropped", "Python bridge retires after all owner calls use Rust; preserve external entry points"
    if is_test(path):
        return "redesigned", "Retain black-box judges; move internal assertions to Rust; deletion only by S1-S4 inventory"
    if Path(path).stem in {"dispatch_run", "provider_exec", "run_lineage", "process-identity", "front_door_build"}:
        return "redesigned", "Replace mechanics and test seams under supervisor S1-S4 gates; retain policy behaviour"
    return "ported", "Preserve boundary; free internal redesign; no dead-code claim from import reachability"


def ts_tokens(text):
    # Keep strings atomic and discard comments, so examples cannot invent edges.
    pattern = r'''//[^\n]*|/\*[\s\S]*?\*/|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`(?:\\.|[^`\\])*`|[A-Za-z_$][\w$]*|[^\s]'''
    return [(m[0], text.count("\n", 0, m.start()) + 1) for m in re.finditer(pattern, text) if not m[0].startswith(("//", "/*"))]


def imports(files):
    """Conservative module edges, including literal dynamic imports and path loaders."""
    edges = {p: set() for p in files}
    unresolved = []
    modules = defaultdict(set)
    for p in files:
        stem = p.rsplit(".", 1)[0] if Path(p).suffix in {".py"} | JS_SUFFIXES else p
        modules[stem.replace("/", ".")].add(p)
        modules[Path(stem).name].add(p)
        if stem.endswith("/__init__"):
            modules[stem[:-9].replace("/", ".")].add(p)

    def resolve(owner, module, line):
        rel = module.replace(".", "/")
        candidates = set()
        # Absolute dotted names and direct-script sibling imports are both used.
        for base in (Path(owner).parent, Path("."), Path("skills"), Path("scripts")):
            for suffix in (".py", "/__init__.py"):
                p = (base / (rel + suffix)).as_posix()
                if p in files:
                    candidates.add(p)
        if not candidates:
            candidates.update(modules.get(module, ()))
        if len(candidates) > 1:
            unresolved.append({"source": owner, "line": line, "kind": "ambiguous-import", "expression": module})
        edges[owner].update(candidates - {owner})

    for p, text in files.items():
        if Path(p).suffix in JS_SUFFIXES:
            tokens = ts_tokens(text)
            for i, (token, line) in enumerate(tokens[:-1]):
                if token not in {"from", "import", "require"}:
                    continue
                at = i + 1
                if tokens[at][0] == "(":
                    at += 1
                value = tokens[at][0]
                if not value.startswith(("'", '"')):
                    if token in {"import", "require"} and tokens[i + 1][0] == "(":
                        unresolved.append({"source": p, "line": line, "kind": "dynamic-ts-review", "expression": "non-literal import/require"})
                    continue
                module = value[1:-1]
                if not module.startswith("."):
                    continue
                target = (ROOT / p).parent / module
                relative = target.resolve().relative_to(ROOT).as_posix()
                options = [relative, re.sub(r"\.js$", ".ts", relative), relative + ".ts", relative + "/index.ts"]
                found = [q for q in options if q in files]
                if found:
                    edges[p].update(found)
                else:
                    unresolved.append({"source": p, "line": line, "kind": "unresolved-local-ts", "expression": module})
            # Cross-language exec paths are dependencies too, not TS imports.
            for token, _ in tokens:
                if token.startswith(("'", '"')):
                    value = token[1:-1]
                    for candidate in (value, posixpath.normpath((Path(p).parent / value).as_posix())):
                        if candidate in files and candidate != p:
                            edges[p].add(candidate)
            continue
        tree = ast.parse(text, filename=p)
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    resolve(p, alias.name, node.lineno)
            elif isinstance(node, ast.ImportFrom):
                module = node.module or ""
                if node.level:
                    base = Path(p).parent
                    for _ in range(node.level - 1):
                        base = base.parent
                    module = ".".join((*base.parts, module)).rstrip(".")
                resolve(p, module, node.lineno)
                for alias in node.names:
                    resolve(p, f"{module}.{alias.name}".lstrip("."), node.lineno)
            elif isinstance(node, ast.Call):
                fn = ast.unparse(node.func)
                if fn.rsplit(".", 1)[-1] in {"import_module", "spec_from_file_location", "_sibling"}:
                    # Quoted file names and module arguments are evidence; no code is executed.
                    literals = [n.value for n in ast.walk(node) if isinstance(n, ast.Constant) and isinstance(n.value, str)]
                    for value in literals:
                        if value.endswith(".py"):
                            candidates = [q for q in files if q.endswith("/" + value) or q == value]
                            edges[p].update(set(candidates) - {p})
                        elif re.fullmatch(r"[A-Za-z_]\w*(?:\.\w+)*", value):
                            resolve(p, value.removeprefix("provenant_"), node.lineno)
                    if not node.args or not all(isinstance(n, ast.Constant) for n in node.args):
                        unresolved.append({"source": p, "line": node.lineno, "kind": "dynamic-loader-review", "expression": ast.unparse(node)[:200]})
        # Path constants passed indirectly to importlib still establish conservative edges.
        for match in re.finditer(r'''["']([^"'\n]+\.py)["']''', text):
            value = match[1]
            edges[p].update(q for q in files if q != p and (q == value or q.endswith("/" + value)))
        if p.endswith("/adapters/__init__.py"):
            # profile(name)'s import_module f-string is bounded by the sibling registry.
            edges[p].update(q for q in files if Path(q).parent == Path(p).parent and q != p)
    return {p: sorted(v) for p, v in sorted(edges.items())}, sorted(unresolved, key=lambda x: (x["source"], x["line"], x["expression"]))


def components(edges):
    """Tarjan SCCs: imports can cycle; a lane dependency graph cannot."""
    index, low, stack, active, groups = {}, {}, [], set(), []

    def visit(p):
        index[p] = low[p] = len(index)
        stack.append(p)
        active.add(p)
        for q in edges[p]:
            if q not in index:
                visit(q)
                low[p] = min(low[p], low[q])
            elif q in active:
                low[p] = min(low[p], index[q])
        if low[p] == index[p]:
            group = []
            while True:
                q = stack.pop()
                active.remove(q)
                group.append(q)
                if q == p:
                    break
            groups.append(sorted(group))

    for p in sorted(edges):
        if p not in index:
            visit(p)
    return sorted(groups)


def slices(path, text):
    """Group declaration/statement spans, not arbitrary physical line chunks."""
    lines = text.splitlines()
    if len(lines) <= 350:
        return [{"start": 1, "end": len(lines), "symbols": [Path(path).stem]}]
    anchors = {1: "module"}
    if Path(path).suffix in JS_SUFFIXES:
        for i, line in enumerate(lines, 1):
            if re.match(r"\s*(?:(?:export |async |private |static )*(?:function|class|interface|type|const)\s|(?:if|else if|try|catch|case)\b|[A-Za-z_]\w*\([^;]*\)\s*(?::[^=]+)?\{)", line):
                anchors[i] = line.strip()[:90]
    else:
        def walk(body, prefix=""):
            for node in body:
                label = prefix + getattr(node, "name", type(node).__name__)
                anchors[node.lineno] = label
                if node.end_lineno - node.lineno > 250:
                    children = getattr(node, "body", [])
                    if children:
                        walk(children, label + ".")
                    for field in ("orelse", "finalbody", "handlers"):
                        walk(getattr(node, field, []), label + "." + field + ".")
        walk(ast.parse(text).body)
    points = sorted(anchors)
    chunks, start, symbols = [], 1, []
    for at, end in zip(points, points[1:] + [len(lines) + 1]):
        if at - start >= 150 and end - start > 350:
            chunks.append({"start": start, "end": at - 1, "symbols": symbols})
            start, symbols = at, []
        symbols.append(anchors[at])
    chunks.append({"start": start, "end": len(lines), "symbols": symbols})
    return chunks


def generate():
    files = source_files()
    all_edges, unresolved = imports(files)
    product = {p: t for p, t in files.items() if not is_test(p)}
    edges = {p: [q for q in all_edges[p] if q in product] for p in product}
    groups = components(edges)
    membership = {p: i for i, group in enumerate(groups) for p in group}
    cyclic = {p for group in groups if len(group) > 1 for p in group}
    nodes, units = [], {}
    integration_ids = {
        i: "integrate:" + hashlib.sha256("\n".join(group).encode()).hexdigest()[:12]
        for i, group in enumerate(groups) if len(group) > 1
    }
    for p, text in product.items():
        units[p] = []
        action, reason = disposition(p)
        risk = "high" if crate(p) in {"confinement", "supervisor", "run", "fabric-store"} or "custody" in p or "worktree" in p else "medium"
        gates = ["unit", "J1", "J2", "differential"]
        if p.startswith("runtime/fabric/") or crate(p).startswith("fabric"):
            gates.append("J3")
        if crate(p) in {"providers", "supervisor", "run", "confinement"} or "process-identity" in p:
            gates.append("lane")
        if "front_door" in p or p.startswith("scripts/provenant"):
            gates.append("front-door")
        if p in cyclic:
            nodes.append({"id": p + "#interface", "kind": "interface", "source_files": [p], "target_crate": crate(p), "depends_on": [], "risk": risk, "recommended_model_tier": "principal", "gates": ["J2", "unit"], "lane_days": 0.25, "action": "redesigned"})
        for i, span in enumerate(slices(p, text), 1):
            key = p + f"#unit-{i:02}"
            units[p].append(key)
            target = crate(p)
            if p.endswith("/provider_exec.py"):
                if any(s.startswith("_run_attempt") for s in span["symbols"]):
                    target = "supervisor"
                elif any(s.startswith("build_plan") for s in span["symbols"]):
                    target = "confinement"
            if target in {"supervisor", "confinement"}:
                risk = "high"
            nodes.append({"id": key, "kind": "port", "source_files": [p], "source_spans": [span], "target_crate": target, "depends_on": [], "risk": risk, "recommended_model_tier": "senior" if risk == "high" else "junior", "review_model_tier": "principal", "gates": gates, "lane_days": 0.5, "action": action})
            if action == "retired":
                nodes[-1].update(kind="retirement", deletion_criterion=reason,
                                 dispatchable=False, lane_days=0, gates=[])
    by_id = {n["id"]: n for n in nodes}
    for p, keys in units.items():
        if p in RETIREMENTS:
            continue  # Deletion criteria replace Rust port prerequisites.
        dependencies = set()
        if p in cyclic:
            dependencies.add(p + "#interface")
        for q in edges[p]:
            dependencies.add(q + "#interface" if membership[p] == membership[q] else integration_ids.get(membership[q], units[q][-1]))
        for i, key in enumerate(keys):
            by_id[key]["depends_on"] = sorted(dependencies | ({keys[i - 1]} if i else set()))
    # A cycle is broken by contracts, then explicitly closed by an integration gate.
    for i, group in enumerate(groups):
        if len(group) > 1:
            key = integration_ids[i]
            nodes.append({"id": key, "kind": "integration", "source_files": group, "target_crate": "cross-crate", "depends_on": sorted(units[p][-1] for p in group), "risk": "high", "recommended_model_tier": "principal", "gates": list(GATES), "lane_days": 0.5, "action": "redesigned"})
    prerequisites = {
        "J1": ("tests/golden/", "cli"),
        "J2": ("schemas/", "records"),
        "J3": ("Fabric MCP transcript recorder/replayer, runner location pending J3", "fabric-mcp"),
        "s2": ("feat/s2-lane-run; integrate accepted S2, do not duplicate it", "supervisor"),
        "adr": ("User approval to amend ADR 0027's std-only dependency restriction", "cross-crate"),
    }
    for n in nodes:
        if n["action"] == "retired":
            continue
        requirements = {g for g in n["gates"] if g in {"J1", "J2", "J3"}} | {"adr"}
        if n["target_crate"] == "supervisor":
            requirements.add("s2")
        n["depends_on"] = sorted(set(n["depends_on"]) | {"prerequisite:" + g for g in requirements} | {"workspace:manifest-and-digest"})
    nodes.extend([
        {"id": "workspace:manifest-and-digest", "kind": "integration", "source_files": ["tools/front-door/Cargo.toml", "tools/front-door/Cargo.lock", "scripts/lib/front_door_build.py", "scripts/check-provenant-install.py"], "target_crate": "cli", "depends_on": ["prerequisite:adr", "prerequisite:J1", "prerequisite:J2"], "risk": "high", "recommended_model_tier": "senior", "review_model_tier": "principal", "gates": ["unit", "J1", "J2", "front-door", "differential"], "lane_days": 0.5, "action": "redesigned", "description": "Add workspace and crate-aware source digest before linking new libraries"},
        {"id": "workspace:front-door-syscalls", "kind": "port", "source_files": ["tools/front-door/src/exec.rs", "tools/front-door/src/resolve.rs"], "target_crate": "supervisor", "depends_on": ["workspace:manifest-and-digest", "prerequisite:s2"], "risk": "high", "recommended_model_tier": "senior", "review_model_tier": "principal", "gates": ["unit", "J1", "front-door", "lane", "differential"], "lane_days": 0.5, "action": "redesigned", "description": "Move existing execve/access/getpwuid unsafe into supervisor::sys and retain root/exec behaviour"},
    ])
    tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0")

    def planned_unit(key, target, patterns, description, dependencies=()):
        return {
            "id": key, "kind": "integration", "source_globs": patterns,
            "source_files": sorted(p for p in tracked if p and any(fnmatch.fnmatchcase(p, g) for g in patterns)),
            "target_crate": target,
            "depends_on": sorted({"workspace:manifest-and-digest", *dependencies}),
            "risk": "high" if target == "supervisor" else "medium", "recommended_model_tier": "senior",
            "review_model_tier": "principal", "gates": ["unit", "J1", "J2", "differential"] + (["lane", "front-door"] if target == "supervisor" else []),
            "lane_days": 0.5, "action": "redesigned", "description": description,
        }

    nodes.extend([
        planned_unit("records:yaml-data-to-json", "records", [
            "skills/*/evals/*.yaml", "library/*/evals/*.yaml", "evals/*.yaml",
            "docs/evals/*.yaml", "config/adapter-compatibility.yaml",
            "runtime/*/provenance_components.yaml", "tests/fixtures/*.yaml",
        ], "Migrate Provenant-owned YAML to JSON with all readers, callers and fixtures, including scripts/model_route.py's runtime adapter compatibility input; preserve values and validate consumers"),
        planned_unit("records:external-yaml-subset", "records", [
            ".github/ISSUE_TEMPLATE/*.yml", ".github/ISSUE_TEMPLATE/*.yaml",
            "skills/setup-repo/templates/*.yml", "skills/setup-repo/templates/*.yaml",
            "*/agents/openai.yaml", "SKILL.md", "*/SKILL.md",
        ], "Implement ADR 0028 decision 5's fail-closed YAML subset; CI parses every tracked external-group file and, while Python exists, differentially compares accepted values with PyYAML; refusals name construct and line, ambiguous plain scalars say quote this"),
        planned_unit("workspace:front-door-unsafe-guard", "supervisor", [
            "tools/front-door/src/lane/*.rs", "tools/front-door/src/main.rs",
        ], "Move remaining front-door lane unsafe into supervisor::sys through safe APIs, then add #![forbid(unsafe_code)] to the binary; every non-supervisor library already forbids unsafe",
            ["workspace:front-door-syscalls", "prerequisite:s2"]),
        planned_unit("workspace:command-construction-lint", "supervisor", [
            "tools/front-door/clippy.toml", "tools/front-door/src/*.rs",
            "tools/front-door/crates/*/src/*.rs",
            "tools/front-door/tests/*.rs", "tools/front-door/crates/*/tests/*.rs",
        ], "Add workspace clippy.toml disallowed-methods for std::process::Command::new outside supervisor; migrate production callers to supervisor custody APIs and review narrowly scoped test-oracle allowances before denying the lint"),
        planned_unit("cli:rust-test-date-lint", "cli", [
            "tools/front-door/crates/cli/src/timebomb.rs", "scripts/lint-timebomb-fixtures.py",
        ], "Retarget date lint to Rust #[test] sources using provenant-cli's existing textual date engine; cover discovery, changed-test context, traversal, diagnostics and CI binding without Python AST analysis"),
    ])
    for n in nodes:
        if "scripts/check_harness.py" in n["source_files"]:
            n["depends_on"] = sorted(set(n["depends_on"]) | {
                "records:external-yaml-subset", "records:yaml-data-to-json",
            })
    for key, (description, target) in prerequisites.items():
        nodes.append({"id": "prerequisite:" + key, "kind": "external-prerequisite", "source_files": [], "target_crate": target, "depends_on": [], "risk": "high", "recommended_model_tier": "principal", "gates": [key] if key in GATES else ["lane"] if key == "s2" else [], "lane_days": 0, "action": "redesigned", "description": description, "dispatchable": False})
    # Kahn waves, with leaves (no outstanding dependencies) first.
    pending = {n["id"]: set(n["depends_on"]) for n in nodes}
    waves = []
    while pending:
        ready = sorted(k for k, deps in pending.items() if not deps)
        if not ready:
            raise ValueError("unit graph has a dependency cycle")
        waves.append(ready)
        for key in ready:
            del pending[key]
        for deps in pending.values():
            deps.difference_update(ready)
    digest = hashlib.sha256()
    for p, t in files.items():
        digest.update(p.encode() + b"\0" + t.encode() + b"\0")
    totals = Counter()
    for n in nodes:
        totals[n["target_crate"]] += n["lane_days"]
    graph = {"version": VERSION, "rulebook_version": VERSION, "source_digest": digest.hexdigest(), "scope": "Git-tracked Python (including shebang entry points), TypeScript and imported production ps.mjs; tests inventory-only; planning scripts excluded", "gate_definitions": GATES, "nodes": sorted(nodes, key=lambda n: n["id"]), "leaves_first_waves": waves, "estimated_lane_days_by_crate": dict(sorted(totals.items())), "source_imports": all_edges, "cyclic_source_groups": [g for g in groups if len(g) > 1], "loader_review": unresolved}
    inventory = ["# Source inventory", "", "Generated by `derive_graph.py`; do not edit. Physical lines, including comments", "and blanks. Every tracked Python/TypeScript source over 200 lines is listed,", "including shebang entry points and tests. Planning scripts are excluded.", "Actions are plans, not claims of completed Rust ports. Test targets name the", "inferred owning crate's test area; retained external judges need no Rust rewrite.", "", "Qualifications: P = boundary preserved, internal redesign allowed; T = retain", "black-box judges, move internal assertions to Rust, delete only under S1-S4;", "S = partial replacement under S1-S4, keep policy; D = conditional bridge or", "front-door deletion after replacement and judge gates. R = retired tooling;", "each unit records its deletion criterion and is not dispatched as a Rust port.", "No file is deemed dead from an absent import. `provider_exec.py` spans providers,", "confinement and supervisor; the graph assigns its individual units.", "", "| Source | Lines | Target crate | Action | Note |", "|---|---:|---|---|---|"]
    for p, t in files.items():
        count = len(t.splitlines())
        if count > 200:
            action, reason = disposition(p)
            owner = crate(p)
            if is_test(p):
                owner += " (tests)"
            note = f"R: {reason}" if action == "retired" else "D" if action == "dropped" else "T" if is_test(p) else "S" if action == "redesigned" else "P"
            inventory.append(f"| [{p}](../../../{p}) | {count} | {owner} | {action} | {note} |")
    inventory.extend(["", f"Source totals: {len(files)} files, {sum(len(t.splitlines()) for t in files.values()):,} lines; product {len(product)} files, {sum(len(t.splitlines()) for t in product.values()):,} lines.", "", "Partial deletions in large retained files are governed by the supervisor's", "[deletion list](../harness/rust-supervisor.md#deletable-tests) and", "[test-by-test inventory](../harness/rust-supervisor-tests.md), not whole-file removal.", ""])
    outputs = {HOME / "graph.json": json.dumps(graph, indent=2, ensure_ascii=False) + "\n", HOME / "inventory.md": "\n".join(inventory)}
    readme = HOME / "README.md"
    if readme.exists():
        table = ["<!-- estimates:start -->", "", "| Crate | Base lane-days | Planning range |", "|---|---:|---:|"]
        for name, days in sorted(totals.items()):
            table.append(f"| {name} | {days:g} | {days:g}–{math.ceil(days * 1.75 * 2) / 2:g} |")
        total = sum(totals.values())
        table.extend([f"| **Total** | **{total:g}** | **{total:g}–{math.ceil(total * 1.75 * 2) / 2:g}** |", "", "<!-- estimates:end -->"])
        original = readme.read_text()
        if "<!-- estimates:start -->" not in original:
            raise ValueError("README needs estimate markers")
        outputs[readme] = re.sub(r"<!-- estimates:start -->[\s\S]*?<!-- estimates:end -->", "\n".join(table), original)
    return outputs


def self_test():
    fixtures = {
        "pkg/a.py": "from . import b\nimport importlib\nimportlib.import_module('pkg.b')\n",
        "pkg/b.py": "from pkg import a\n",
        "scripts/a.py": "import helper\n",
        "scripts/helper.py": "import yaml\nyaml.safe_load('a: 1')\n",
        "runtime/a.ts": "// import './wrong.js'\n/* export * from './wrong.js' */\nexport { b } from './b.js';\nconst x = import('./b.js');\nimport(dynamic);\nconst owner = 'scripts/helper.py';\n",
        "runtime/b.ts": "export const b = 1;\n",
    }
    graph, review = imports(fixtures)
    assert graph["pkg/a.py"] == ["pkg/b.py"]
    assert graph["pkg/b.py"] == ["pkg/a.py"]
    assert graph["scripts/a.py"] == ["scripts/helper.py"]
    assert graph["runtime/a.ts"] == ["runtime/b.ts", "scripts/helper.py"]
    assert any(r["kind"] == "dynamic-ts-review" for r in review)
    assert ["pkg/a.py", "pkg/b.py"] in components(graph)
    assert not any(r["source"] == "scripts/helper.py" for r in review)
    for p, t in fixtures.items():
        spans = slices(p, t)
        assert spans[0]["start"] == 1 and spans[-1]["end"] == len(t.splitlines())
        assert all(a["end"] + 1 == b["start"] for a, b in zip(spans, spans[1:]))
    print("8 graph fixture checks passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if committed outputs differ")
    parser.add_argument("--self-test", action="store_true", help="check import resolution and cycle fixtures")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return 0
    outputs = generate()
    changed = []
    for path, text in outputs.items():
        if not path.exists() or path.read_text() != text:
            changed.append(path.name)
            if not args.check:
                path.write_text(text)
    print(("stale: " if args.check and changed else "generated: ") + ", ".join(changed or ["no changes"]))
    return int(args.check and bool(changed))


if __name__ == "__main__":
    raise SystemExit(main())
