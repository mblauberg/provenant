"""Machine-local Agent Fabric product-root pointer support."""

from __future__ import annotations

import json
import os
from pathlib import Path
import tempfile
from typing import Any


SCHEMA_VERSION = 1
POINTER_RELATIVE_PATH = Path(".agent-fabric/product-root.json")


def _atomic_write_text(destination: Path, content: str, *, prefix: str) -> None:
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=prefix,
        suffix=".tmp",
        dir=destination.parent,
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, 0o600)
        os.replace(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)


def load_pointer_path(instance_root: Path) -> Path | None:
    """Parse the product path in an instance pointer, even if it moved."""
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
        if not candidate.is_absolute():
            return None
        return candidate
    except (OSError, TypeError, ValueError, json.JSONDecodeError):
        return None


def load_pointer_file(instance_root: Path) -> Path | None:
    """Return the existing product root named by an instance pointer, if any."""
    candidate = load_pointer_path(instance_root)
    return candidate if candidate is not None and candidate.exists() else None


def write_pointer_file(instance_root: Path, product_root: Path) -> None:
    """Atomically record the product root for one machine-local instance.

    The directory carries its own `.gitignore` of `*`, written before the
    pointer so no window exists in which the absolute path is stageable. Ignore
    rules do not cross repository roots, so a rule in the product checkout says
    nothing about an independent instance repository; it has to travel with the
    file it protects (ADR 0019).

    Writing goes through the pointer's parent directory three times, and
    `mkdir` follows a symlink, so the resolved parent is required to stay inside
    the resolved instance root: a swapped or symlinked `.agent-fabric` is refused
    rather than followed. This is hardening on the same trust model as the seeding guards in
    `instance_installation._publish`, not a privilege boundary. Anyone able to
    swap a directory inside the instance root already holds the user's own
    privileges on the user's own machine.
    """
    resolved_product = product_root.expanduser().resolve(strict=True)
    resolved_instance = instance_root.expanduser().resolve()
    pointer = instance_root.expanduser() / POINTER_RELATIVE_PATH
    if pointer.parent.is_symlink():
        resolved_parent = pointer.parent.resolve(strict=False)
        if resolved_parent != resolved_instance and resolved_instance not in resolved_parent.parents:
            raise ValueError(
                "product pointer directory escapes the instance root: "
                f"{pointer.parent} resolves to {resolved_parent}"
            )
        raise ValueError("product pointer directory must not be a symlink")
    pointer.parent.mkdir(parents=True, exist_ok=True)
    resolved_parent = pointer.parent.resolve()
    if resolved_parent != resolved_instance and resolved_instance not in resolved_parent.parents:
        raise ValueError(
            "product pointer directory escapes the instance root: "
            f"{pointer.parent} resolves to {resolved_parent}"
        )
    _atomic_write_text(pointer.parent / ".gitignore", "*\n", prefix=".gitignore.")
    payload = json.dumps(
        {
            "schema_version": SCHEMA_VERSION,
            "product_root": str(resolved_product),
        },
        sort_keys=True,
    )
    _atomic_write_text(pointer, payload + "\n", prefix=".product-root.")
