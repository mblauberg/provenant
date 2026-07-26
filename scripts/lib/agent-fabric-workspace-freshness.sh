# Shared workspace build freshness predicate.
#
# The caller must define an absolute agents_home. A workspace is stale when
# any declared output is missing or older than TypeScript source, build
# configuration, schema generators, or the root manifests that define the
# project-reference graph.
#
# This is a sourced library, so its locals are the caller's globals on every
# shell that lacks `local`. The names are prefixed rather than left as
# `workspace`/`output`/`input`, which are too generic to clobber safely.
workspace_is_stale() {
  _afws_workspace=$1
  shift

  for _afws_output do
    [ -f "$_afws_output" ] || return 0

    # A `find` that cannot read the tree proves nothing about freshness, and
    # piping into `grep -q` would report grep's status rather than find's, so an
    # unreadable directory would silently read as "no newer sources". Take the
    # status directly and treat an unusable scan as stale.
    # The schema-generator rule stays scoped to the protocol package, which owns
    # the only generator whose output this predicate guards. Matching on the
    # directory name rather than an exact string against a rebuilt path keeps
    # that scope from silently lapsing under a trailing slash or a realpath-
    # resolved caller.
    _afws_patterns="src:*.ts"
    case "${_afws_workspace%/}" in
      */agent-fabric-protocol) _afws_patterns="$_afws_patterns scripts:*.mjs" ;;
    esac

    for _afws_generated in $_afws_patterns; do
      _afws_directory="$_afws_workspace/${_afws_generated%%:*}"
      [ -d "$_afws_directory" ] || continue
      _afws_newer=$(
        find "$_afws_directory" -type f -name "${_afws_generated#*:}" \
          -newer "$_afws_output" -print -quit 2>/dev/null
      ) || return 0
      [ -z "$_afws_newer" ] || return 0
    done

    for _afws_input in \
      "$_afws_workspace/package.json" \
      "$_afws_workspace/tsconfig.json" \
      "$_afws_workspace/tsconfig.build.json" \
      "$agents_home/package.json" \
      "$agents_home/package-lock.json" \
      "$agents_home/tsconfig.json"
    do
      [ ! -f "$_afws_input" ] || [ ! "$_afws_input" -nt "$_afws_output" ] || return 0
    done
  done

  return 1
}
