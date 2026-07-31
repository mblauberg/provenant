#!/usr/bin/env python3
"""Plan, install, reconcile or remove only harness-managed skill links."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import tempfile
import sys
from typing import Any

try:
    import scripts.managed_installation_manifest as manifest_io
except ModuleNotFoundError as exc:
    if exc.name != "scripts":
        raise
    import managed_installation_manifest as manifest_io  # type: ignore[no-redef]


ROOT = Path(__file__).resolve().parents[1]
SKILL_NAME = manifest_io.SKILL_NAME
SHARED_NAMES = manifest_io.SHARED_NAMES
InstallError = manifest_io.InstallError
_now = manifest_io.now
_sha_skill = manifest_io.sha_skill
_load_manifest = manifest_io.load_manifest
_entry = manifest_io.entry


def _skills(
    source: Path,
    *,
    optional: bool = False,
    validate_content: bool = True,
) -> dict[str, Path]:
    if not source.is_dir():
        if optional and not source.exists() and not source.is_symlink():
            return {}
        raise InstallError("source must be an existing skill directory")
    if validate_content:
        for child in sorted(source.rglob("*")):
            if child.is_symlink():
                raise InstallError(
                    f"skill source contains a symlink: {child.relative_to(source)}"
                )
    skills = {
        path.parent.name: path.parent.resolve()
        for path in sorted(source.glob("*/SKILL.md"))
    }
    if any(not SKILL_NAME.fullmatch(name) for name in skills):
        raise InstallError("source contains an invalid skill name")
    return skills


def _shared(source: Path) -> dict[str, Path]:
    """Report shared libraries the skills import; they carry no SKILL.md."""
    return {
        name: (source / name).resolve()
        for name in SHARED_NAMES
        if (source / name).is_dir() and not (source / name).is_symlink()
    }


def _catalogues(
    source: Path,
    custom_source: Path | None = None,
) -> tuple[dict[str, Path], dict[str, Path]]:
    """Every directory a per-entry installation owns: skills and shared libraries.

    A whole-directory projection exposes both without a manifest, so the
    catalogue only drives per-entry layouts.
    """
    managed = {**_skills(source), **_shared(source)}
    custom = (
        _skills(custom_source, optional=True, validate_content=False)
        if custom_source is not None
        else {}
    )
    collisions = sorted(set(managed) & set(custom))
    if collisions:
        details = "; ".join(
            f"{managed[name]} conflicts with {custom[name]}" for name in collisions
        )
        raise InstallError(f"skill source name collision: {details}")
    return managed, custom


def _catalogue(
    source: Path,
    custom_source: Path | None = None,
) -> dict[str, Path]:
    managed, custom = _catalogues(source, custom_source)
    return {**managed, **custom}


def _same_link(destination: Path, source: Path) -> bool:
    if not destination.is_symlink():
        return False
    try:
        return destination.resolve(strict=False) == source.resolve()
    except (OSError, RuntimeError):
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


def _plan(
    managed_catalogue: dict[str, Path],
    custom_catalogue: dict[str, Path],
    target: Path,
    manifest: dict[str, Any],
    *,
    custom_source_provided: bool,
) -> list[dict[str, str]]:
    managed = manifest["managed"]
    custom = manifest["custom"]
    items: list[dict[str, str]] = []
    for name, source_path in managed_catalogue.items():
        if not custom_source_provided and name in custom:
            continue
        destination = target / name
        entry = managed.get(name)
        if entry is None:
            previous_custom = custom.get(name)
            if previous_custom is not None and name not in custom_catalogue:
                if _same_link(
                    destination, Path(previous_custom["source_target"])
                ):
                    state = "custom-to-managed"
                elif destination.exists() or destination.is_symlink():
                    state = "conflicting"
                else:
                    state = "missing"
            else:
                state = (
                    "unmanaged"
                    if destination.exists() or destination.is_symlink()
                    else "missing"
                )
        elif _same_link(destination, source_path):
            state = "managed" if entry.get("source_sha256") == _sha_skill(source_path) else "stale"
        elif destination.is_symlink() and not destination.exists():
            state = "stale"
        elif not destination.exists() and not destination.is_symlink():
            state = "stale"
        else:
            state = "conflicting"
        items.append({"name": name, "state": state})
    for name in sorted(
        set(managed) - set(managed_catalogue) - set(custom_catalogue)
    ):
        destination = target / name
        if _same_link(destination, Path(managed[name].get("source_target", "/missing"))):
            state = "retired-managed"
        elif not destination.exists() and not destination.is_symlink():
            state = "retired-missing"
        else:
            state = "conflicting"
        items.append({"name": name, "state": state})
    for name, source_path in custom_catalogue.items():
        destination = target / name
        entry = custom.get(name)
        if entry is None:
            previous_managed = managed.get(name)
            if previous_managed is not None and name not in managed_catalogue:
                if _same_link(
                    destination, Path(previous_managed["source_target"])
                ):
                    state = "managed-to-custom"
                else:
                    state = "conflicting"
            elif _same_link(destination, source_path):
                state = "custom-untracked"
            elif destination.exists() or destination.is_symlink():
                state = "conflicting"
            else:
                state = "custom-missing"
        else:
            recorded_source = Path(entry["source_target"])
            if recorded_source.resolve() != source_path:
                state = (
                    "custom-rebind"
                    if _same_link(destination, recorded_source)
                    else "custom-conflicting"
                )
            else:
                state = (
                    "custom"
                    if _same_link(destination, source_path)
                    else "custom-conflicting"
                )
        items.append({"name": name, "state": state})
    if custom_source_provided:
        for name in sorted(
            set(custom) - set(custom_catalogue) - set(managed_catalogue)
        ):
            destination = target / name
            old_source = Path(custom[name]["source_target"])
            state = (
                "custom-retired"
                if _same_link(destination, old_source)
                else "custom-retired-missing"
                if not destination.exists() and not destination.is_symlink()
                else "custom-conflicting"
            )
            items.append({"name": name, "state": state})
    return items


def _integrity(
    managed_catalogue: dict[str, Path],
    custom_catalogue: dict[str, Path],
    target: Path,
    manifest: dict[str, Any],
) -> list[dict[str, str]]:
    """Report the installed catalogue without claiming ownership of foreign entries."""
    catalogue = {**managed_catalogue, **custom_catalogue}
    items: list[dict[str, str]] = []
    for name, source_path in catalogue.items():
        destination = target / name
        owner = "custom" if name in custom_catalogue else "managed"
        if owner == "custom" and (
            not source_path.is_dir()
            or not (source_path / "SKILL.md").is_file()
        ):
            state = "missing-source"
        elif _same_link(destination, source_path):
            state = "present"
        elif not destination.exists() and not destination.is_symlink():
            state = "missing"
        elif destination.is_symlink():
            state = "foreign"
        elif name in custom_catalogue:
            state = "noncanonical"
        else:
            try:
                state = "present" if _sha_skill(destination) == _sha_skill(source_path) else "noncanonical"
            except (OSError, InstallError):
                state = "noncanonical"
        items.append(
            {
                "name": name,
                "owner": owner,
                "scope": "required",
                "source_target": str(source_path),
                "state": state,
            }
        )

    if target.is_dir():
        for destination in sorted(target.iterdir()):
            name = destination.name
            if name == ".DS_Store" or name in catalogue or not destination.is_symlink():
                continue
            try:
                resolved = destination.resolve(strict=False)
                if not any(
                    resolved.is_relative_to(source_path.parent)
                    for source_path in catalogue.values()
                ):
                    raise ValueError
                state = "noncanonical"
            except (OSError, ValueError):
                state = "foreign"
            items.append({"name": name, "scope": "extra", "state": state})
    return items


def _write_manifest(target: Path, manifest: dict[str, Any]) -> None:
    manifest_io.write_manifest(target, manifest)


def _raise_on_conflicts(
    items: list[dict[str, str]],
    manifest: dict[str, Any],
    *,
    excluded_names: set[str],
) -> None:
    custom_conflicts = [
        item["name"]
        for item in items
        if item["state"] == "custom-conflicting"
        and item["name"] not in excluded_names
    ]
    if custom_conflicts:
        remedies = "; ".join(
            "manually restore "
            f"{name} from {manifest['custom'][name]['source_target']} "
            "or provide --custom-source pointing to the intended custom skills directory"
            for name in custom_conflicts
        )
        raise InstallError(f"custom targets changed outside harness: {remedies}")
    conflicts = [
        item["name"]
        for item in items
        if item["state"] == "conflicting"
        and item["name"] not in excluded_names
    ]
    if conflicts:
        raise InstallError(
            "conflicting managed targets changed outside harness: "
            + ", ".join(conflicts)
        )


def _load_renames(path: Path | None) -> list[dict[str, str]]:
    if path is None:
        return []
    try:
        data = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise InstallError(f"rename registry is unreadable: {exc}") from exc
    renames = data.get("renames") if isinstance(data, dict) and data.get("schema_version") == 1 else None
    if not isinstance(renames, list) or any(
        not isinstance(item, dict)
        or set(item) != {"from", "to"}
        or not SKILL_NAME.fullmatch(str(item["from"]))
        or not SKILL_NAME.fullmatch(str(item["to"]))
        for item in renames
    ):
        raise InstallError("rename registry is invalid")
    return renames


def _prepare_renames(
    source: Path,
    target: Path,
    manifest: dict[str, Any],
    renames: list[dict[str, str]],
) -> list[dict[str, Any]]:
    skills = _skills(source)
    managed = manifest["managed"]
    operations: list[dict[str, Any]] = []
    # Several sources may converge on one target: only the first such rename
    # creates the shared link, while all source history remains on the target.
    creating: set[str] = set()
    target_history: dict[str, list[dict[str, str]]] = {}
    for rename in renames:
        old, new = rename["from"], rename["to"]
        if old not in managed or new not in skills:
            continue
        # A manifest-recorded custom name stays custom-owned even while its
        # projected link is missing, so a managed rename may never claim it.
        if new in manifest["custom"]:
            raise InstallError(
                f"managed rename target {new} is a recorded custom skill: "
                f"manually restore {new} from "
                f"{manifest['custom'][new]['source_target']} or retire the "
                "custom record via --custom-source before renaming onto it"
            )
        old_destination = target / old
        new_destination = target / new
        old_source = Path(managed[old]["source_target"])
        if (old_destination.exists() or old_destination.is_symlink()) and not _same_link(old_destination, old_source):
            raise InstallError(f"conflicting managed rename source: {old}")
        new_is_correct = _same_link(new_destination, skills[new])
        if (new_destination.exists() or new_destination.is_symlink()) and not new_is_correct:
            raise InstallError(f"conflicting rename target: {new}")
        if new_is_correct and new in managed and Path(managed[new]["source_target"]).resolve() != skills[new]:
            raise InstallError(f"conflicting managed rename target: {new}")
        if new not in target_history:
            target_history[new] = list(managed.get(new, {}).get("history", []))
        target_history[new].extend(managed.get(old, {}).get("history", []))
        target_history[new].append({"from": old, "to": new, "at": _now()})
        create_new = not new_is_correct and new not in creating
        if create_new:
            creating.add(new)
        operations.append({
            "old": old,
            "new": new,
            "old_destination": old_destination,
            "old_source": old_source,
            "new_destination": new_destination,
            "new_source": skills[new],
            "create_new": create_new,
            "manage_new": new in managed or new in creating,
            "entry": _entry(new, skills[new], list(target_history[new])),
        })
    return operations


def _apply_renames(
    manifest: dict[str, Any],
    operations: list[dict[str, Any]],
) -> None:
    for operation in operations:
        if operation["create_new"]:
            _replace_link(operation["new_destination"], operation["new_source"])
        if operation["old_destination"].is_symlink():
            operation["old_destination"].unlink()
    for operation in operations:
        if operation["manage_new"]:
            manifest["managed"][operation["new"]] = operation["entry"]
        manifest["managed"].pop(operation["old"], None)


def execute(
    action: str,
    source: Path,
    target: Path,
    renames: Path | None = None,
    custom_source: Path | None = None,
) -> dict[str, Any]:
    if action not in {
        "validate-sources",
        "plan",
        "check",
        "install",
        "reconcile",
        "uninstall-managed",
    }:
        raise InstallError(f"unsupported action: {action}")
    source = Path(source)
    if source.is_symlink():
        raise InstallError("skill source contains a symlink: source")
    source = source.resolve()
    if custom_source is not None:
        custom_source = Path(custom_source)
        if custom_source.is_symlink():
            raise InstallError("skill source contains a symlink: custom source")
        custom_source = custom_source.resolve()
    managed_catalogue, custom_catalogue = _catalogues(source, custom_source)
    custom_source_provided = custom_source is not None
    if action == "validate-sources":
        return {
            "schema_version": 1,
            "action": action,
            "items": [
                {"name": name, "source": str(path)}
                for name, path in {
                    **managed_catalogue,
                    **custom_catalogue,
                }.items()
            ],
            "changed": [],
        }
    target = Path(target).resolve()
    manifest = _load_manifest(target)
    if action == "plan":
        return {
            "schema_version": 1,
            "action": action,
            "items": _plan(
                managed_catalogue,
                custom_catalogue,
                target,
                manifest,
                custom_source_provided=custom_source_provided,
            ),
            "changed": [],
        }
    if action == "check":
        integrity_custom_catalogue = (
            custom_catalogue
            if custom_source_provided
            else {
                name: Path(entry["source_target"])
                for name, entry in manifest["custom"].items()
            }
        )
        items = _integrity(
            managed_catalogue,
            integrity_custom_catalogue,
            target,
            manifest,
        )
        return {
            "schema_version": 1,
            "action": action,
            "ok": all(item["state"] == "present" for item in items if item["scope"] == "required"),
            "items": items,
            "changed": [],
        }
    rename_operations: list[dict[str, Any]] = []
    if action == "reconcile":
        rename_operations = _prepare_renames(source, target, manifest, _load_renames(renames))
    items = _plan(
        managed_catalogue,
        custom_catalogue,
        target,
        manifest,
        custom_source_provided=custom_source_provided,
    )
    renamed_old = {operation["old"] for operation in rename_operations}
    _raise_on_conflicts(items, manifest, excluded_names=renamed_old)
    target.mkdir(parents=True, exist_ok=True)
    changed: list[str] = [operation["new"] for operation in rename_operations]
    if rename_operations:
        _apply_renames(manifest, rename_operations)
        items = _plan(
            managed_catalogue,
            custom_catalogue,
            target,
            manifest,
            custom_source_provided=custom_source_provided,
        )
        _raise_on_conflicts(items, manifest, excluded_names=set())
    if action in {"install", "reconcile"}:
        for item in items:
            name, state = item["name"], item["state"]
            if state in {"missing", "stale", "custom-to-managed"}:
                destination = target / name
                _replace_link(destination, managed_catalogue[name])
                history = list(manifest["managed"].get(name, {}).get("history", []))
                manifest["managed"][name] = _entry(
                    name, managed_catalogue[name], history
                )
                manifest["custom"].pop(name, None)
                changed.append(name)
            elif state == "retired-managed":
                (target / name).unlink()
                del manifest["managed"][name]
                changed.append(name)
            elif state == "retired-missing":
                del manifest["managed"][name]
                changed.append(name)
            elif state in {
                "custom-missing",
                "custom-untracked",
                "custom-rebind",
                "managed-to-custom",
            }:
                if state in {
                    "custom-missing",
                    "custom-rebind",
                    "managed-to-custom",
                }:
                    _replace_link(target / name, custom_catalogue[name])
                manifest["custom"][name] = {
                    "source_target": str(custom_catalogue[name])
                }
                manifest["managed"].pop(name, None)
                changed.append(name)
            elif state == "custom-retired":
                (target / name).unlink()
                del manifest["custom"][name]
                changed.append(name)
            elif state == "custom-retired-missing":
                del manifest["custom"][name]
                changed.append(name)
    elif action == "uninstall-managed":
        for name, entry in list(manifest["managed"].items()):
            destination = target / name
            if _same_link(destination, Path(entry.get("source_target", "/missing"))):
                destination.unlink()
                del manifest["managed"][name]
                changed.append(name)
            elif destination.exists() or destination.is_symlink():
                raise InstallError(f"conflicting managed target changed outside harness: {name}")
            else:
                del manifest["managed"][name]
                changed.append(name)
    if changed:
        _write_manifest(target, manifest)
    return {
        "schema_version": 1,
        "action": action,
        "items": _plan(
            managed_catalogue,
            custom_catalogue,
            target,
            manifest,
            custom_source_provided=custom_source_provided,
        ),
        "changed": changed,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action",
        choices=(
            "validate-sources",
            "plan",
            "check",
            "install",
            "reconcile",
            "uninstall-managed",
        ),
    )
    parser.add_argument("--target", required=True, type=Path)
    parser.add_argument("--source", type=Path, default=ROOT / "skills")
    parser.add_argument("--custom-source", type=Path)
    parser.add_argument("--renames", type=Path)
    parser.add_argument("--summary", action="store_true")
    args = parser.parse_args(argv)
    try:
        result = execute(
            args.action,
            args.source,
            args.target,
            args.renames,
            args.custom_source,
        )
    except (OSError, InstallError) as exc:
        print(f"conflicting: {exc}", file=sys.stderr)
        return 3
    if args.action == "check":
        warnings = [item for item in result["items"] if item["scope"] == "extra" and item["state"] != "present"]
        if warnings:
            detail = ", ".join(f'{item["name"]}={item["state"]}' for item in warnings)
            print(f"warning: unmanaged skill links outside catalogue: {detail}", file=sys.stderr)
    if args.action == "check" and not result["ok"]:
        print(json.dumps(result, indent=2))
        custom_failures = [
            item
            for item in result["items"]
            if item.get("owner") == "custom"
            and item["scope"] == "required"
            and item["state"] != "present"
        ]
        if custom_failures:
            remedies = "; ".join(
                "manually restore "
                f"{item['name']} from {item['source_target']} "
                "or provide --custom-source pointing to the intended custom skills directory"
                for item in custom_failures
            )
            print(
                f"conflicting: custom skill installation integrity failed: {remedies}",
                file=sys.stderr,
            )
            return 3
        failures = ", ".join(
            f'{item["name"]}={item["state"]}'
            for item in result["items"]
            if item["scope"] == "required" and item["state"] != "present"
        )
        print(f"conflicting: skill installation integrity failed: {failures}", file=sys.stderr)
        return 3
    if args.summary:
        if args.action == "check":
            print(f"skills checked={len(result['items'])} target={args.target}")
            return 0
        linked = len(result["changed"])
        existing = sum(
            item["state"] in {"managed", "unmanaged", "custom"}
            for item in result["items"]
        ) - linked
        print(f"skills linked={linked} existing={existing} target={args.target}")
    else:
        print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
