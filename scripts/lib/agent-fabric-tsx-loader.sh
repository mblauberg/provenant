# Shared resolution of the tsx ESM loader used by the source-tree fallback.
#
# npm hoists workspace dependencies to the repository root, so a linked git
# worktree usually has no node_modules of its own and the loader lives in an
# ancestor. Resolution must therefore look beyond the worktree — but the result
# is handed straight to `node --import`, so "look beyond" cannot mean "look
# anywhere". An unbounded walk towards / will execute a loader.mjs planted in
# $HOME or any intermediate directory, which contradicts the same workspace
# trust doctrine that refuses filesystem-root and home-wide authority.
#
# Resolution is therefore bounded by the tree that owns the workspace: the
# worktree itself, or, when the workspace is a linked git worktree, the
# repository whose node_modules npm hoisted the dependencies into. Nothing
# above that boundary may contribute an executable.
#
# The probe tests for the loader file itself and never for the node_modules
# directory: a worktree that has only ever run Vitest owns a node_modules
# holding nothing but a .vite cache, which satisfies a directory test while
# containing no packages at all.
#
# This is a sourced library, so its locals are the caller's globals on every
# shell that lacks `local`. The names are prefixed rather than left as
# `dir`/`candidate`, which are too generic to clobber safely.

# Physical path of a directory, with every symlink resolved. Invoked through a
# symlinked alias such as ~/.codex/skills/..., a lexical ancestor walk climbs
# the link path rather than the real tree and misses the node_modules that is
# actually there.
_aftsx_physical() {
  ( CDPATH= cd -- "$1" 2>/dev/null && pwd -P ) || printf '%s\n' "$1"
}

# The outermost directory allowed to provide a loader. For a linked worktree
# this is the repository that owns it; otherwise the workspace itself. A
# workspace outside any git repository is its own boundary.
resolve_tsx_loader_boundary() {
  _aftsx_home=$(_aftsx_physical "$1")

  _aftsx_common=$(
    git -C "$_aftsx_home" rev-parse --path-format=absolute --git-common-dir 2>/dev/null
  ) || _aftsx_common=
  if [ -z "$_aftsx_common" ]; then
    printf '%s\n' "$_aftsx_home"
    return 0
  fi

  _aftsx_owner=$(_aftsx_physical "$(dirname -- "$_aftsx_common")")
  # A bare repository, or any shape whose common dir is not an ancestor of the
  # workspace, must not widen the boundary beyond the workspace itself.
  case "$_aftsx_home" in
    "$_aftsx_owner" | "$_aftsx_owner"/*) printf '%s\n' "$_aftsx_owner" ;;
    *) printf '%s\n' "$_aftsx_home" ;;
  esac
}

# Whether a resolved path lies inside the boundary. Both arguments are already
# physical, so this is a prefix test on canonical paths rather than on whatever
# spelling the caller happened to use.
_aftsx_within() {
  case "$1" in
    "$2" | "$2"/*) return 0 ;;
    *) return 1 ;;
  esac
}

# A loader.mjs is accepted only when the package around it really is tsx. The
# bare file test admits any file of that name, including a planted one. This
# deliberately does not pin a version: no declared expectation exists to pin
# against, and inventing one here would fail every legitimate upgrade.
_aftsx_is_tsx_package() {
  _aftsx_manifest=${1%/dist/loader.mjs}/package.json
  [ -f "$_aftsx_manifest" ] || return 1
  grep -q '"name"[[:space:]]*:[[:space:]]*"tsx"' "$_aftsx_manifest"
}

# resolve_tsx_loader PACKAGE_ROOT [WORKSPACE]
#
# PACKAGE_ROOT is the package that declares the tsx dependency. Resolution
# starts there rather than at the workspace root, so the declaring package's
# own node_modules is found first instead of being skipped entirely.
#
# WORKSPACE is what the boundary is derived from, and it must be the workspace
# rather than the package: deriving it from PACKAGE_ROOT would bound the walk
# at the package itself, so the hoisted node_modules one level up — the whole
# reason this fallback walks at all — would be unreachable.
resolve_tsx_loader() {
  _aftsx_start=$(_aftsx_physical "$1")
  _aftsx_boundary=$(resolve_tsx_loader_boundary "${2:-$1}")

  # Node's own resolution algorithm, rooted at the declaring package. It is
  # symlink-correct and finds exactly what `import "tsx"` would find from that
  # package, which is the behaviour this fallback is trying to reproduce.
  _aftsx_resolved=$(
    node -e 'try{process.stdout.write(require.resolve("tsx/dist/loader.mjs",{paths:[process.argv[1]]}))}catch{}' \
      "$_aftsx_start" 2>/dev/null
  ) || _aftsx_resolved=
  if [ -n "$_aftsx_resolved" ] && [ -f "$_aftsx_resolved" ]; then
    _aftsx_resolved=$(_aftsx_physical "$(dirname -- "$_aftsx_resolved")")/loader.mjs
    if _aftsx_within "$_aftsx_resolved" "$_aftsx_boundary" &&
      _aftsx_is_tsx_package "$_aftsx_resolved"; then
      printf '%s\n' "$_aftsx_resolved"
      return 0
    fi
  fi

  # Fallback for what Node cannot serve: a hoisted node_modules that is an
  # ancestor of the workspace but not on the declaring package's resolution
  # path. The walk stops at the boundary and never reaches /.
  _aftsx_dir=$_aftsx_start
  while :; do
    _aftsx_candidate="$_aftsx_dir/node_modules/tsx/dist/loader.mjs"
    if [ -f "$_aftsx_candidate" ] && _aftsx_is_tsx_package "$_aftsx_candidate"; then
      printf '%s\n' "$_aftsx_candidate"
      return 0
    fi
    [ "$_aftsx_dir" = "$_aftsx_boundary" ] && return 1
    [ "$_aftsx_dir" = / ] && return 1
    _aftsx_parent=$(dirname -- "$_aftsx_dir")
    [ "$_aftsx_parent" = "$_aftsx_dir" ] && return 1
    _aftsx_dir=$_aftsx_parent
  done
}
