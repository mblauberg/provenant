"""Build helper for the Rust front door (ADR 0027): digest, identity, smoke and selection."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys

import pytest

from lib import front_door_build as build

ROOT = Path(__file__).resolve().parents[1]
HELPER = ROOT / "scripts/lib/front_door_build.py"
MARKER = "# Managed by scripts/install-harness; local edits to an installed copy are not preserved."


def make_crate(product: Path) -> Path:
    crate = product / "tools/front-door"
    (crate / "src/help").mkdir(parents=True)
    (crate / "Cargo.toml").write_text('[package]\nname = "provenant"\n')
    (crate / "Cargo.lock").write_text("# lock\n")
    (crate / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.94.1"\n')
    (crate / "src/main.rs").write_text("fn main() {}\n")
    (crate / "src/help/general.txt").write_text("usage: provenant\n")
    return crate


def stamped_binary(path: Path, digest: str, *, exit_code: int = 0, banner: str = "usage: provenant <command>") -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        f'#!/bin/sh\necho "{banner}"\nexit {exit_code}\n\n{MARKER}\nprovenant-front-door source-sha256={digest}\n'
    )
    path.chmod(0o755)
    return path


def fake_cargo(directory: Path, *, fail: bool = False) -> None:
    directory.mkdir(parents=True, exist_ok=True)
    cargo = directory / "cargo"
    body = "#!/bin/sh\n"
    if fail:
        body += "exit 101\n"
    else:
        body += (
            'mkdir -p "$CARGO_TARGET_DIR/release"\n'
            'printf \'#!/bin/sh\\necho "usage: provenant"\\nexit 0\\n\\n%s\\nprovenant-front-door '
            f"source-sha256=%s\\n' '{MARKER}' \"$PROVENANT_FD_SOURCE_SHA256\" "
            '> "$CARGO_TARGET_DIR/release/provenant"\n'
            'chmod 755 "$CARGO_TARGET_DIR/release/provenant"\n'
        )
    cargo.write_text(body)
    cargo.chmod(0o755)


def test_digest_is_stable_and_covers_every_input(tmp_path):
    crate = make_crate(tmp_path)
    base = build.source_digest(crate)
    assert base == build.source_digest(crate)
    assert len(base) == 64

    for relative, change in (
        ("Cargo.toml", "# edit\n"),
        ("Cargo.lock", "# edit\n"),
        ("rust-toolchain.toml", "# edit\n"),
        ("src/main.rs", "// edit\n"),
        ("src/help/general.txt", "edit\n"),
    ):
        path = crate / relative
        original = path.read_text()
        path.write_text(original + change)
        assert build.source_digest(crate) != base, relative
        path.write_text(original)
        assert build.source_digest(crate) == base


def test_digest_notices_added_and_renamed_sources_but_ignores_target(tmp_path):
    crate = make_crate(tmp_path)
    base = build.source_digest(crate)

    (crate / "target/release").mkdir(parents=True)
    (crate / "target/release/provenant").write_bytes(b"artefact")
    (crate / "README.md").write_text("not an input")
    assert build.source_digest(crate) == base

    (crate / "src/extra.rs").write_text("")
    added = build.source_digest(crate)
    assert added != base
    (crate / "src/extra.rs").rename(crate / "src/other.rs")
    assert build.source_digest(crate) != added


def test_digest_without_sources_is_a_missing_file_error(tmp_path):
    with pytest.raises(FileNotFoundError):
        build.source_digest(tmp_path)


def test_embedded_digest_reads_only_whole_identity_lines(tmp_path):
    digest = "c" * 64
    assert build.embedded_digest(stamped_binary(tmp_path / "a", digest)) == digest
    unstamped = tmp_path / "b"
    unstamped.write_bytes(b"\x00\xffprovenant-front-door source-sha256=" + digest.encode() + b" not a line\n")
    assert build.embedded_digest(unstamped) is None
    assert build.embedded_digest(stamped_binary(tmp_path / "c", "unknown")) == "unknown"
    binary = tmp_path / "d"
    binary.write_bytes(b"\x00\x01\n" + f"provenant-front-door source-sha256={digest}\n".encode() + b"\x00")
    assert build.embedded_digest(binary) == digest


def test_smoke_requires_help_to_exit_zero_with_usage(tmp_path):
    build.smoke(stamped_binary(tmp_path / "ok", "a" * 64))
    with pytest.raises(build.FrontDoorError):
        build.smoke(stamped_binary(tmp_path / "exits", "a" * 64, exit_code=3))
    with pytest.raises(build.FrontDoorError):
        build.smoke(stamped_binary(tmp_path / "silent", "a" * 64, banner="hello"))
    with pytest.raises(build.FrontDoorError):
        build.smoke(tmp_path / "missing")


def test_python_mode_never_looks_at_the_crate(tmp_path):
    kind, source, note = build.select(tmp_path, "python", tmp_path / "b")
    assert (kind, source, note) == ("python", tmp_path / "scripts/provenant.template", "")


def test_auto_without_a_crate_is_the_template_with_no_note(tmp_path):
    assert build.select(tmp_path, "auto", tmp_path / "b")[0::2] == ("python", "")


def test_rust_without_a_crate_or_without_cargo_raises(tmp_path):
    with pytest.raises(build.FrontDoorError, match="no tools/front-door crate"):
        build.select(tmp_path, "rust", tmp_path / "b")
    make_crate(tmp_path)
    with pytest.raises(build.FrontDoorError, match="cargo is not on PATH"):
        build.select(tmp_path, "rust", tmp_path / "b", cargo_available=False)


def test_auto_without_cargo_falls_back_with_the_reason(tmp_path):
    make_crate(tmp_path)

    kind, source, note = build.select(tmp_path, "auto", tmp_path / "b", cargo_available=False)

    assert kind == "python" and source.name == "provenant.template"
    assert note == "cargo is not on PATH"


def test_build_stamps_the_digest_and_selects_the_binary(tmp_path, monkeypatch):
    crate = make_crate(tmp_path)
    fake_cargo(tmp_path / "fake")
    monkeypatch.setenv("PATH", f"{tmp_path / 'fake'}{os.pathsep}{os.environ['PATH']}")

    kind, source, note = build.select(tmp_path, "rust", tmp_path / "b")

    assert (kind, note) == ("rust", "built")
    assert build.embedded_digest(source) == build.source_digest(crate)


def test_failed_build_is_fatal_for_rust_and_a_fallback_for_auto(tmp_path, monkeypatch):
    make_crate(tmp_path)
    fake_cargo(tmp_path / "fake", fail=True)
    monkeypatch.setenv("PATH", f"{tmp_path / 'fake'}{os.pathsep}{os.environ['PATH']}")

    with pytest.raises(build.FrontDoorError, match="cargo build failed"):
        build.select(tmp_path, "rust", tmp_path / "b")
    kind, _, note = build.select(tmp_path, "auto", tmp_path / "b")
    assert kind == "python" and "cargo build failed" in note


def test_prebuilt_must_be_absolute_regular_matching_and_healthy(tmp_path):
    crate = make_crate(tmp_path)
    digest = build.source_digest(crate)
    good = stamped_binary(tmp_path / "pre/good", digest)
    assert build.select(tmp_path, "rust", tmp_path / "b", good)[0::2] == ("rust", "prebuilt")

    link = tmp_path / "pre/link"
    link.symlink_to(good)
    for bad, message in (
        (link, "absolute regular file"),
        (Path("relative"), "absolute regular file"),
        (stamped_binary(tmp_path / "pre/stale", "0" * 64), "different sources"),
        (stamped_binary(tmp_path / "pre/sick", digest, exit_code=1), "smoke run"),
    ):
        with pytest.raises(build.FrontDoorError, match=message):
            build.select(tmp_path, "rust", tmp_path / "b", bad)


def test_unknown_mode_is_a_value_error(tmp_path):
    with pytest.raises(ValueError):
        build.select(tmp_path, "fast", tmp_path / "b")


def test_command_line_prints_only_the_answer_on_stdout(tmp_path):
    crate = make_crate(tmp_path)
    result = subprocess.run(
        [sys.executable, str(HELPER), "digest", str(crate)], capture_output=True, text=True, check=False
    )
    assert result.stdout == build.source_digest(crate) + "\n" and result.stderr == ""

    chosen = subprocess.run(
        [sys.executable, str(HELPER), "select", "--product-root", str(tmp_path),
         "--mode", "python", "--build-dir", str(tmp_path / "b")],
        capture_output=True, text=True, check=False,
    )
    assert chosen.stdout == f"python\t{tmp_path / 'scripts/provenant.template'}\n"

    missing = subprocess.run(
        [sys.executable, str(HELPER), "select", "--product-root", str(tmp_path / "none"),
         "--mode", "rust", "--build-dir", str(tmp_path / "b")],
        capture_output=True, text=True, check=False,
    )
    assert missing.returncode == 3 and missing.stdout == ""
