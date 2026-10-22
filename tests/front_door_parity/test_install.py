"""Installed-artefact contract (contract C§11; assertions 45 to 47).

The artefact is whatever `PROVENANT_PARITY_IMPL` names: the Python template, or
the compiled binary. Both must satisfy the same managed-file rules, because the
installer classifies and publishes bytes without knowing which one it holds.
"""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import threading

import pytest

from parity_support import IMPL, ROOT, clean_environment, front_door_binary

INSTALLER = ROOT / "scripts/install-provenant-command.py"
INSTALL_HARNESS = ROOT / "scripts/install-harness"
TEMPLATE = ROOT / "scripts/provenant.template"
MARKER = (
    b"# Managed by scripts/install-harness; "
    b"local edits to an installed copy are not preserved."
)


def artefact() -> Path:
    return front_door_binary() if IMPL == "rust" else TEMPLATE


def other_artefact(tmp_path: Path) -> Path:
    """A different managed front door: the template when testing Rust, a stale copy otherwise."""
    if IMPL == "rust":
        return TEMPLATE
    stale = tmp_path / "stale-template"
    stale.write_bytes(TEMPLATE.read_bytes() + b"\n# stale managed copy\n")
    return stale


def installer(*arguments: str | Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(INSTALLER), *map(str, arguments)],
        capture_output=True,
        text=True,
        check=False,
    )


def classify(source: Path, destination: Path) -> subprocess.CompletedProcess[str]:
    return installer("classify", "--source", source, "--destination", destination)


def publish(source: Path, destination: Path, expected: str) -> subprocess.CompletedProcess[str]:
    return installer("publish", "--source", source, "--destination", destination, "--expected", expected)


def test_artefact_carries_the_managed_marker_as_a_line():
    """C§16 #45: the ownership rule is a byte search for the marker line."""
    assert MARKER in artefact().read_bytes().splitlines()


def test_fresh_install_is_absent_then_existing_and_executable(tmp_path):
    """C§16 #45: absent -> installed (mode 0755) -> existing."""
    destination = tmp_path / "bin/provenant"

    assert classify(artefact(), destination).stdout == "absent\n"
    done = publish(artefact(), destination, "absent")
    assert done.stdout == "installed\n", done.stderr
    assert destination.read_bytes() == artefact().read_bytes()
    assert (destination.stat().st_mode & 0o777) == 0o755
    assert classify(artefact(), destination).stdout == "existing\n"
    assert publish(artefact(), destination, "existing").stdout == "existing\n"


def test_an_older_managed_front_door_is_updated_in_place(tmp_path):
    """C§16 #45: managed-file -> updated, both directions between the two kinds."""
    old = other_artefact(tmp_path)
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    destination.write_bytes(old.read_bytes())

    assert classify(artefact(), destination).stdout == "managed-file\n"
    done = publish(artefact(), destination, "managed-file")
    assert done.stdout == "updated\n", done.stderr
    assert destination.read_bytes() == artefact().read_bytes()
    assert (destination.stat().st_mode & 0o777) == 0o755


@pytest.mark.parametrize("kind", ["foreign-file", "symlink", "directory", "byte-identical-symlink"])
def test_foreign_paths_are_collisions_and_are_left_alone(tmp_path, kind):
    """C§16 #45: a foreign file, a symlink or a directory is exit 3 and untouched."""
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    if kind == "foreign-file":
        destination.write_bytes(b"#!/bin/sh\necho mine\n")
    elif kind == "symlink":
        destination.symlink_to(tmp_path / "elsewhere")
    elif kind == "directory":
        destination.mkdir()
    else:
        target = tmp_path / "copy"
        target.write_bytes(artefact().read_bytes())
        destination.symlink_to(target)
    before = os.lstat(destination).st_ino

    for run in (classify(artefact(), destination), publish(artefact(), destination, "managed-file")):
        assert run.returncode == 3
        assert "provenant command collision=" in run.stderr
    assert os.lstat(destination).st_ino == before


def test_update_is_atomic_never_absent_or_partial(tmp_path):
    """C§16 #45: poll the destination through an update; only old or new bytes are ever seen."""
    old = other_artefact(tmp_path)
    destination = tmp_path / "bin/provenant"
    destination.parent.mkdir()
    destination.write_bytes(old.read_bytes())
    seen: set[bytes | None] = set()
    stop = threading.Event()

    def poll() -> None:
        while not stop.is_set():
            try:
                seen.add(destination.read_bytes())
            except FileNotFoundError:
                seen.add(None)

    thread = threading.Thread(target=poll)
    thread.start()
    try:
        done = publish(artefact(), destination, "managed-file")
    finally:
        stop.set()
        thread.join()

    assert done.stdout == "updated\n", done.stderr
    assert seen <= {old.read_bytes(), artefact().read_bytes()}, "a partial or missing file was visible"


def test_a_bin_directory_inside_the_product_is_refused_before_any_mutation(tmp_path):
    """C§16 #46: `install-harness` refuses a destination inside the checkout."""
    inside = ROOT / ".front-door-parity-never-created"
    home = tmp_path / "home"
    home.mkdir()
    environment = clean_environment(
        HOME=str(home), PROVENANT_BIN_DIR=str(inside), PROVENANT_ALLOW_LINKED_WORKTREE_INSTALL="1"
    )

    done = subprocess.run(
        [str(INSTALL_HARNESS), "--platform", "codex"],
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )

    assert done.returncode == 3
    assert "PROVENANT_BIN_DIR must be outside the product checkout" in done.stderr
    assert not inside.exists()
    assert list(home.iterdir()) == []


def test_python_stub_contains_only_resolution_and_delegation():
    """C§16 #47: the template holds no routing, usage, help or owner names."""
    if IMPL == "rust":
        pytest.skip("one binary by design (ADR 0027 decision 2); the layers are source modules")
    text = TEMPLATE.read_text()
    for forbidden in ("owners =", "def usage", "def help_text", "model-route", "worktree", "check-harness"):
        assert forbidden not in text
