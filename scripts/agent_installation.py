"""Managed-link mechanics for root-level Claude agent definitions."""

from __future__ import annotations

from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import sys
import tempfile
from typing import Any


MANIFEST_NAME = ".agent-harness-agents-installation.json"
AGENT_NAME = re.compile(r"^[a-z0-9][a-z0-9-]*\.md$")


class InstallError(ValueError):
    pass


def _now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace(
        "+00:00", "Z"
    )


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _sources(source: Path) -> dict[str, Path]:
    if source.is_symlink():
        raise InstallError("source must not be a symlink")
    if not source.is_dir():
        raise InstallError("source must be an existing agent definition directory")
    paths = sorted(source.glob("*.md"))
    if any(
        not AGENT_NAME.fullmatch(path.name)
        or path.is_symlink()
        or not path.is_file()
        for path in paths
    ):
        raise InstallError("source contains an invalid agent definition")
    return {path.name: path.resolve() for path in paths}


def _manifest_path(target: Path) -> Path:
    return target.parent / MANIFEST_NAME


def _load_manifest(target: Path) -> dict[str, Any]:
    path = _manifest_path(target)
    if not path.exists():
        return {
            "schema_version": 1,
            "owner": "agent-harness",
            "surface": "claude-agents",
            "target_root": str(target.resolve()),
            "updated_at": _now(),
            "managed": {},
        }
    try:
        manifest = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise InstallError(f"agent installation manifest is unreadable: {exc}") from exc
    if (
        not isinstance(manifest, dict)
        or manifest.get("schema_version") != 1
        or manifest.get("owner") != "agent-harness"
        or manifest.get("surface") != "claude-agents"
        or manifest.get("target_root") != str(target.resolve())
        or not isinstance(manifest.get("managed"), dict)
    ):
        raise InstallError("agent installation manifest is invalid")
    required_entry = {
        "owner",
        "source_target",
        "source_sha256",
        "installed_at",
    }
    for name, entry in manifest["managed"].items():
        if (
            not isinstance(name, str)
            or not AGENT_NAME.fullmatch(name)
            or not isinstance(entry, dict)
            or set(entry) != required_entry
            or entry.get("owner") != "agent-harness"
            or not isinstance(entry.get("source_target"), str)
            or not Path(entry["source_target"]).is_absolute()
            or not isinstance(entry.get("source_sha256"), str)
            or not re.fullmatch(r"[0-9a-f]{64}", entry["source_sha256"])
        ):
            raise InstallError(f"agent installation manifest entry is invalid: {name}")
    return manifest


def _same_link(destination: Path, source: Path) -> bool:
    if not destination.is_symlink():
        return False
    try:
        return destination.resolve(strict=False) == source.resolve()
    except (OSError, RuntimeError):
        return False


def _canonical_directory_link(source: Path, target: Path) -> bool:
    if not target.is_symlink():
        return False
    try:
        if target.resolve(strict=True) == source.resolve(strict=True):
            return True
    except (OSError, RuntimeError):
        pass
    raise InstallError("target must not be a non-canonical symlink")


def _same_file_content(destination: Path, source: Path) -> bool:
    if destination.is_symlink() or not destination.is_file():
        return False
    try:
        return _sha256(destination) == _sha256(source)
    except OSError:
        return False


