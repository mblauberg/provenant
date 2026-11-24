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
# One bounded, resumable fetch for the package and its checksum. A dropped
# stream resumes the partial file inside a fixed attempt budget; verification
# runs only after both complete, and a final failure names bytes and time.
download_artefact() {
    dl_name=$1
    dl_dest=$2
    dl_label=$3
    dl_attempts=0
    dl_max_attempts=3
    : > "$dl_dest"
    dl_start=$(date +%s)
    while [ "$dl_attempts" -lt "$dl_max_attempts" ]; do
        dl_attempts=$((dl_attempts + 1))
        if curl -fLsS --proto '=https' --tlsv1.2 -C - \
            --retry 5 --retry-all-errors --retry-delay 1 \
            "$base/$dl_name" -o "$dl_dest" </dev/null; then
            return 0
        fi
    done
    dl_bytes=$(wc -c < "$dl_dest" 2>/dev/null | tr -d '[:space:]') || dl_bytes=0
    [ -n "$dl_bytes" ] || dl_bytes=0
    dl_elapsed=$(( $(date +%s) - dl_start ))
    printf 'Cannot download release %s after %s attempts: received %s bytes in %s seconds\n' \
        "$dl_label" "$dl_attempts" "$dl_bytes" "$dl_elapsed" >&2
    exit 4
}
verify_artefact() {
    artefact=$1
    download_artefact "$artefact.sha256" "$scratch/checksum" checksum
    expected=$(awk 'NR == 1 {print $1}' "$scratch/checksum")
    [ "${#expected}" -eq 64 ] || { printf '%s\n' 'Invalid checksum' >&2; exit 4; }
    case "$expected" in *[!0-9a-fA-F]*) printf '%s\n' 'Invalid checksum' >&2; exit 4 ;; esac
    actual=$(shasum -a 256 "$scratch/$artefact" </dev/null)
    actual=${actual%% *}
    [ "$actual" = "$expected" ] || { printf '%s\n' 'Release checksum mismatch' >&2; exit 4; }
}
download_artefact "$archive" "$scratch/$archive" package
verify_artefact "$archive"
mkdir "$scratch/package"
tar -xzpf "$scratch/$archive" -C "$scratch/package" </dev/null || {
    printf '%s\n' 'Cannot unpack release package' >&2; exit 4;
}
sh "$scratch/package/scripts/install.sh" "$@" </dev/null
