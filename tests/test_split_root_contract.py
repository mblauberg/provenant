"""Split-root contract probes.

These are class-level probes, not a regression list.  The expected failures are
deliberately strict: when the owning issue lands, an XPASS fails the rig and
the annotation must be removed.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

import pytest


ROOT = Path(__file__).resolve().parents[1]
MODEL_ROUTE = ROOT / "scripts" / "model_route.py"
PROVENANT_TEMPLATE = ROOT / "scripts" / "provenant.template"
SKILL_ROOT_COMMAND = re.compile(r"\$\{AGENTS_HOME:-\$HOME/\.agents\}/skills/")


def _clean_split_environment(tmp_path: Path, product_root: Path) -> dict[str, str]:
    environment = os.environ.copy()
    environment.pop("AGENT_FABRIC_INSTANCE_ROOT", None)
    environment.pop("AGENTS_HOME", None)
    environment.update(
        {
            "AGENT_FABRIC_PRODUCT_ROOT": str(product_root),
            "HOME": str(tmp_path / "home"),
        }
    )
    return environment


@pytest.mark.xfail(
    strict=True,
    reason="#549 owns the root-selection fix; a defaulted instance must not fall back to the product",
)
def test_model_route_reads_instance_catalogue_without_an_instance_environment_override(tmp_path: Path) -> None:
    default_instance = tmp_path / "home" / ".agents"
    config = default_instance / "config"
    config.mkdir(parents=True)
    catalogue = json.loads((ROOT / "config" / "model-routing.json").read_text())
    catalogue["catalog_date"] = "2099-12-31"
    (config / "model-routing.json").write_text(json.dumps(catalogue))
    shutil.copy2(ROOT / "config" / "model-preferences.json", config / "model-preferences.json")

    result = subprocess.run(
        [
            sys.executable,
            str(MODEL_ROUTE),
            "resolve",
            "--adapter",
            "claude",
            "--alias",
            "scout",
            "--role",
            "worker",
        ],
        cwd=ROOT,
        env=_clean_split_environment(tmp_path, ROOT),
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["catalog_date"] == "2099-12-31"


def test_installed_wrapper_does_not_promote_default_instance_root_to_explicit_environment(tmp_path: Path) -> None:
    product = tmp_path / "product"
    scripts = product / "scripts"
    scripts.mkdir(parents=True)
    target = scripts / "provenant"
    target.write_text("#!/bin/sh\nprintf 'instance=%s\\n' \"${AGENT_FABRIC_INSTANCE_ROOT-<unset>}\"\n")
    target.chmod(0o755)
    mcp_target = scripts / "agent-fabric-mcp"
    mcp_target.write_text("#!/bin/sh\nexit 0\n")
    mcp_target.chmod(0o755)
    wrapper = tmp_path / "provenant"
    shutil.copy2(PROVENANT_TEMPLATE, wrapper)
    wrapper.chmod(0o755)

    environment = _clean_split_environment(tmp_path, product)
    environment["AGENTS_HOME"] = str(product)
    result = subprocess.run(
        [str(wrapper), "probe"],
        cwd=tmp_path,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == "instance=<unset>\n"


@pytest.mark.xfail(
    strict=True,
    reason="#563 owns managed-skill command roots; product/instance split must not use the fused AGENTS_HOME skill path",
)
def test_managed_skill_commands_do_not_resolve_from_fused_agents_home_fallback() -> None:
    matches: list[str] = []
    for relative in ("skills", "workflows"):
        for path in sorted((ROOT / relative).rglob("*")):
            if not path.is_file():
                continue
            for line_number, line in enumerate(path.read_text(errors="replace").splitlines(), start=1):
                if SKILL_ROOT_COMMAND.search(line):
                    matches.append(f"{path.relative_to(ROOT)}:{line_number}")

    assert matches == [], "fused skill-root command paths remain: " + ", ".join(matches)
