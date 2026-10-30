#!/bin/sh
# Bootstrap a verified release. Exit: 2 usage, 3 toolchain consent, 4 download/checksum,
# 5 build/package; installation failures retain the CLI's exit code.
# Parse the entire download before executing any of it.
main() {
set -eu

die() { printf '%s\n' "$2" >&2; exit "$1"; }
ask() {
    printf '%s [%s] ' "$1" "$2" >&2
    if [ "${PROVENANT_INSTALL_TTY:-}" = 1 ]; then
        IFS= read -r answer || die 2 'Input ended during setup'
    else
        IFS= read -r answer </dev/tty || die 2 'Input ended during setup'
    fi
    answer=${answer:-$2}
}

yes=false
source_dir=
tag=
state=${PROVENANT_STATE_DIR:-"$HOME/.local/share/provenant/v1"}
bin_dir=
upgrade=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --yes) yes=true; shift ;;
        --source|--from-source)
            source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
            shift
            if [ "$#" -gt 0 ] && [ "${1#--}" = "$1" ]; then source_dir=$1; shift; fi ;;
        --tag|--state-dir|--bin-dir)
            [ "$#" -ge 2 ] || die 2 "$1 requires a value"
            case "$1" in --tag) tag=$2 ;; --state-dir) state=$2 ;; --bin-dir) bin_dir=$2 ;; esac
            shift 2 ;;
        --upgrade) upgrade=true; shift ;;
        --help)
            printf '%s\n' 'Usage: install.sh [--yes] [--upgrade] [--source|--from-source [DIR]] [--tag TAG] [--state-dir DIR] [--bin-dir DIR]'
            exit 0 ;;
        *) die 2 "Unknown option: $1" ;;
    esac
