"""Owner-selected Git locations and the writer Git grants derived from them."""

from dataclasses import dataclass
import os
from pathlib import Path
import subprocess


# Writer Git metadata that would redirect discovery or object reads. Every writer boundary,
# native and sandbox-exec, keeps these read-only.
GIT_PRIVATE_READ_ONLY = ("config.worktree", "commondir", "gitdir")


def writer_read_only(worktree, private, common):
    return [Path(worktree) / ".git", *(Path(private) / name for name in GIT_PRIVATE_READ_ONLY),
            Path(common) / "objects/info"]


def writer_git_writes(private, common, ref):
    """(directories, files) a writer's Git may write: its private directory, objects and own branch.

    Integration baselines read the primary branch and its upstream, so no writer may move them,
    a tag or another checkout's branch. A detached writer gets no shared ref at all.
    """
    files = [common / ref, common / (ref + ".lock"), common / "logs" / ref] if ref else []
    return [private, common / "objects"], files


@dataclass(frozen=True)
class GitLocation:
    worktree: Path
    private: Path
    common: Path

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
        # An alternate can inject objects from a writer-controlled store; writers cannot write
        # objects/info, so one here was planted outside the writer boundary.
        if os.path.lexists(self.common / "objects/info/alternates"):
            raise ValueError("alternate object stores cannot vouch for instruction protection")

    def writer_ref(self):
        """The branch the writer's HEAD names, if the owner may grant it; None when detached."""
        head = (self.private / "HEAD").read_text().strip()
        if not head.startswith("ref: "):
            return None
        ref = head.removeprefix("ref: ")
        if not ref.startswith("refs/heads/") or subprocess.run(
                ["git", "check-ref-format", ref], capture_output=True, timeout=5).returncode:
            raise ValueError("writer HEAD must name a local branch or be detached")
        # A writer can point its own HEAD anywhere, so refuse a branch another checkout holds.
        others = [self.common / "HEAD", *(path / "HEAD" for path in (self.common / "worktrees").glob("*")
                                          if path != self.private)]
        if any(path.is_file() and path.read_text().strip() == head for path in others):
            raise ValueError(f"writer branch {ref} is checked out by another worktree")
        return ref


def owner_location(cwd, *, common=None):
    """Resolve a checkout's Git directories from an owner-named common directory or its own files.

    No writer can write a worktree's gitfile, commondir or gitdir (writer_read_only), and the
    common directory must register the worktree, so a lane cannot choose these locations.
    """
    worktree = Path(cwd).resolve()
    gitfile = worktree / ".git"
    if common is None and gitfile.is_dir() and not gitfile.is_symlink():
        common = gitfile
    elif common is None:
        if gitfile.is_symlink() or not gitfile.is_file():
            raise ValueError("not a Git checkout")
        private = worktree / gitfile.read_text().strip().removeprefix("gitdir: ")
        common = private / (private / "commondir").read_text().strip()
    common = Path(common).resolve()
    if common == worktree / ".git":
        return GitLocation(worktree, common, common)
    # The registration in the common directory, not the gitfile, names the private directory.
    for private in (common / "worktrees").glob("*"):
        back = private / "gitdir"
        if back.is_file() and not back.is_symlink() and Path(back.read_text().strip()) == worktree / ".git":
            return GitLocation(worktree, private, common)
    raise ValueError("writer is not registered in the owner common Git directory")
