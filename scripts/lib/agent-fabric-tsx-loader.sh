# Shared resolution of the tsx ESM loader used by the source-tree fallback.
#
# npm hoists workspace dependencies to the repository root, so a linked git
# worktree has no node_modules of its own and the loader lives in an ancestor.
# Resolution therefore walks from the starting directory towards the filesystem
# root the way Node resolves a bare specifier, rather than assuming
# node_modules sits directly beneath agents_home.
#
# The probe tests for the loader file itself and never for the node_modules
# directory: a worktree that has only ever run Vitest owns a node_modules
# holding nothing but a .vite cache, which satisfies a directory test while
# containing no packages at all.
#
# This is a sourced library, so its locals are the caller's globals on every
# shell that lacks `local`. The names are prefixed rather than left as
# `dir`/`candidate`, which are too generic to clobber safely.
resolve_tsx_loader() {
  _aftsx_dir=$1

  while :; do
    _aftsx_candidate="$_aftsx_dir/node_modules/tsx/dist/loader.mjs"
    if [ -f "$_aftsx_candidate" ]; then
      printf '%s\n' "$_aftsx_candidate"
      return 0
    fi
    [ "$_aftsx_dir" = / ] && return 1
    _aftsx_dir=$(dirname -- "$_aftsx_dir")
  done
}
