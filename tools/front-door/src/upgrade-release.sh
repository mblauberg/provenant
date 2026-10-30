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
app_stage=
trap 'rm -rf "$scratch"; if [ -n "$app_stage" ]; then rm -rf "$app_stage"; fi' 0
trap 'exit 130' INT
trap 'exit 143' TERM HUP
base=https://github.com/mblauberg/provenant/releases/download/$release_tag
archive=provenant-aarch64-apple-darwin.tar.gz
apps_archive=provenant-apps-aarch64-apple-darwin.tar.gz
verify_artefact() {
    artefact=$1
    curl -fLsS --proto '=https' --tlsv1.2 "$base/$artefact.sha256" -o "$scratch/checksum" </dev/null
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
apps_available=false
if status=$(curl -LsS --proto '=https' --tlsv1.2 -w '%{http_code}' "$base/$apps_archive" -o "$scratch/$apps_archive" </dev/null); then
    case "$status" in
        200) apps_available=true ;;
        404) printf '%s\n' 'This release has no bundled app archive; upgrading Provenant without refreshing the app.' ;;
        *) printf '%s\n' "Cannot download bundled app archive (HTTP $status)" >&2; exit 4 ;;
    esac
else
    printf '%s\n' 'Cannot download bundled app archive' >&2
    exit 4
fi
if "$apps_available"; then verify_artefact "$apps_archive"; fi
mkdir "$scratch/package"
tar -xzpf "$scratch/$archive" -C "$scratch/package" </dev/null
if "$apps_available"; then
    mkdir "$scratch/apps"
    tar -xzpf "$scratch/$apps_archive" -C "$scratch/apps" </dev/null
    app='Provenant Console.app'
    for binary in provenant-console quota-menubar; do
        [ -x "$scratch/apps/$app/Contents/MacOS/$binary" ] || {
            printf '%s\n' "Release app executable is missing: $binary" >&2; exit 4;
        }
    done
    # Stage on the destination filesystem before replacing the installed bundle.
    mkdir -p "$HOME/Applications"
    app_stage=$(mktemp -d "$HOME/Applications/.provenant-upgrade.XXXXXX")
    cp -R "$scratch/apps/$app" "$app_stage/$app"
fi
PROVENANT_APP_REFRESHED_BY_UPGRADE=1 sh "$scratch/package/scripts/install.sh" "$@" </dev/null
if "$apps_available"; then
    agent="$HOME/Library/LaunchAgents/dev.provenant.quota-menubar.plist"
    service="gui/$(id -u)/dev.provenant.quota-menubar"
    if [ -f "$agent" ]; then
        launchctl bootout "$service" </dev/null >/dev/null 2>&1 || :
    fi
    app_backup=$app_stage/previous.app
    if [ -e "$HOME/Applications/$app" ]; then mv "$HOME/Applications/$app" "$app_backup"; fi
    if ! mv "$app_stage/$app" "$HOME/Applications/$app"; then
        if [ -e "$app_backup" ]; then
            mv "$app_backup" "$HOME/Applications/$app" || {
                app_stage=
                printf '%s\n' "Could not restore the previous Provenant app; backup remains at $app_backup" >&2; exit 4;
            }
        fi
        printf '%s\n' 'Could not install the refreshed Provenant app' >&2; exit 4
    fi
    rm -rf "$app_backup"
    rmdir "$app_stage"
    app_stage=
    if [ -f "$agent" ]; then
        plutil -replace ProgramArguments.0 -string "$HOME/Applications/$app/Contents/MacOS/quota-menubar" "$agent"
        attempts=0
        until launchctl bootstrap "gui/$(id -u)" "$agent" </dev/null; do
            attempts=$((attempts + 1))
            [ "$attempts" -lt 5 ] || { printf '%s\n' 'Could not restart the Provenant menu bar app' >&2; exit 4; }
            sleep 0.3
        done
        launchctl kickstart -k "$service" </dev/null || { printf '%s\n' 'Could not restart the Provenant menu bar app' >&2; exit 4; }
        launchctl print "$service" </dev/null >/dev/null || { printf '%s\n' 'Could not verify the Provenant menu bar app' >&2; exit 4; }
    fi
fi
