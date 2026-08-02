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

  # GIT_DIR, GIT_COMMON_DIR and GIT_WORK_TREE override what rev-parse reports,
  # so an inherited environment could otherwise name an ancestor repository and
  # widen the boundary to it. The boundary must come from the workspace on
  # disk, never from the caller's environment.
  _aftsx_common=$(
    unset GIT_DIR GIT_COMMON_DIR GIT_WORK_TREE
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

# Resolve and validate the one Node executable used by loader identity checks
# and TypeScript verification. An absolute override is required to work with a
# sanitised PATH; otherwise the ordinary PATH lookup remains the fallback.
resolve_agent_fabric_node() {
  _aftsx_node=${AGENT_FABRIC_NODE:-}
  if [ -n "$_aftsx_node" ]; then
    case "$_aftsx_node" in
      /*) ;;
      *) return 1 ;;
    esac
  else
    _aftsx_node=$(command -v node 2>/dev/null) || return 1
  fi
  [ -f "$_aftsx_node" ] && [ -x "$_aftsx_node" ] || return 1
  printf '%s\n' "$_aftsx_node"
}

# Validate one candidate and print the path that would actually be executed.
#
# The boundary governs *where a package may be declared*, not where its bytes
# live. A node_modules entry is frequently a symlink — pnpm links every package
# out to a global store, and npm links workspace members — so demanding that
# the resolved file also sit inside the boundary would reject ordinary layouts
# while proving nothing extra. Containment is therefore enforced by the caller,
# which only ever searches node_modules directories inside the boundary.
#
# What this checks is the candidate itself:
#
#   - it is canonicalised, including its own basename. A loader.mjs that is
#     itself a symlink executes its target, so the link path says nothing about
#     what runs, and the target is what must be identified and executed;
#   - the package owning the *resolved* file names itself tsx. Reading the
#     manifest as JSON rather than grepping it means a `"name": "tsx"` buried
#     in a description, a keyword list or a dependency entry cannot pass. This
#     is what stops a loader.mjs symlinked at some unrelated payload.
#
# It deliberately does not pin a version: no declared expectation exists to pin
# against, and inventing one here would fail every legitimate upgrade.
_aftsx_validate() {
  _aftsx_node=$(resolve_agent_fabric_node) || return 1
  "$_aftsx_node" -e '
const { realpathSync } = require("node:fs");
const { dirname } = require("node:path");
try {
  const real = realpathSync(process.argv[1]);
  const manifest = require(dirname(dirname(real)) + "/package.json");
  if (manifest.name !== "tsx") process.exit(1);
  process.stdout.write(real);
} catch { process.exit(1); }
' "$1" 2>/dev/null
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

  # The walk searches only node_modules directories that are themselves inside
  # the boundary, which is what makes the result safe. `require.resolve` is not
  # used: it reports a realpath and discards where the package was declared, so
  # its answer cannot be told apart from one found outside the boundary. Doing
  # the lookup here keeps the location and the target distinct, and the fixed
  # `tsx/dist/loader.mjs` path needs none of Node's exports-map machinery.
  #
  # Containment is checked per candidate rather than only at the loop's
  # stopping point. When PACKAGE_ROOT is not inside the boundary the walk never
  # meets it, so a termination-only check would climb to / and accept whatever
  # it passed on the way.
  _aftsx_dir=$_aftsx_start
  while :; do
    case "$_aftsx_dir" in
      "$_aftsx_boundary" | "$_aftsx_boundary"/*)
        _aftsx_candidate="$_aftsx_dir/node_modules/tsx/dist/loader.mjs"
        if [ -f "$_aftsx_candidate" ]; then
          _aftsx_accepted=$(_aftsx_validate "$_aftsx_candidate") || _aftsx_accepted=
          if [ -n "$_aftsx_accepted" ]; then
            printf '%s\n' "$_aftsx_accepted"
            return 0
          fi
        fi
        ;;
    esac
    [ "$_aftsx_dir" = "$_aftsx_boundary" ] && return 1
    [ "$_aftsx_dir" = / ] && return 1
    _aftsx_parent=$(dirname -- "$_aftsx_dir")
    [ "$_aftsx_parent" = "$_aftsx_dir" ] && return 1
    _aftsx_dir=$_aftsx_parent
  done
}

# resolve_attested_tsx_loader PRODUCT_ROOT
#
# Dependency attestation covers exactly PRODUCT_ROOT/node_modules. Launchers
# that rely on that receipt must not use the broader workspace walk above:
# doing so could select a package-local shadow tree whose bytes were never
# included in the receipt. The accepted loader's real path must also remain
# inside the attested tree, so a symlink cannot redirect execution to mutable
# bytes outside it.
resolve_attested_tsx_loader() {
  _aftsx_attested_root=$(_aftsx_physical "$1")
  _aftsx_attested_modules="$_aftsx_attested_root/node_modules"
  _aftsx_attested_modules_real=$(_aftsx_physical "$_aftsx_attested_modules")
  _aftsx_attested_candidate="$_aftsx_attested_modules/tsx/dist/loader.mjs"
  [ -f "$_aftsx_attested_candidate" ] || return 1
  _aftsx_attested_accepted=$(_aftsx_validate "$_aftsx_attested_candidate") || return 1
  case "$_aftsx_attested_accepted" in
    "$_aftsx_attested_modules_real/.bin" | "$_aftsx_attested_modules_real/.bin"/*)
      return 1
      ;;
    "$_aftsx_attested_modules_real"/*)
      printf '%s\n' "$_aftsx_attested_accepted"
      ;;
    *) return 1 ;;
  esac
}
