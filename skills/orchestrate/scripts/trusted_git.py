"""Owner-selected Git locations for writer protection, independent of lane discovery."""

from dataclasses import dataclass
import os
from pathlib import Path


# Used by both native and sandbox-exec writer boundaries.
GIT_PRIVATE_READ_ONLY = ("config.worktree", "commondir", "gitdir")


def writer_read_only(worktree, private, common):
    return [Path(worktree) / ".git", *(Path(private) / name for name in GIT_PRIVATE_READ_ONLY),
            Path(common) / "objects/info"]


@dataclass(frozen=True)
class GitLocation:
    worktree: Path
    private: Path
    common: Path

    def __fspath__(self):
        return str(self.worktree)

    def __str__(self):
        return str(self.worktree)

    def record(self):
        return {"worktree": str(self.worktree), "private": str(self.private), "common": str(self.common)}

    def validate(self):
        gitfile = self.worktree / ".git"
        if self.private == self.common:
            if gitfile.is_symlink() or gitfile.resolve() != self.common:
                raise ValueError("worktree Git directory differs from owner-recorded location")
        else:
            if gitfile.is_symlink() or gitfile.read_text().strip() != f"gitdir: {self.private}":
                raise ValueError("worktree gitfile differs from owner-recorded location")
            for name in GIT_PRIVATE_READ_ONLY:
                if (self.private / name).is_symlink():
                    raise ValueError(f"writer Git {name} is a link")
            common = (self.private / (self.private / "commondir").read_text().strip()).resolve()
            back = Path((self.private / "gitdir").read_text().strip()).resolve()
            if common != self.common or back != gitfile:
                raise ValueError("discovered common Git directory differs from owner-recorded location")
        # An alternate can inject objects from a writer-controlled store. Protection uses only
        # the owner's object store; providers cannot write its objects/info directory.
        if os.path.lexists(self.common / "objects/info/alternates"):
            raise ValueError("alternate object stores cannot vouch for instruction protection")

    def command(self):
        self.validate()
        return ["git", "--git-dir", str(self.private), "--work-tree", str(self.worktree)]

    def environment(self):
        return {"GIT_DIR": str(self.private), "GIT_COMMON_DIR": str(self.common),
                "GIT_OBJECT_DIRECTORY": str(self.common / "objects")}


def owner_location(cwd, *, common=None, owner=None):
    """Resolve from an owner primary checkout or its canonical .worktrees layout."""
    if isinstance(cwd, GitLocation):
        return cwd
    worktree = Path(cwd).resolve()
    if common is None:
        primary = Path(owner).resolve() if owner else worktree
        if worktree.parent.name == ".worktrees":
            primary = worktree.parent.parent
        if not (primary / ".git").is_dir() or (primary / ".git").is_symlink():
            raise ValueError("owner common Git directory required for a non-canonical worktree")
        common = primary / ".git"
    common = Path(common).resolve()
    if common == worktree / ".git":
        return GitLocation(worktree, common, common)
    # The registration lives in the owner's common directory; do not use the lane gitfile
    # or HEAD to choose either the private directory or the evidence key.
    for private in (common / "worktrees").glob("*"):
        back = private / "gitdir"
        if back.is_file() and not back.is_symlink() and Path(back.read_text().strip()) == worktree / ".git":
            return GitLocation(worktree, private, common)
    raise ValueError("writer is not registered in the owner common Git directory")
