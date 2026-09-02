"""A skill script must establish `_shared` itself, not inherit a sibling's repair (#755).

`skills/*/scripts/` entry points are executed by path, so `sys.path[0]` is the
script's own directory and the shared library one level up is not reachable
without an explicit bootstrap. That bootstrap is a process-global side effect:
once any module performs it, every module loaded afterwards in that process can
import `_shared` whether or not it did the work. The result is a silent
load-order dependency, which is the failure `test_delivery_validator_structure`
already documents for the delivery validator.

These tests remove the ambiguity. Each consumer is loaded on its own into a copy
of the catalogue in which every *other* module's path repair has been
neutralised, so a consumer that free-rides on a sibling fails here instead of
failing later when an import is reordered.
"""

from __future__ import annotations

from pathlib import Path
import re
import shutil
import subprocess
import sys

import pytest

ROOT = Path(__file__).resolve().parents[1]
SKILLS = ROOT / "skills"
REPAIR = re.compile(r"^(\s*)sys\.path\.(?:insert|append)\(.*\)\s*$")


def _consumers() -> list[Path]:
    """Every skill script that reaches for the shared library.

    Discovered rather than listed: a hardcoded tuple lets a new consumer ship
    without a bootstrap while this file reports green.
    """
    return sorted(
        path
        for path in SKILLS.glob("*/scripts/*.py")
        if "_shared" in path.read_text()
    )


CONSUMERS = _consumers()


def _neutralise_sibling_repairs(tree: Path, keep: Path) -> None:
    """Replace every `sys.path` repair outside `keep` with `pass`.

    `pass` rather than deletion because some repairs sit inside an `if` guard,
    whose body would otherwise become empty.
    """
    for path in tree.rglob("*.py"):
        if path == keep:
            continue
        lines = path.read_text().splitlines(keepends=True)
        changed = False
        for index, line in enumerate(lines):
            match = REPAIR.match(line.rstrip("\n"))
            if match:
                lines[index] = f"{match.group(1)}pass\n"
                changed = True
        if changed:
            path.write_text("".join(lines))


def test_the_consumer_list_is_not_empty() -> None:
    assert CONSUMERS, "no skill script imports the shared library: the discovery is wrong"


@pytest.mark.parametrize("consumer", CONSUMERS, ids=lambda p: str(p.relative_to(SKILLS)))
def test_each_shared_consumer_loads_without_a_sibling_path_repair(
    consumer: Path, tmp_path: Path
) -> None:
    tree = tmp_path / "skills"
    shutil.copytree(SKILLS, tree, symlinks=False)
    under_test = tree / consumer.relative_to(SKILLS)
    _neutralise_sibling_repairs(tree, under_test)

    # -I isolates the interpreter so an inherited PYTHONPATH or a user-site
    # .pth cannot satisfy the import and make the check vacuous. -I also
    # implies -P, which suppresses the script-directory entry a real
    # invocation gets, so the harness restores exactly that one entry and
    # nothing else.
    # The module is registered in sys.modules before it is executed because
    # `@dataclass` resolves its own module through `sys.modules` while the
    # class body is being processed.
    probe = (
        "import importlib.util, sys\n"
        "sys.path.insert(0, sys.argv[1])\n"
        "spec = importlib.util.spec_from_file_location('module_under_test', sys.argv[2])\n"
        "module = importlib.util.module_from_spec(spec)\n"
        "sys.modules['module_under_test'] = module\n"
        "spec.loader.exec_module(module)\n"
    )
    result = subprocess.run(
        [sys.executable, "-I", "-c", probe, str(under_test.parent), str(under_test)],
        cwd=tmp_path,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )

    assert result.returncode == 0, (
        f"{consumer.relative_to(ROOT)} only imports `_shared` because a sibling "
        f"repaired sys.path first:\n{result.stderr}"
    )