def _replace_link(destination: Path, source: Path) -> None:
    """Publish one staged link; the temporary path stays on the target filesystem."""
    temporary: Path | None = None
    try:
        descriptor, raw_path = tempfile.mkstemp(
            dir=destination.parent,
            prefix=f".{destination.name}.",
            suffix=".tmp",
        )
        os.close(descriptor)
        temporary = Path(raw_path)
        temporary.unlink()
        temporary.symlink_to(source)
        os.replace(temporary, destination)
        temporary = None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def _write_manifest(target: Path, manifest: dict[str, Any]) -> None:
    path = _manifest_path(target)
    manifest["updated_at"] = _now()
    descriptor, raw_path = tempfile.mkstemp(
        dir=path.parent,
        prefix=".agents-installation.",
        suffix=".tmp",
    )
    temporary = Path(raw_path)
    try:
        with os.fdopen(descriptor, "w") as handle:
            json.dump(manifest, handle, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def _state(
    destination: Path,
    source: Path,
    entry: dict[str, Any] | None,
) -> str:
    if entry is None:
        if not destination.exists() and not destination.is_symlink():
            return "missing"
        return "adoptable" if _same_file_content(destination, source) else "unmanaged"
    if _same_link(destination, source):
        return (
            "managed"
            if Path(entry["source_target"]).resolve() == source.resolve()
            and entry["source_sha256"] == _sha256(source)
            else "stale"
        )
    if _same_link(destination, Path(entry["source_target"])):
        return "stale"
    if not destination.exists() and not destination.is_symlink():
        return "stale"
    return "conflicting"


def _entry(source: Path) -> dict[str, str]:
    return {
        "owner": "agent-harness",
        "source_target": str(source),
        "source_sha256": _sha256(source),
        "installed_at": _now(),
    }


def _integrity_state(destination: Path, source: Path) -> str:
    if _same_link(destination, source):
        return "present"
    if not destination.exists() and not destination.is_symlink():
        return "missing"
    if destination.is_symlink():
        return "foreign"
    return "adoptable" if _same_file_content(destination, source) else "noncanonical"


def _raise_on_conflicts(
    states: dict[str, str],
    sources: dict[str, Path],
    target: Path,
    manifest: dict[str, Any],
) -> None:
    unmanaged = [name for name, state in states.items() if state == "unmanaged"]
    if unmanaged:
        remedies = "; ".join(
            f"{name}: manually move or remove {target / name} before rerunning "
            f"to adopt {sources[name]}"
            for name in unmanaged
        )
        raise InstallError("unmanaged agent targets would be overwritten: " + remedies)

    conflicts = [name for name, state in states.items() if state == "conflicting"]
    for name in sorted(set(manifest["managed"]) - set(sources)):
        destination = target / name
        recorded_source = Path(manifest["managed"][name]["source_target"])
        if (destination.exists() or destination.is_symlink()) and not _same_link(
            destination, recorded_source
        ):
            conflicts.append(name)
    if conflicts:
        raise InstallError("conflicting managed targets: " + ", ".join(sorted(conflicts)))


def install(source: Path, target: Path) -> tuple[int, int, list[str]]:
    sources = _sources(source)
    if _canonical_directory_link(source, target):
        return 0, 0, []
    target = target.resolve()
    manifest = _load_manifest(target)
    states = {
        name: _state(target / name, path, manifest["managed"].get(name))
        for name, path in sources.items()
    }
    _raise_on_conflicts(states, sources, target, manifest)

    target.mkdir(parents=True, exist_ok=True)
    newly_managed = [
        name for name, state in states.items() if state in {"missing", "adoptable"}
    ]
    manifest_changed = bool(newly_managed)
    for name in newly_managed:
        manifest["managed"][name] = _entry(sources[name])
    if newly_managed:
        _write_manifest(target, manifest)

    linked = 0
    for name, path in sources.items():
        destination = target / name
        if states[name] in {"missing", "stale", "adoptable"}:
            _replace_link(destination, path)
            manifest["managed"][name] = _entry(path)
            manifest_changed = True
            linked += 1
    for name in sorted(set(manifest["managed"]) - set(sources)):
        destination = target / name
        if destination.is_symlink():
            destination.unlink()
        del manifest["managed"][name]
        manifest_changed = True
    if manifest_changed:
        _write_manifest(target, manifest)

    failures = []
    for name, path in sources.items():
        integrity = _integrity_state(target / name, path)
        if integrity != "present":
            failures.append(f"{name}={integrity}")
    return linked, len(sources) - linked, failures


def _check_items(source: Path, target: Path) -> list[dict[str, str]]:
    sources = _sources(source)
    if _canonical_directory_link(source, target):
        return []
    target = target.resolve()
    manifest = _load_manifest(target)
    items: list[dict[str, str]] = []
    for name, path in sources.items():
        entry = manifest["managed"].get(name)
        state = _integrity_state(target / name, path)
        if entry is None:
            if state not in {"missing", "adoptable"}:
                state = "unmanaged"
        elif state == "present" and (
            Path(entry["source_target"]).resolve() != path.resolve()
            or entry["source_sha256"] != _sha256(path)
        ):
            state = "stale"
        items.append({"name": name, "state": state})
    for name in sorted(set(manifest["managed"]) - set(sources)):
        items.append({"name": name, "state": "retired"})
    known = set(sources) | set(manifest["managed"])
    if target.is_dir():
        for destination in sorted(target.iterdir()):
            if destination.name.endswith(".md") and destination.name not in known:
                items.append({"name": destination.name, "state": "foreign"})
    return items


def check(source: Path, target: Path) -> list[str]:
    items = _check_items(source, target)
    sources = _sources(source)
    target = target.resolve()
    failures = []
    for item in items:
        if item["state"] in {"present", "adoptable"}:
            continue
        if item["state"] == "unmanaged":
            name = item["name"]
            failures.append(
                f"{name}=unmanaged; manually move or remove {target / name} "
                f"before rerunning to adopt {sources[name]}"
            )
        else:
            failures.append(f"{item['name']}={item['state']}")
    return failures


def plan(source: Path, target: Path) -> dict[str, str]:
    sources = _sources(source)
    if _canonical_directory_link(source, target):
        return {}
    target = target.resolve()
    manifest = _load_manifest(target)
    states = {
        name: _state(target / name, path, manifest["managed"].get(name))
        for name, path in sources.items()
    }
    _raise_on_conflicts(states, sources, target, manifest)
    return states


def uninstall_managed(source: Path, target: Path) -> tuple[int, int, list[str]]:
    _sources(source)
    if _canonical_directory_link(source, target):
        return 0, 0, []
    target = target.resolve()
    manifest = _load_manifest(target)
    conflicts = []
    for name, entry in manifest["managed"].items():
        destination = target / name
        if (destination.exists() or destination.is_symlink()) and not _same_link(
            destination, Path(entry["source_target"])
        ):
            conflicts.append(name)
    if conflicts:
        remedies = "; ".join(
            f"{name}: manually restore the managed link to "
            f"{manifest['managed'][name]['source_target']} or remove {target / name}"
            for name in conflicts
        )
        raise InstallError("conflicting managed agent targets changed outside harness: " + remedies)

    removed = 0
    for name in list(manifest["managed"]):
        destination = target / name
        if destination.is_symlink():
            destination.unlink()
        del manifest["managed"][name]
        removed += 1
    if removed:
        _write_manifest(target, manifest)
    return removed, 0, []


def run(action: str, source: Path, target: Path, summary: bool) -> int:
    if action == "validate-sources":
        sources = _sources(source)
        if summary:
            print(f"agents sources={len(sources)} source={source}")
        else:
            print(json.dumps({"schema_version": 1, "action": action, "items": [
                {"name": name, "source": str(path)}
                for name, path in sources.items()
            ], "changed": []}, indent=2))
        return 0
    if action == "plan":
        states = plan(source, target)
        if summary:
            print(f"agents planned={len(states)} target={target}")
        else:
            print(json.dumps({"schema_version": 1, "action": action, "items": [
                {"name": name, "state": state}
                for name, state in states.items()
            ], "changed": []}, indent=2))
        return 0
    if action in {"install", "reconcile", "uninstall-managed"}:
        _sources(source)
        if _canonical_directory_link(source, target):
            if summary:
                print(f"agents existing=directory-link target={target}")
            else:
                print(json.dumps({
                    "schema_version": 1,
                    "action": action,
                    "items": [],
                    "changed": [],
                }, indent=2))
            return 0
    if action in {"install", "reconcile"}:
        before = plan(source, target)
        before_managed = set(_load_manifest(target)["managed"])
        linked, existing, failures = install(source, target)
        after = plan(source, target)
        changed_names = {
            name
            for name in set(before) | set(after)
            if before.get(name) != after.get(name)
        }
        changed_names.update(
            before_managed - set(_load_manifest(target)["managed"])
        )
        if summary:
            print(f"agents linked={linked} existing={existing} target={target}")
        else:
            print(json.dumps({
                "schema_version": 1,
                "action": action,
                "items": [
                    {"name": name, "state": state}
                    for name, state in after.items()
                ],
                "changed": sorted(changed_names),
            }, indent=2))
        if failures:
            print(
                "conflicting: agent installation integrity failed: "
                + ", ".join(failures),
                file=sys.stderr,
            )
            return 3
        return 0
    if action == "check":
        items = _check_items(source, target)
        failures = check(source, target)
        if summary:
            print(f"agents checked={len(_sources(source))} target={target}")
        else:
            print(json.dumps({
                "schema_version": 1,
                "action": action,
                "items": items,
                "changed": [],
            }, indent=2))
        if failures:
            print(
                "conflicting: agent installation integrity failed: "
                + ", ".join(failures),
                file=sys.stderr,
            )
            return 3
        return 0
    if action == "uninstall-managed":
        before_managed = set(_load_manifest(target)["managed"])
        removed, _, failures = uninstall_managed(source, target)
        changed = sorted(
            before_managed - set(_load_manifest(target)["managed"])
        )
        if summary:
            print(f"agents uninstalled={removed} target={target}")
        else:
            print(json.dumps({
                "schema_version": 1,
                "action": action,
                "items": [],
                "changed": changed,
            }, indent=2))
        if failures:
            print(
                "conflicting: agent installation integrity failed: "
                + ", ".join(failures),
                file=sys.stderr,
            )
            return 3
        return 0
    raise InstallError(f"unsupported agents action: {action}")
