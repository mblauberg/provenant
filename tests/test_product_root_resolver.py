import json
from pathlib import Path
import subprocess
import sys
import threading

import pytest

from scripts.lib.product_root_resolver import load_pointer_file, load_pointer_path, write_pointer_file


def test_pointer_file_round_trips_an_absolute_existing_product_root(tmp_path: Path) -> None:
    instance_root = tmp_path / "instance"
    product_root = tmp_path / "product"
    product_root.mkdir()

    write_pointer_file(instance_root, product_root)

    pointer = instance_root / ".agent-fabric/product-root.json"
    assert json.loads(pointer.read_text()) == {
        "schema_version": 1,
        "product_root": str(product_root),
    }
    assert load_pointer_file(instance_root) == product_root


def test_pointer_file_returns_none_for_missing_invalid_or_stale_values(tmp_path: Path) -> None:
    instance_root = tmp_path / "instance"
    pointer = instance_root / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)

    invalid_values = [
        "{",
        json.dumps({"schema_version": 2, "product_root": str(tmp_path)}),
        json.dumps({"schema_version": 1, "product_root": "relative"}),
        json.dumps({"schema_version": 1, "product_root": str(tmp_path / "missing")}),
    ]
    for value in invalid_values:
        pointer.write_text(value)
        assert load_pointer_file(instance_root) is None

    pointer.unlink()
    assert load_pointer_file(instance_root) is None


def test_pointer_path_preserves_an_absolute_stale_path_for_relocation(tmp_path: Path) -> None:
    instance_root = tmp_path / "instance"
    pointer = instance_root / ".agent-fabric/product-root.json"
    pointer.parent.mkdir(parents=True)
    stale = tmp_path / "old'\\product"
    pointer.write_text(json.dumps({"schema_version": 1, "product_root": str(stale)}))

    assert load_pointer_file(instance_root) is None
    assert load_pointer_path(instance_root) == stale


def test_concurrent_pointer_writes_leave_complete_valid_json(tmp_path: Path) -> None:
    instance_root = tmp_path / "instance"
    products = [tmp_path / f"product-{index}" for index in range(8)]
    for product in products:
        product.mkdir()

    threads = [
        threading.Thread(target=write_pointer_file, args=(instance_root, product))
        for product in products
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    assert load_pointer_file(instance_root) in products


def test_a_symlinked_pointer_directory_is_refused_not_followed(tmp_path: Path) -> None:
    """Writing goes through the pointer's parent three times; mkdir follows links.

    Hardening on the same trust model as the seeding guards: swapping a
    directory inside the instance root already needs the user's own privileges.
    It is cheap, and it turns a misdirected write into a loud refusal.
    """
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    product_root = tmp_path / "product"
    product_root.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (instance_root / ".agent-fabric").symlink_to(outside, target_is_directory=True)

    with pytest.raises(ValueError, match="escapes the instance root"):
        write_pointer_file(instance_root, product_root)

    assert list(outside.iterdir()) == [], "nothing may be written through the swap"


def test_a_self_targeting_pointer_directory_is_refused_not_followed(tmp_path: Path) -> None:
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    product_root = tmp_path / "product"
    product_root.mkdir()
    (instance_root / ".agent-fabric").symlink_to(instance_root, target_is_directory=True)

    with pytest.raises(ValueError, match="must not be a symlink"):
        write_pointer_file(instance_root, product_root)

    assert not (instance_root / ".gitignore").exists()
    assert not (instance_root / "product-root.json").exists()


def test_pointer_writer_replaces_gitignore_symlink_without_following_it(tmp_path: Path) -> None:
    instance_root = tmp_path / "instance"
    pointer_directory = instance_root / ".agent-fabric"
    pointer_directory.mkdir(parents=True)
    product_root = tmp_path / "product"
    product_root.mkdir()
    external = tmp_path / "external-gitignore"
    external.write_text("keep me\n")
    (pointer_directory / ".gitignore").symlink_to(external)

    write_pointer_file(instance_root, product_root)

    assert external.read_text() == "keep me\n"
    assert not (pointer_directory / ".gitignore").is_symlink()
    assert (pointer_directory / ".gitignore").read_text() == "*\n"


def test_the_pointer_writer_cli_reports_an_escape_as_a_conflict(tmp_path: Path) -> None:
    instance_root = tmp_path / "instance"
    instance_root.mkdir()
    product_root = tmp_path / "product"
    product_root.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (instance_root / ".agent-fabric").symlink_to(outside, target_is_directory=True)

    result = subprocess.run(
        [
            sys.executable,
            str(Path(__file__).resolve().parents[1] / "scripts" / "write-product-root-pointer.py"),
            str(instance_root),
            str(product_root),
        ],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 3
    assert "conflicting:" in result.stderr
    assert "Traceback" not in result.stderr
    assert list(outside.iterdir()) == []
