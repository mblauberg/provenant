# Shared workspace build freshness predicate.
#
# The caller must define an absolute agents_home. A workspace is stale when
# any declared output is missing or older than TypeScript source, build
# configuration, schema generators, or the root manifests that define the
# project-reference graph.
workspace_is_stale() {
  workspace=$1
  shift

  for output do
    [ -f "$output" ] || return 0

    if [ -d "$workspace/src" ] \
      && find "$workspace/src" -type f -name '*.ts' -newer "$output" -print -quit | grep -q .; then
      return 0
    fi

    for input in \
      "$workspace/package.json" \
      "$workspace/tsconfig.json" \
      "$workspace/tsconfig.build.json"
    do
      [ ! -f "$input" ] || [ ! "$input" -nt "$output" ] || return 0
    done

    if [ "$workspace" = "$agents_home/runtime/agent-fabric-protocol" ] \
      && [ -d "$workspace/scripts" ] \
      && find "$workspace/scripts" -type f -name '*.mjs' -newer "$output" -print -quit | grep -q .; then
      return 0
    fi

    for input in \
      "$agents_home/package.json" \
      "$agents_home/package-lock.json" \
      "$agents_home/tsconfig.json"
    do
      [ ! -f "$input" ] || [ ! "$input" -nt "$output" ] || return 0
    done
  done

  return 1
}
