#!/bin/sh
# Keep rustc file! paths and panic locations stable across checkout locations.
set -eu
compiler=$1
shift

workspace_root=
manifest_dir=${CARGO_MANIFEST_DIR-}
if [ -n "$manifest_dir" ]; then
    candidate=$manifest_dir
    while [ "$candidate" != / ]; do
        if [ -f "$candidate/Cargo.toml" ] && grep -q '^\[workspace\]' "$candidate/Cargo.toml"; then
            workspace_root=$(CDPATH= cd -- "$candidate" && pwd -P)
            break
        fi
        candidate=$(dirname "$candidate")
    done
fi
if [ -n "$workspace_root" ]; then
    set -- "$@" "--remap-path-prefix=$workspace_root=/workspace"
fi

cargo_home=${CARGO_HOME:-${HOME:?HOME or CARGO_HOME must be set}/.cargo}
for mapping in \
    "registry/src:/cargo/registry/src" \
    "registry/cache:/cargo/registry/cache" \
    "git/checkouts:/cargo/git/checkouts" \
    "git/db:/cargo/git/db"
do
    relative=${mapping%%:*}
    destination=${mapping#*:}
    path=$cargo_home/$relative
    if [ -d "$path" ]; then
        prefix=$(CDPATH= cd -- "$path" && pwd -P)
        set -- "$@" "--remap-path-prefix=$prefix=$destination"
    fi
done

# rustc's codegen metadata can include output object paths even when debug
# info is disabled. Cargo supplies each crate's output directory explicitly;
# remap that per invocation to the same stable prefix as well.
out_dir=
next_is_out_dir=false
for arg do
    if [ "$next_is_out_dir" = true ]; then
        out_dir=$arg
        next_is_out_dir=false
    elif [ "$arg" = "--out-dir" ]; then
        next_is_out_dir=true
    elif [ "${arg#--out-dir=}" != "$arg" ]; then
        out_dir=${arg#--out-dir=}
    fi
done
if [ -n "$out_dir" ]; then
    case "$out_dir" in
        /*) path=$out_dir ;;
        *) path=$PWD/$out_dir ;;
    esac
    if [ -d "$path" ]; then
        prefix=$(CDPATH= cd -- "$path" && pwd -P)
        set -- "$@" "--remap-path-prefix=$prefix=/rustc-output"
    fi
fi

exec "$compiler" "$@"
