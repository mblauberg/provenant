"""Protected instruction snapshot quarantine helpers for provider attempts."""

import os
from pathlib import Path
import stat
import subprocess
import tempfile

try:
    from . import provider_exec
    from .output_custody import install
except ImportError:
    import provider_exec
    from output_custody import install

QUARANTINE_IDENTITY = {"GIT_AUTHOR_NAME": "Provenant Fabric", "GIT_AUTHOR_EMAIL": "fabric@provenant.invalid",
                       "GIT_COMMITTER_NAME": "Provenant Fabric", "GIT_COMMITTER_EMAIL": "fabric@provenant.invalid"}


def _git_bytes(cwd, *args, input=None, env=None):
    """_git_output for object writes: raw bytes in and out, and no hook or signing command either."""
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    result = subprocess.run(
        ["git", "-C", str(cwd), "-c", "core.fsmonitor=false", "-c", "protocol.allow=never",
         "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgSign=false", *args],
        input=input, capture_output=True, timeout=30,
        env={**environment, "GIT_NO_LAZY_FETCH": "1", **(env or {})},
    )
    if result.returncode != 0:
        raise ValueError(os.fsdecode(result.stderr).strip() or f"git {args[0]} failed")
    return result.stdout


def quarantinable(changes):
    """Ordinary paths below .agents/ the owner can archive and restore safely."""
    return all(change.startswith(provider_exec.INSTRUCTION_DIR + "/") and not change.endswith(" (unresolved conflict)")
               for change in changes)


def _is_ancestor(cwd, ancestor, descendant):
    try:
        _git_bytes(cwd, "merge-base", "--is-ancestor", ancestor, descendant)
    except ValueError:
        return False
    return True


def _quarantine_target(cwd, start_head):
    """The .agents entries a quarantined path returns to.

    That is the start commit's, or an integration ref's the lane has since merged or rebased onto,
    taking each path from whichever side changed it since their merge base.
    """
    def tree(rev):
        return provider_exec._instruction_entries(cwd, ("ls-tree", "-r", "-z", "--full-tree", rev))[0]

    target = tree(start_head)
    for ref in provider_exec._integration_refs(cwd):
        if not _is_ancestor(cwd, ref, "HEAD") or _is_ancestor(cwd, ref, start_head):
            continue
        incoming = tree(ref)
        if _is_ancestor(cwd, start_head, ref):
            target = incoming
            continue
        base = tree(provider_exec._git_output(cwd, "merge-base", start_head, ref).strip())
        target = {path: entry for path in set(target) | set(incoming)
                  if (entry := (incoming if target.get(path) == base.get(path) else target).get(path))}
    return target


def _restore_views(cwd, start):
    """What each view of a quarantined .agents path returns to.

    HEAD returns to the quarantine target. The index and files return to their starting content,
    except a path an integration branch the lane took in changed, which takes that branch's version.
    """
    target = _quarantine_target(cwd, start["head"])
    integrated = {path for path in set(target) | set(start["tree"]) if target.get(path) != start["tree"].get(path)}

    def view(entries):
        return {path: entry for path in set(entries) | integrated
                if (entry := (target if path in integrated else entries).get(path))}

    return {"head": target, "index": view(start["index"]), "disk": view(start["disk"]), "integrated": integrated}


def _store_blob(cwd, path):
    """Write the file or link at a worktree path to the object store without filters; None when absent."""
    file = Path(cwd).resolve() / path
    # A path under a link or a replaced directory is not the lane's file at that path.
    if file.parent.resolve() != file.parent or not os.path.lexists(file):
        return None
    status = file.lstat()
    if stat.S_ISLNK(status.st_mode):
        mode, data = "120000", os.fsencode(os.readlink(file))
    elif stat.S_ISREG(status.st_mode):
        mode = "100755" if status.st_mode & 0o100 else "100644"
        with open(os.open(file, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), "rb") as handle:
            data = handle.read()
    else:
        raise ValueError(f"{path} is not a file or link")
    return mode, os.fsdecode(_git_bytes(cwd, "hash-object", "-w", "--no-filters", "--stdin", input=data)).strip()


def _index_info(entries, paths, zero):
    """update-index -z --index-info input setting each path to its entry, removals first."""
    removals = [f"0 {zero}\t{path}\0" for path in paths if path not in entries]
    updates = [f"{entries[path][0]} {entries[path][1]}\t{path}\0" for path in paths if path in entries]
    return os.fsencode("".join(removals + updates))


QUARANTINE_PATCHES = (("disk", "protected.patch"), ("index", "protected.index.patch"),
                      ("head", "protected.head.patch"))


