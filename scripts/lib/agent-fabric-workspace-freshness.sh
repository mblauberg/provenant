# Shared workspace build freshness predicate.
#
# The caller must define an absolute agents_home. A workspace is stale when
# any declared output is missing or older than TypeScript source, build
# configuration, or schema generators. Root manifests that define the
# project-reference graph are content-addressed when a successful build left a
# digest stamp beside the output; older builds without a usable stamp fall back
# to the conservative mtime rule.
#
# This is a sourced library, so its locals are the caller's globals on every
# shell that lacks `local`. The names are prefixed rather than left as
# `workspace`/`output`/`input`, which are too generic to clobber safely.
workspace_root_manifest_digest() {
  _afws_digest_root=$1
  _afws_digest_material=

  for _afws_digest_name in package.json package-lock.json tsconfig.json; do
    _afws_digest_input="$_afws_digest_root/$_afws_digest_name"
    [ -r "$_afws_digest_input" ] || return 1
    if command -v sha256sum >/dev/null 2>&1; then
      _afws_digest_output=$(sha256sum "$_afws_digest_input" 2>/dev/null) || return 1
    elif command -v shasum >/dev/null 2>&1; then
      _afws_digest_output=$(shasum -a 256 "$_afws_digest_input" 2>/dev/null) || return 1
    else
      return 1
    fi
    _afws_digest_value=${_afws_digest_output%% *}
    [ "${#_afws_digest_value}" -eq 64 ] || return 1
    case "$_afws_digest_value" in *[!0-9a-f]*) return 1 ;; esac
    _afws_digest_material="${_afws_digest_material}${_afws_digest_name}:${_afws_digest_value}
"
  done

  if command -v sha256sum >/dev/null 2>&1; then
    _afws_digest_output=$(printf '%s' "$_afws_digest_material" | sha256sum 2>/dev/null) \
      || return 1
  else
    _afws_digest_output=$(printf '%s' "$_afws_digest_material" | shasum -a 256 2>/dev/null) \
      || return 1
  fi
  _afws_digest_value=${_afws_digest_output%% *}
  [ "${#_afws_digest_value}" -eq 64 ] || return 1
  case "$_afws_digest_value" in *[!0-9a-f]*) return 1 ;; esac
  printf '%s\n' "$_afws_digest_value"
}

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
      "$_afws_workspace/tsconfig.build.json"
    do
      [ ! -f "$_afws_input" ] || [ ! "$_afws_input" -nt "$_afws_output" ] || return 0
    done

    _afws_root_manifests_addressed=false
    _afws_manifest_stamp="${_afws_output%/*}/.root-manifests.sha256"
    if [ -r "$_afws_manifest_stamp" ]; then
      _afws_recorded_digest=$(cat "$_afws_manifest_stamp" 2>/dev/null) || _afws_recorded_digest=
      if [ "${#_afws_recorded_digest}" -eq 64 ]; then
        case "$_afws_recorded_digest" in
          *[!0-9a-f]*) ;;
          *)
            _afws_current_digest=$(workspace_root_manifest_digest "$agents_home") \
              || _afws_current_digest=
            if [ -n "$_afws_current_digest" ]; then
              [ "$_afws_recorded_digest" = "$_afws_current_digest" ] || return 0
              _afws_root_manifests_addressed=true
            fi
            ;;
        esac
      fi
    fi

    if [ "$_afws_root_manifests_addressed" = false ]; then
      for _afws_input in \
        "$agents_home/package.json" \
        "$agents_home/package-lock.json" \
        "$agents_home/tsconfig.json"
      do
        [ ! -f "$_afws_input" ] || [ ! "$_afws_input" -nt "$_afws_output" ] || return 0
      done
    fi
  done

  return 1
}
