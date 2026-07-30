#!/usr/bin/env python3
"""Own the instance side of an installation: desired state and seeded files.

Two artifacts are easy to confuse and must never merge, so [ADR
0019](../docs/adr/0019-installed-file-class-ownership.md) separates them and
this module is the only writer of the first.

Desired state is the instance's committed statement of intent: which product
version and which install mode this instance wants. It is path-free, identical
on every machine the instance is checked out on, and belongs in the instance
repository. The installation receipt, written by
`managed_installation_manifest.py`, is the opposite: absolute target roots,
per-entry digests and timestamps for what is on this machine right now. It is
machine-local and ignored.

Seeded files are the third owner in that ADR. The product ships a template; the
installer copies it into the instance root only when nothing is there, and never
again. There is no hash-drift check and no three-way merge: both repositories
are Git repositories, so Git is the drift detector.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import sys
import tempfile
from typing import Any


ROOT = Path(__file__).resolve().parents[1]

DESIRED_STATE_RELATIVE = ("config", "installation.json")
DESIRED_STATE_CONTRACT = "installation-desired-state"
DESIRED_STATE_SCHEMA_VERSION = 1
PRODUCT_NAME = "provenant"
#: `fused` is today's layout, where the instance root is the product checkout.
#: `split` is two trees. A path-free file cannot say where the product is, so a
#: consumer resolving a product root reads the mode first and, when it is
#: `split`, the environment second.
INSTALL_MODES = ("fused", "split")

#: Product templates that become instance-owned the moment they are written.
#: Each is a path relative to both roots.
SEEDED_FILES = (
    "AGENTS.md",
    "config/model-preferences.json",
    "config/model-routing.json",
)

#: Machine-local pointer to the product checkout, in the receipt class rather
#: than the committed one. Committed instance state never carries an absolute
#: machine path, so the path a consumer needs lives here, is ignored, and is
#: rewritten on every install. Relocating the product is therefore always
#: "re-run install-harness", never an edit to a committed file or a client
#: configuration.
POINTER_RELATIVE = (".agent-fabric", "product-root.json")
POINTER_SCHEMA_VERSION = 1


class InstallError(ValueError):
    pass


def desired_state_path(instance_root: Path) -> Path:
    return Path(instance_root).joinpath(*DESIRED_STATE_RELATIVE)


def pointer_path(instance_root: Path) -> Path:
    return Path(instance_root).joinpath(*POINTER_RELATIVE)


def write_pointer(product_root: Path, instance_root: Path) -> dict[str, Any]:
    """Rewrite the machine-local product pointer on every install.

    The directory carries its own `.gitignore` of `*`. Ignore rules do not cross
    repository roots, so the product checkout's `.gitignore` says nothing about
    an independent instance repository; a self-contained rule keeps the absolute
    path uncommittable wherever the instance lives.
    """
    document = {
        "schema_version": POINTER_SCHEMA_VERSION,
        "product_root": str(Path(product_root).resolve()),
    }
    path = pointer_path(instance_root)
    _write_text(path.parent / ".gitignore", "*\n")
    _write_json(path, document)
    return document


def read_pointer(instance_root: Path) -> dict[str, Any] | None:
    path = pointer_path(instance_root)
    if not path.exists():
        return None
    try:
        document = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise InstallError(f"product pointer is unreadable: {exc}") from exc
    if (
        not isinstance(document, dict)
        or document.get("schema_version") != POINTER_SCHEMA_VERSION
        or set(document) != {"schema_version", "product_root"}
        or not isinstance(document.get("product_root"), str)
        or not Path(document["product_root"]).is_absolute()
    ):
        raise InstallError("product pointer is invalid")
    return document


def install_mode(product_root: Path, instance_root: Path) -> str:
    """A fused layout is one directory serving as both roots."""
    return (
        "fused"
        if Path(product_root).resolve() == Path(instance_root).resolve()
        else "split"
    )


def product_version(product_root: Path) -> str:
    """The product version is the one the product already publishes."""
    manifest = Path(product_root) / "package.json"
    try:
        version = json.loads(manifest.read_text()).get("version")
    except (OSError, json.JSONDecodeError) as exc:
        raise InstallError(f"product package.json is unreadable: {exc}") from exc
    if not isinstance(version, str) or not version:
        raise InstallError("product package.json declares no version")
    return version


def _path_free_failure(value: str) -> str | None:
    """Reject anything that could only be true on one machine.

    An absolute path, a home-relative path or a Windows drive letter in a
    committed file is the failure this class exists to prevent: it survives the
    commit, travels to the user's other machine and names a directory that is
    not there.
    """
    if value.startswith("~"):
        return "home-relative path"
    if value.startswith("/") or value.startswith("\\"):
        return "absolute path"
    if len(value) >= 3 and value[1] == ":" and value[2] in "\\/":
        return "absolute path"
    if "${" in value:
        return "path token"
    return None


def _check_path_free(document: Any, pointer: str = "") -> None:
    if isinstance(document, dict):
        for key, item in document.items():
            _check_path_free(item, f"{pointer}/{key}")
    elif isinstance(document, list):
        for index, item in enumerate(document):
            _check_path_free(item, f"{pointer}/{index}")
    elif isinstance(document, str):
        failure = _path_free_failure(document)
        if failure is not None:
            raise InstallError(
                f"desired state must stay path-free: {pointer or '/'} is a {failure}"
            )


def validate_desired_state(document: Any) -> dict[str, Any]:
    if not isinstance(document, dict):
        raise InstallError("desired state must be an object")
    if document.get("schema_version") != DESIRED_STATE_SCHEMA_VERSION:
        raise InstallError("desired state schema_version is unsupported")
    if document.get("contract") != DESIRED_STATE_CONTRACT:
        raise InstallError("desired state contract is not installation-desired-state")
    if set(document) != {"schema_version", "contract", "product", "mode"}:
        raise InstallError("desired state has unexpected or missing fields")
    if document.get("mode") not in INSTALL_MODES:
        raise InstallError("desired state mode must be fused or split")
    product = document.get("product")
    if not isinstance(product, dict) or not set(product) <= {"name", "version", "ref"}:
        raise InstallError("desired state product has unexpected or missing fields")
    if not {"name", "version"} <= set(product):
        raise InstallError("desired state product has unexpected or missing fields")
    for field in ("name", "version", "ref"):
        value = product.get(field)
        if field == "ref" and value is None:
            continue
        if not isinstance(value, str) or not value:
            raise InstallError(f"desired state product {field} is invalid")
    _check_path_free(document)
    return document


def build_desired_state(product_root: Path, instance_root: Path) -> dict[str, Any]:
    return {
        "schema_version": DESIRED_STATE_SCHEMA_VERSION,
        "contract": DESIRED_STATE_CONTRACT,
        "product": {"name": PRODUCT_NAME, "version": product_version(product_root)},
        "mode": install_mode(product_root, instance_root),
    }


def load_desired_state(instance_root: Path) -> dict[str, Any] | None:
    path = desired_state_path(instance_root)
    if not path.exists():
        return None
    try:
        document = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise InstallError(f"desired state is unreadable: {exc}") from exc
    return validate_desired_state(document)


def _publish(path: Path, write: Any, mode: str = "w") -> None:
    """Stage beside the destination, then rename over it.

    Renaming replaces whatever is at the destination, including a symlink, so a
    path checked a moment ago cannot be turned into a redirection to somewhere
    outside the instance root before the bytes land.
    """
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        raise InstallError(f"instance directory is not writable: {exc}") from exc
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode,
            dir=path.parent,
            prefix=f".{path.name}.",
            suffix=".tmp",
            delete=False,
        ) as handle:
            temporary = Path(handle.name)
            write(handle)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        temporary = None
    except OSError as exc:
        raise InstallError(f"instance file is not writable: {path.name}: {exc}") from exc
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def _write_json(path: Path, document: dict[str, Any]) -> None:
    def write(handle: Any) -> None:
        json.dump(document, handle, indent=2, sort_keys=True)
        handle.write("\n")

    _publish(path, write)


def _write_text(path: Path, text: str) -> None:
    _publish(path, lambda handle: handle.write(text))


def seed_desired_state(product_root: Path, instance_root: Path) -> tuple[str, dict[str, Any]]:
    """Write the desired state when absent; never rewrite what is there.

    The state is `created` on the first install and `existing` on every later
    one, including an update that ships a different product version. Desired
    state is what the instance wants, so an installer that corrected it would be
    reading its own intent back to the user.
    """
    existing = load_desired_state(instance_root)
    if existing is not None:
        return "existing", existing
    document = validate_desired_state(build_desired_state(product_root, instance_root))
    _write_json(desired_state_path(instance_root), document)
    return "created", document


def seed_instance_files(product_root: Path, instance_root: Path) -> list[dict[str, str]]:
    """Copy each product template into the instance root only when absent."""
    product_root = Path(product_root).resolve()
    instance_root = Path(instance_root).resolve()
    results: list[dict[str, str]] = []
    for relative in SEEDED_FILES:
        source = product_root.joinpath(*relative.split("/"))
        destination = instance_root.joinpath(*relative.split("/"))
        if destination.exists() or destination.is_symlink():
            results.append({"path": relative, "state": "existing"})
            continue
        if source.resolve() == destination:
            # A fused layout has one file that is both template and instance
            # copy; the branch above already reported it.
            results.append({"path": relative, "state": "existing"})
            continue
        if not source.is_file():
            raise InstallError(f"product template is missing: {relative}")
        try:
            template = source.read_bytes()
        except OSError as exc:
            raise InstallError(f"product template is unreadable: {relative}: {exc}") from exc
        # Stage and rename rather than copying onto the checked path: between
        # the existence check above and the write, a symlink planted at the
        # destination would otherwise redirect the bytes out of the instance.
        _publish(destination, lambda handle, data=template: handle.write(data), mode="wb")
        results.append({"path": relative, "state": "created"})
    return results


def execute(action: str, product_root: Path, instance_root: Path) -> dict[str, Any]:
    if action not in {"seed", "show"}:
        raise InstallError(f"unsupported action: {action}")
    product_root = Path(product_root).resolve()
    instance_root = Path(instance_root).resolve()
    if action == "show":
        desired = load_desired_state(instance_root)
        return {
            "schema_version": 1,
            "action": action,
            "mode": install_mode(product_root, instance_root),
            "desired_state": desired,
            "desired_state_state": "existing" if desired is not None else "missing",
            "product_pointer": read_pointer(instance_root),
            "seeded": [
                {
                    "path": relative,
                    "state": (
                        "existing"
                        if instance_root.joinpath(*relative.split("/")).exists()
                        else "missing"
                    ),
                }
                for relative in SEEDED_FILES
            ],
        }
    state, desired = seed_desired_state(product_root, instance_root)
    return {
        "schema_version": 1,
        "action": action,
        "mode": install_mode(product_root, instance_root),
        "desired_state": desired,
        "desired_state_state": state,
        "product_pointer": write_pointer(product_root, instance_root),
        "seeded": seed_instance_files(product_root, instance_root),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("seed", "show"))
    parser.add_argument("--product-root", type=Path, default=ROOT)
    parser.add_argument("--instance-root", type=Path, default=ROOT)
    parser.add_argument("--summary", action="store_true")
    args = parser.parse_args(argv)
    try:
        result = execute(args.action, args.product_root, args.instance_root)
    except (OSError, InstallError) as exc:
        print(f"conflicting: {exc}", file=sys.stderr)
        return 3
    if args.summary:
        created = sum(item["state"] == "created" for item in result["seeded"])
        print(
            f"instance mode={result['mode']} "
            f"desired-state={result['desired_state_state']} "
            f"seeded={created} existing={len(result['seeded']) - created} "
            f"root={args.instance_root}"
        )
    else:
        print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
