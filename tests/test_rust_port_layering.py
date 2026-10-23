"""Rust port crates depend only on the internal crates PORTING.md R2 allows."""
import re
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKSPACE = ROOT / "tools/front-door"


def allowed_edges() -> dict[str, set[str]]:
    rulebook = (ROOT / "docs/specs/rust-port/PORTING.md").read_text()
    section = rulebook.split("## R2.", 1)[1].split("\n## ", 1)[0]
    edges = {}
    for crate, allowed in re.findall(r"^\| ([a-z-]+) \| [^|]+ \| ([^|]+) \|$", section, re.M):
        if crate == "Crate":
            continue
        names = re.findall(r"\b(core|records|config|supervisor|confinement|providers|quota|telemetry|"
                           r"routing|run|fabric-store|fabric-runtime|fabric-mcp|workflow)\b", allowed)
        # cli may use every library; "supervisor's safe ... API" names supervisor.
        edges[crate] = set(names) if crate != "cli" else {"*"}
    return edges


def test_internal_dependencies_follow_r2():
    edges = allowed_edges()
    members = sorted((WORKSPACE / "crates").glob("*/Cargo.toml"))
    assert {path.parent.name for path in members} == set(edges)
    violations = []
    for manifest in members:
        crate = manifest.parent.name
        data = tomllib.loads(manifest.read_text())
        sections = [data.get("dependencies", {}), data.get("dev-dependencies", {})]
        used = {name.removeprefix("provenant-") for section in sections for name in section
                if name.startswith("provenant-")} - {crate}  # a self dev-dependency enables test features
        allowed = edges[crate]
        if "*" in allowed:
            allowed = set(edges) - {"cli"}
        violations += [f"{crate} -> {name}" for name in sorted(used - allowed)]
    assert not violations, violations


def test_every_internal_crate_is_a_workspace_dependency():
    data = tomllib.loads((WORKSPACE / "Cargo.toml").read_text())
    declared = {name.removeprefix("provenant-") for name in data["workspace"]["dependencies"]
                if name.startswith("provenant-")}
    assert declared == {path.parent.name for path in (WORKSPACE / "crates").glob("*/Cargo.toml")}


def test_every_non_supervisor_library_forbids_unsafe():
    for manifest in (WORKSPACE / "crates").glob("*/Cargo.toml"):
        if manifest.parent.name != "supervisor":
            assert "#![forbid(unsafe_code)]" in (manifest.parent / "src/lib.rs").read_text(), manifest