def quarantine_instruction_changes(cwd, start, changes, attempt_dir, *, restore_changes=True, published=None):
    """Move a lane's authored .agents changes out of its worktree into patches the chair applies.

    Each of the lane's files, index and HEAD is archived as a patch from the view's restore point
    (protected.patch for files; index and HEAD patches only when they hold something else). A commit,
    the index and the files then return to their restore points. Blobs are hashed and written
    without filters, so no command the repository configures runs. With restore_changes=False,
    archive only. Returns the patches written; protected.patch may be empty for an index-only edit.
    """
    root = Path(cwd).resolve()
    paths = sorted(changes)
    restore = _restore_views(cwd, start)
    zero = "0" * (64 if start["object_format"] == "sha256" else 40)
    head = os.fsdecode(_git_bytes(cwd, "rev-parse", "--verify", "HEAD^{commit}")).strip()
    lane = {"head": provider_exec._instruction_entries(cwd, ("ls-tree", "-r", "-z", "--full-tree", head))[0],
            "index": provider_exec._instruction_entries(cwd, ("ls-files", "--stage", "-z"), index=True)[0],
            "disk": {path: entry for path in paths if (entry := _store_blob(cwd, path))}}
    if published is None:
        published = []
    # Scratch files stay out of the attempt directory, which a lane may be able to write.
    with tempfile.TemporaryDirectory() as private:
        scratch_index = Path(private, "index")

        def tree_of(entries, base=None):
            scratch_index.unlink(missing_ok=True)
            environment = {"GIT_INDEX_FILE": str(scratch_index)}
            if base is not None:
                _git_bytes(cwd, "read-tree", base, env=environment)
            _git_bytes(cwd, "update-index", "-z", "--index-info", input=_index_info(entries, paths, zero),
                       env=environment)
            return os.fsdecode(_git_bytes(cwd, "write-tree", env=environment)).strip()

        written = set()
        for view, name in QUARANTINE_PATCHES:
            patch = _git_bytes(cwd, "diff", "--binary", "--full-index", "--no-renames", "--no-ext-diff",
                               "--no-textconv", "--no-color", "--src-prefix=a/", "--dst-prefix=b/",
                               tree_of(restore[view]), tree_of(lane[view]))
            if name == "protected.patch" or (patch and patch not in written):
                written.add(patch)
                Path(private, name).write_bytes(patch)
                # Replace, never follow, whatever the lane left at the destination.
                install(str(Path(private, name)), str(Path(attempt_dir, name)))
                published.append(Path(attempt_dir, name))
        if not restore_changes:
            return published
        tree = tree_of(restore["head"], base=head)
        if tree != os.fsdecode(_git_bytes(cwd, "rev-parse", head + "^{tree}")).strip():
            message = (f"fabric: quarantine protected {provider_exec.INSTRUCTION_DIR}/ changes\n\n"
                       f"Saved to {', '.join(map(str, published))}.\n")
            commit = os.fsdecode(_git_bytes(cwd, "commit-tree", tree, "-p", head, "-m", message,
                                            env=QUARANTINE_IDENTITY)).strip()
            _git_bytes(cwd, "update-ref", "-m", "fabric: quarantine", "HEAD", commit, head)
    _git_bytes(cwd, "update-index", "-z", "--index-info", input=_index_info(restore["index"], paths, zero))
    for path in paths:
        file = root / path
        if file.parent.resolve() == file.parent and os.path.lexists(file):
            if file.is_dir() and not file.is_symlink():
                raise ValueError(f"{path} is a directory")
            file.unlink()
    for path in paths:
        if path not in restore["disk"] or restore["disk"][path][0] == "160000":
            continue
        file = root / path
        if file.is_dir() and not file.is_symlink():
            file.rmdir()  # A directory the lane put where the start had a file; its files went above.
        file.parent.mkdir(parents=True, exist_ok=True)
        if file.parent.resolve() != file.parent:
            raise ValueError(f"{path} lies below a link")
        mode, oid = restore["disk"][path]
        data = _git_bytes(cwd, "cat-file", "blob", oid)
        if mode == "120000":
            os.symlink(os.fsdecode(data), file)
            continue
        descriptor = os.open(file, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                             0o755 if mode == "100755" else 0o644)
        with open(descriptor, "wb") as handle:
            handle.write(data)
    instructions = root / provider_exec.INSTRUCTION_DIR
    for path in paths:  # Tidy directories the lane added and quarantine emptied.
        parent = (root / path).parent
        while parent != instructions and parent.is_relative_to(instructions) and parent.is_dir():
            try:
                parent.rmdir()
            except OSError:
                break
            parent = parent.parent
    return published
