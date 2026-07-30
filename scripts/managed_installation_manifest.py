"""Schema and persistence for managed-skill manifests."""

from __future__ import annotations

from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile
from typing import Any


MANIFEST_NAME = ".agent-harness-installation.json"
SKILL_NAME = re.compile(r"^[a-z0-9][a-z0-9-]*$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
# Shared library directories sit beside the skills, carry no SKILL.md and are
# never renamed through the skill rename registry, so they keep their own name.
SHARED_NAMES = ("_shared",)


def is_managed_name(name: str) -> bool:
    """Managed entries are the skills plus the shared libraries beside them."""
    return name in SHARED_NAMES or bool(SKILL_NAME.fullmatch(name))


class InstallError(ValueError):
    pass


def now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace(
        "+00:00", "Z"
    )


def sha_skill(path: Path) -> str:
    digest = hashlib.sha256()
    for child in sorted(path.rglob("*")):
        relative = child.relative_to(path)
        if (
            any(part in {"__pycache__", ".DS_Store"} for part in relative.parts)
            or child.suffix == ".pyc"
        ):
            continue
        if child.is_symlink():
            raise InstallError(f"skill source contains a symlink: {relative}")
        if not child.is_file():
            continue
        digest.update(relative.as_posix().encode())
        digest.update(b"\0")
        digest.update(b"x" if child.stat().st_mode & 0o111 else b"-")
        digest.update(b"\0")
        digest.update(child.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def manifest_path(target: Path) -> Path:
    return target.parent / MANIFEST_NAME


def empty_manifest(target: Path) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "owner": "agent-harness",
        "target_root": str(target.resolve()),
        "updated_at": now(),
        "managed": {},
        "custom": {},
    }


def load_manifest(target: Path) -> dict[str, Any]:
    path = manifest_path(target)
    if not path.exists():
        return empty_manifest(target)
    try:
        data = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise InstallError(f"installation manifest is unreadable: {exc}") from exc
    if (
        not isinstance(data, dict)
        or data.get("schema_version") != 1
        or data.get("owner") != "agent-harness"
        or not isinstance(data.get("managed"), dict)
    ):
        raise InstallError("installation manifest is invalid or not owned by agent-harness")
    if data.get("target_root") != str(target.resolve()):
        raise InstallError("installation manifest belongs to a different target root")
    custom = data.setdefault("custom", {})
    if not isinstance(custom, dict):
        raise InstallError("installation manifest custom links are invalid")
    required_entry = {"owner", "source_target", "source_sha256", "installed_at", "history"}
    for name, item in data["managed"].items():
        if not isinstance(name, str) or not is_managed_name(name):
            raise InstallError("installation manifest contains an invalid skill name")
        if (
            not isinstance(item, dict)
            or set(item) != required_entry
            or item.get("owner") != "agent-harness"
        ):
            raise InstallError(f"installation manifest entry is invalid: {name}")
        if not isinstance(item.get("source_target"), str) or not Path(
            item["source_target"]
        ).is_absolute():
            raise InstallError(f"installation manifest source target is invalid: {name}")
        if not isinstance(item.get("source_sha256"), str) or not SHA256.fullmatch(
            item["source_sha256"]
        ):
            raise InstallError(f"installation manifest digest is invalid: {name}")
        if not isinstance(item.get("history"), list):
            raise InstallError(f"installation manifest history is invalid: {name}")
    for name, item in custom.items():
        if not isinstance(name, str) or not SKILL_NAME.fullmatch(name):
            raise InstallError("installation manifest contains an invalid custom skill name")
        if (
            not isinstance(item, dict)
            or set(item) != {"source_target"}
            or not isinstance(item.get("source_target"), str)
            or not Path(item["source_target"]).is_absolute()
        ):
            raise InstallError(f"installation manifest custom link is invalid: {name}")
    return data


def write_manifest(target: Path, manifest: dict[str, Any]) -> None:
    path = manifest_path(target)
    path.parent.mkdir(parents=True, exist_ok=True)
    manifest["updated_at"] = now()
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w",
            dir=path.parent,
            prefix=".installation.",
            suffix=".tmp",
            delete=False,
        ) as handle:
            temporary = Path(handle.name)
            json.dump(manifest, handle, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        temporary = None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def entry(
    _name: str,
    source: Path,
    history: list[dict[str, str]] | None = None,
) -> dict[str, Any]:
    return {
        "owner": "agent-harness",
        "source_target": str(source),
        "source_sha256": sha_skill(source),
        "installed_at": now(),
        "history": history or [],
    }
