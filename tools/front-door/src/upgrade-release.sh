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
download_package() {
    artefact=$1
    dest=$scratch/$artefact
    # A dropped HTTP/2 stream must not abort the whole peer upgrade. Resume the
    # partial file and retry within a bounded budget. Verification runs only
    # after this function returns, so a partial file is never accepted.
    attempts=0
    max_attempts=3
    : > "$dest"
    start=$(date +%s)
    while [ "$attempts" -lt "$max_attempts" ]; do
        attempts=$((attempts + 1))
        if curl -fLsS --proto '=https' --tlsv1.2 -C - \
            --retry 5 --retry-all-errors --retry-delay 1 \
            "$base/$artefact" -o "$dest" </dev/null; then
            return 0
        fi
    done
    bytes=$(wc -c < "$dest" 2>/dev/null | tr -d '[:space:]') || bytes=0
    [ -n "$bytes" ] || bytes=0
    elapsed=$(( $(date +%s) - start ))
    printf 'Cannot download release package after %s attempts: received %s bytes in %s seconds\n' \
        "$attempts" "$bytes" "$elapsed" >&2
    exit 4
}
download_package "$archive"
verify_artefact "$archive"
mkdir "$scratch/package"
tar -xzpf "$scratch/$archive" -C "$scratch/package" </dev/null || {
    printf '%s\n' 'Cannot unpack release package' >&2; exit 4;
}
sh "$scratch/package/scripts/install.sh" "$@" </dev/null
