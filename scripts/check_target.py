#!/usr/bin/env python3
"""Select the registered checkout that owns `provenant check`."""

from __future__ import annotations

import os
from pathlib import Path
import sys


REPAIR = "repair: run ./scripts/check-harness from a registered checkout"


def nearest_git_metadata(root: Path) -> Path | None:
    """Return the nearest .git entry without following or trusting it."""
    requested = root.resolve()
    for ancestor in (requested, *requested.parents):
        candidate = ancestor / ".git"
        try:
            candidate.lstat()
        except FileNotFoundError:
            continue
        except OSError as exc:
            raise ValueError(f"cannot inspect Git metadata at {candidate}: {exc}") from exc
        return candidate
    return None


def registered_caller_checkout(product_root: Path) -> Path | None:
    """Return the registered product checkout containing the caller, if any."""
    try:
        from worktree import PolicyError, common_git_dir, owning_root, worktree_records
    except ImportError as exc:
        raise ValueError(f"worktree policy owner is unavailable; {REPAIR}") from exc
    try:
        caller = Path.cwd().resolve()
    except OSError as exc:
        raise ValueError("cannot resolve the working directory; repair: cd into the checkout you intend to check") from exc
    product_metadata = product_root.resolve() / ".git"
    try:
        product_metadata.lstat()
    except FileNotFoundError:
        return None
    except OSError as exc:
        raise ValueError(f"cannot inspect product Git metadata at {product_metadata}: {exc}") from exc
    try:
        records = worktree_records(product_root)
    except PolicyError as exc:
        raise ValueError(f"cannot verify product checkout {product_root}: {exc}") from exc
    registered = [(Path(str(record["worktree"])), Path(str(record["worktree"])).resolve())
                  for record in records if record.get("worktree") is not None]
    resolved_product = product_root.resolve()
    product_records = [raw for raw, resolved in registered if resolved == resolved_product]
    if not product_records or any(path.is_symlink() for path in product_records):
        raise ValueError(f"product checkout {resolved_product} is not registered in its Git repository; "
                         "repair: run ./scripts/check-harness from a registered checkout")
    containing = [(raw, checkout) for raw, checkout in registered
                  if caller == checkout or caller.is_relative_to(checkout)]
    selected_record = max(containing, key=lambda item: len(item[1].parts)) if containing else None
    if selected_record is not None and selected_record[0].is_symlink():
        raise ValueError(f"registered checkout path is a symlink: {selected_record[0]}")
    selected = selected_record[1] if selected_record is not None else None
    if selected is not None:
        try:
            selected_root = owning_root(selected)
            selected_common = common_git_dir(selected_root)
            product_common = common_git_dir(resolved_product)
        except PolicyError as exc:
            raise ValueError(f"cannot verify Git ownership for {selected}: {exc}") from exc
        if selected_root != selected:
            raise ValueError(f"registered checkout identity does not match {selected}")
        if selected_common != product_common:
            raise ValueError(f"registered checkout {selected} belongs to another Git repository")
    caller_metadata = nearest_git_metadata(caller)
    if caller_metadata is None:
        if selected is not None:
            raise ValueError(f"registered checkout {selected} has no .git metadata")
        return selected
    if selected is not None and caller_metadata == selected / ".git":
        return selected
    try:
        caller_root = owning_root(caller)
    except PolicyError as exc:
        raise ValueError(f"invalid Git metadata at {caller_metadata}; repair: run ./scripts/check-harness "
                         "from the checkout you intend to check") from exc
    try:
        caller_common = common_git_dir(caller_root)
        product_common = common_git_dir(resolved_product)
    except PolicyError as exc:
        raise ValueError(f"cannot verify caller Git ownership: {exc}") from exc
    if caller_common == product_common:
        raise ValueError(f"caller checkout {caller_root} is not a registered worktree of product root "
                         f"{resolved_product}; repair: run ./scripts/check-harness from the checkout you "
                         "intend to check")
    return selected


def checked_target(root: Path) -> Path:
    """Require the selected checkout's own regular executable checker."""
    target = root / "scripts/check-harness"
    if target.is_symlink() or not target.is_file() or not os.access(target, os.X_OK):
        raise ValueError(f"selected checkout {root} has no regular executable scripts/check-harness; "
                         "repair: restore that checkout or run the checker directly from another checkout")
    if not target.resolve().is_relative_to(root.resolve()):
        raise ValueError(f"selected checker {target} resolves outside checkout {root}")
    return target


def check_target() -> tuple[Path, Path]:
    local_root = Path(__file__).resolve().parent.parent
    caller_root = registered_caller_checkout(local_root)
    selected = caller_root or local_root
    return checked_target(selected), selected


def main() -> int:
    try:
        checker, root = check_target()
    except (OSError, ValueError) as exc:
        print(f"provenant: {exc}", file=sys.stderr)
        return 3
    sys.stdout.buffer.write(os.fsencode(checker) + b"\0" + os.fsencode(root) + b"\0")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
