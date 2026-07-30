import json
from pathlib import Path
import threading

from scripts.lib.product_root_resolver import load_pointer_file, write_pointer_file


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
