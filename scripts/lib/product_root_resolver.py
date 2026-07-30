"""Machine-local Agent Fabric product-root pointer support."""

from __future__ import annotations

import json
import os
from pathlib import Path
import tempfile
from typing import Any


SCHEMA_VERSION = 1
POINTER_RELATIVE_PATH = Path(".agent-fabric/product-root.json")


def load_pointer_file(instance_root: Path) -> Path | None:
    """Return the valid product root named by an instance pointer, if any."""
    try:
        value: Any = json.loads((instance_root / POINTER_RELATIVE_PATH).read_text())
        if not isinstance(value, dict):
            return None
        schema_version = value.get("schema_version")
        product_root = value.get("product_root")
        if (
            not isinstance(schema_version, int)
            or isinstance(schema_version, bool)
            or schema_version != SCHEMA_VERSION
            or not isinstance(product_root, str)
            or not product_root
        ):
            return None
        candidate = Path(product_root).expanduser()
        if not candidate.is_absolute() or not candidate.exists():
            return None
        return candidate
    except (OSError, TypeError, ValueError, json.JSONDecodeError):
        return None


def write_pointer_file(instance_root: Path, product_root: Path) -> None:
    """Atomically record the product root for one machine-local instance.

    The directory carries its own `.gitignore` of `*`, written before the
    pointer so no window exists in which the absolute path is stageable. Ignore
    rules do not cross repository roots, so a rule in the product checkout says
    nothing about an independent instance repository; it has to travel with the
    file it protects (ADR 0019).
    """
    resolved_product = product_root.expanduser().resolve(strict=True)
    pointer = instance_root.expanduser() / POINTER_RELATIVE_PATH
    pointer.parent.mkdir(parents=True, exist_ok=True)
    (pointer.parent / ".gitignore").write_text("*\n")
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=".product-root.",
        suffix=".tmp",
        dir=pointer.parent,
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w") as stream:
            json.dump(
                {
                    "schema_version": SCHEMA_VERSION,
                    "product_root": str(resolved_product),
                },
                stream,
                sort_keys=True,
            )
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, 0o600)
        os.replace(temporary, pointer)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
