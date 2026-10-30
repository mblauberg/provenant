#!/bin/sh
# The CLI supplies the resolved release_tag; no downloaded code runs before verification.
set -eu
: "${release_tag:?Missing release tag}"
case "$(uname -s)/$(uname -m)" in
    Darwin/arm64) ;;
    *) printf '%s\n' 'only macOS arm64 release packages are published; use --source' >&2; exit 4 ;;
esac
umask 077
scratch=$(mktemp -d "${TMPDIR:-/tmp}/provenant-upgrade.XXXXXX")
trap 'rm -rf "$scratch"' 0
trap 'exit 130' INT
trap 'exit 143' TERM HUP
base=https://github.com/mblauberg/provenant/releases/download/$release_tag
archive=provenant-aarch64-apple-darwin.tar.gz
verify_artefact() {
    artefact=$1
    curl -fLsS --proto '=https' --tlsv1.2 "$base/$artefact.sha256" -o "$scratch/checksum" </dev/null || {
        printf '%s\n' 'Cannot download release checksum' >&2; exit 4;
    }
    expected=$(awk 'NR == 1 {print $1}' "$scratch/checksum")
    [ "${#expected}" -eq 64 ] || { printf '%s\n' 'Invalid checksum' >&2; exit 4; }
    case "$expected" in *[!0-9a-fA-F]*) printf '%s\n' 'Invalid checksum' >&2; exit 4 ;; esac
    actual=$(shasum -a 256 "$scratch/$artefact" </dev/null)
    actual=${actual%% *}
    [ "$actual" = "$expected" ] || { printf '%s\n' 'Release checksum mismatch' >&2; exit 4; }
}
curl -fLsS --proto '=https' --tlsv1.2 "$base/$archive" -o "$scratch/$archive" </dev/null || {
    printf '%s\n' 'Cannot download release package' >&2; exit 4;
}
verify_artefact "$archive"
mkdir "$scratch/package"
tar -xzpf "$scratch/$archive" -C "$scratch/package" </dev/null || {
    printf '%s\n' 'Cannot unpack release package' >&2; exit 4;
}
sh "$scratch/package/scripts/install.sh" "$@" </dev/null