done
case "$state" in /*) ;; *) die 2 '--state-dir must be absolute' ;; esac
case "$bin_dir" in ''|/*) ;; *) die 2 '--bin-dir must be absolute' ;; esac
case "$tag" in *[!a-zA-Z0-9._-]*) die 2 'Invalid release tag' ;; esac
if [ -z "$source_dir" ]; then
    case "$(uname -s)/$(uname -m)" in
        Darwin/arm64) target=aarch64-apple-darwin ;;
        *) die 4 'only macOS arm64 release packages are published; use --source' ;;
    esac
fi
receipt=$state/install/receipt.json
if "$upgrade" && [ ! -f "$receipt" ]; then die 2 'No install receipt; run install first'; fi

guided=false
if ! "$yes" && [ ! -f "$receipt" ]; then
    if { [ -t 1 ] && ( : </dev/tty ) 2>/dev/null; } || [ "${PROVENANT_INSTALL_TTY:-}" = 1 ]; then guided=true; fi
fi
umask 077
scratch=$(mktemp -d "${TMPDIR:-/tmp}/provenant-install.XXXXXX")
app_stage=
instruction_count=0
instructions_committed=false
cleanup() {
    if ! "$instructions_committed"; then
        while [ "$instruction_count" -gt 0 ]; do
            record=$scratch/instructions/$instruction_count
            file=$(cat "$record/path")
            rm -f "$file"
            if [ -e "$record/original" ] || [ -L "$record/original" ]; then
                cp -Pp "$record/original" "$file"
            fi
            instruction_count=$((instruction_count - 1))
        done
    fi
    if [ -n "$app_stage" ]; then rm -rf "$app_stage"; fi
    rm -rf "$scratch"
}
trap cleanup 0
trap 'exit 130' INT
trap 'exit 143' TERM HUP
release=$scratch/release

if [ -n "$source_dir" ]; then
    source_dir=$(CDPATH='' cd -- "$source_dir" && pwd) || die 2 'Source directory does not exist'
    workspace=$source_dir/tools/front-door
    [ -f "$workspace/rust-toolchain.toml" ] || die 2 'Source directory is not a Provenant checkout'
    toolchain=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' "$workspace/rust-toolchain.toml")
    [ -n "$toolchain" ] || die 2 'Pinned toolchain is missing'
    PATH=$HOME/.cargo/bin:$PATH
    export PATH
    if ! command -v rustup >/dev/null 2>&1 || ! rustup run "$toolchain" rustc --version </dev/null >/dev/null 2>&1; then
        if ! "$yes"; then
            if ( : </dev/tty ) 2>/dev/null || [ "${PROVENANT_INSTALL_TTY:-}" = 1 ]; then
                ask "Install Rust $toolchain with rustup? y/n" n
                case "$answer" in y|Y|yes) ;; *) die 3 'Toolchain installation declined' ;; esac
            else die 3 'Pinned Rust toolchain missing; rerun with --yes to install it'; fi
        fi
        if ! command -v rustup >/dev/null 2>&1; then
            curl -fLsS --proto '=https' --tlsv1.2 https://sh.rustup.rs -o "$scratch/rustup.sh" </dev/null || die 4 'Cannot download rustup'
            sh "$scratch/rustup.sh" -y --no-modify-path --default-toolchain none </dev/null || die 5 'Cannot install rustup'
        fi
        rustup toolchain install "$toolchain" --profile minimal --component rustfmt --component clippy </dev/null || die 5 'Cannot install toolchain'
    fi
    (
        cd "$workspace"
        CARGO_TARGET_DIR=$scratch/target
        CARGO_BUILD_BUILD_DIR=$scratch/target
        RUSTC_WRAPPER=.cargo/rustc-wrapper.sh
        export CARGO_TARGET_DIR CARGO_BUILD_BUILD_DIR RUSTC_WRAPPER
        cargo build --release --locked --bin provenant </dev/null &&
        cargo run --release --locked --example package-release -- --out "$release" </dev/null
    ) || die 5 'Build or packaging failed'
else
    if [ -z "$tag" ] && [ -f "$0" ] && command -v git >/dev/null 2>&1; then
        repo=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
        # With /bin/sh -s, $0 can be the shell binary itself.
        if [ -f "$repo/tools/front-door/Cargo.toml" ]; then
            tag=$(git -C "$repo" describe --tags --exact-match HEAD </dev/null 2>/dev/null || :)
        fi
    fi
    base=https://github.com/mblauberg/provenant/releases
    if [ -n "$tag" ]; then base=$base/download/$tag; else base=$base/latest/download; fi
    archive=provenant-$target.tar.gz
    curl -fLsS --proto '=https' --tlsv1.2 "$base/$archive" -o "$scratch/package.tar.gz" </dev/null || die 4 'Cannot download release package'
    curl -fLsS --proto '=https' --tlsv1.2 "$base/$archive.sha256" -o "$scratch/checksum" </dev/null || die 4 'Cannot download release checksum'
    expected=$(awk 'NR == 1 {print $1}' "$scratch/checksum")
    [ "${#expected}" -eq 64 ] || die 4 'Invalid checksum'
    case "$expected" in *[!0-9a-fA-F]*) die 4 'Invalid checksum' ;; esac
    actual=$(shasum -a 256 "$scratch/package.tar.gz")
    actual=${actual%% *}
    [ "$actual" = "$expected" ] || die 4 'Release checksum mismatch'
    if [ "$target" = aarch64-apple-darwin ] && [ "${PROVENANT_APP_REFRESHED_BY_UPGRADE:-}" != 1 ]; then
        apps_archive=provenant-apps-aarch64-apple-darwin.tar.gz
        apps_status=$(curl -LsS --proto '=https' --tlsv1.2 -w '%{http_code}' "$base/$apps_archive" -o "$scratch/$apps_archive" </dev/null) || die 4 'Cannot download bundled app archive'
        case "$apps_status" in
            200)
                curl -fLsS --proto '=https' --tlsv1.2 "$base/$apps_archive.sha256" -o "$scratch/apps-checksum" </dev/null || die 4 'Cannot download bundled app checksum'
                apps_expected=$(awk 'NR == 1 {print $1}' "$scratch/apps-checksum")
                [ "${#apps_expected}" -eq 64 ] || die 4 'Invalid bundled app checksum'
                case "$apps_expected" in *[!0-9a-fA-F]*) die 4 'Invalid bundled app checksum' ;; esac
                apps_actual=$(shasum -a 256 "$scratch/$apps_archive")
                apps_actual=${apps_actual%% *}
                [ "$apps_actual" = "$apps_expected" ] || die 4 'Bundled app checksum mismatch'
                mkdir "$scratch/apps"
                tar -xzpf "$scratch/$apps_archive" -C "$scratch/apps" </dev/null || die 4 'Cannot unpack bundled app archive'
                app='Provenant Console.app'
                for binary in provenant-console quota-menubar; do
                    [ -x "$scratch/apps/$app/Contents/MacOS/$binary" ] || die 4 "Bundled app executable is missing: $binary"
                done
                mkdir -p "$HOME/Applications"
                app_stage=$(mktemp -d "$HOME/Applications/.provenant-upgrade.XXXXXX")
                cp -R "$scratch/apps/$app" "$app_stage/$app" ;;
            404) printf '%s\n' 'This release has no bundled app archive; upgrading Provenant without refreshing the app.' ;;
            *) die 4 "Cannot download bundled app archive (HTTP $apps_status)" ;;
        esac
    fi
    mkdir "$release"
    tar -xzpf "$scratch/package.tar.gz" -C "$release" </dev/null || die 4 'Cannot unpack release'
fi
[ -x "$release/bin/provenant" ] || die 4 'Release binary is missing'

# Existing receipts always use their recorded selection, paths and hooks, and
# every registered account directory is installed too.
set -- --state-dir "$state" install --source "$release"
if [ -n "$bin_dir" ]; then set -- "$@" --bin-dir "$bin_dir"; fi
providers=
register=false
hooks=off
backup_instruction() {
    record=$scratch/instructions/$((instruction_count + 1))
    mkdir -p "$record"
    printf '%s' "$file" > "$record/path"
    if [ -e "$file" ] || [ -L "$file" ]; then cp -Pp "$file" "$record/original"; fi
    instruction_count=$((instruction_count + 1))
}
instruction() {
    file=$1
    asset=$2
    [ -f "$asset" ] || return 0
    while :; do
        ask "$file: replace, append or skip?" skip
        case "$answer" in
            replace|r)
                if [ -d "$file" ] && [ ! -L "$file" ]; then die 2 "Instruction path is a directory: $file"; fi
                backup_instruction
                rm -f "$file" # The install below creates the managed link.
                return 0 ;;
            append|a)
                [ ! -L "$file" ] || die 2 "Cannot append to a symlink: $file"
                [ ! -d "$file" ] || die 2 "Instruction path is a directory: $file"
                backup_instruction
                mkdir -p "$(dirname -- "$file")"
                printf '\n' >> "$file"
                cat "$asset" >> "$file"
                return 0 ;;
            skip|s)
                if [ ! -e "$file" ] && [ ! -L "$file" ]; then
                    backup_instruction
                    mkdir -p "$(dirname -- "$file")"
                    : > "$file"
                fi
                return 0 ;;
        esac
    done
}
instructions() {
    instruction "$1/$2" "$release/instructions/AGENTS.md"
    instruction "$1/HARNESS.md" "$release/instructions/HARNESS.md"
}
if "$guided"; then
    for provider in claude codex cursor agy opencode; do
        cli=$provider
        [ "$provider" != cursor ] || cli=cursor-agent
        detected=n
        if command -v "$cli" >/dev/null 2>&1; then detected=y; fi
        if [ "$provider" = claude ]; then
            for directory in "$HOME"/.claude-*; do [ ! -d "$directory" ] || detected=y; done
        fi
        while :; do
            ask "Set up $provider? y/n" "$detected"
            case "$answer" in
                y|Y|yes) providers="$providers $provider"; set -- "$@" --harness "$provider"; break ;;
                n|N|no) break ;;
            esac
        done
    done
    [ -n "$providers" ] || die 2 'No providers selected'
    for provider in $providers; do
        case "$provider" in
            claude)
                instructions "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" CLAUDE.md
                for directory in "$HOME"/.claude-*; do
                    if [ -d "$directory" ] && [ "$directory" != "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" ]; then
                        instructions "$directory" CLAUDE.md
                    fi
                done ;;
            codex) instructions "${CODEX_HOME:-$HOME/.codex}" AGENTS.md ;;
            agy) instructions "$HOME/.gemini" GEMINI.md ;;
            opencode) instructions "$HOME/.config/opencode" AGENTS.md ;;
        esac
    done
    case " $providers " in *' claude '*)
        ask 'Enable Claude hooks? y/n' n
        case "$answer" in y|Y|yes) hooks=on; set -- "$@" --hooks on ;; esac
        found=false
        for directory in "$HOME"/.claude-*; do [ ! -d "$directory" ] || found=true; done
        if "$found"; then
            ask 'Register the Claude accounts in ~/.claude-*? y/n' y
            case "$answer" in y|Y|yes) register=true ;; esac
        fi ;;
    esac
fi
"$release/bin/provenant" "$@" </dev/null
instructions_committed=true
if [ -n "$app_stage" ]; then
    agent="$HOME/Library/LaunchAgents/dev.provenant.quota-menubar.plist"
    service="gui/$(id -u)/dev.provenant.quota-menubar"
    if [ -f "$agent" ]; then launchctl bootout "$service" </dev/null >/dev/null 2>&1 || :; fi
    app_backup=$app_stage/previous.app
    if [ -e "$HOME/Applications/$app" ]; then mv "$HOME/Applications/$app" "$app_backup"; fi
    if ! mv "$app_stage/$app" "$HOME/Applications/$app"; then
        if [ -e "$app_backup" ]; then
            mv "$app_backup" "$HOME/Applications/$app" || {
                app_stage=
                die 4 "Could not restore the previous Provenant app; backup remains at $app_backup"
            }
        fi
        die 4 'Could not install the refreshed Provenant app'
    fi
    rm -rf "$app_backup"
    rmdir "$app_stage"
    app_stage=
    if [ -f "$agent" ]; then
        plutil -replace ProgramArguments.0 -string "$HOME/Applications/$app/Contents/MacOS/quota-menubar" "$agent"
        attempts=0
        until launchctl bootstrap "gui/$(id -u)" "$agent" </dev/null; do
            attempts=$((attempts + 1))
            [ "$attempts" -lt 5 ] || die 4 'Could not restart the Provenant menu bar app'
            sleep 0.3
        done
        launchctl kickstart -k "$service" </dev/null || die 4 'Could not restart the Provenant menu bar app'
        launchctl print "$service" </dev/null >/dev/null || die 4 'Could not verify the Provenant menu bar app'
    fi
fi
if "$guided"; then
    case " $providers " in *' claude '*)
        for directory in "$HOME"/.claude-*; do
            [ -d "$directory" ] || continue
            if [ "$directory" != "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" ]; then
                set -- --state-dir "$state" install --source "$release" --harness claude --hooks "$hooks"
                if [ -n "$bin_dir" ]; then set -- "$@" --bin-dir "$bin_dir"; fi
                CLAUDE_CONFIG_DIR=$directory "$release/bin/provenant" "$@" </dev/null
            fi
        done ;;
    esac
fi
if "$register"; then
    for directory in "$HOME"/.claude-*; do
        [ -d "$directory" ] || continue
        label=${directory##*/.claude-}
        "$release/bin/provenant" --state-dir "$state" accounts register "$label" --adapter claude --directory "$directory" </dev/null
    done
fi
}
main "$@"
