#!/bin/sh
# Bootstrap a verified release. Exit: 2 usage, 3 toolchain consent, 4 download/checksum,
# 5 build/package; installation failures retain the CLI's exit code.
set -eu

die() { printf '%s\n' "$2" >&2; exit "$1"; }
ask() {
    printf '%s [%s] ' "$1" "$2" >&2
    IFS= read -r answer || die 2 'Input ended during setup'
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
        --from-source)
            source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
            shift
            if [ "$#" -gt 0 ] && [ "${1#--}" = "$1" ]; then source_dir=$1; shift; fi ;;
        --tag|--state-dir|--bin-dir)
            [ "$#" -ge 2 ] || die 2 "$1 requires a value"
            case "$1" in --tag) tag=$2 ;; --state-dir) state=$2 ;; --bin-dir) bin_dir=$2 ;; esac
            shift 2 ;;
        --upgrade) upgrade=true; shift ;;
        --help)
            printf '%s\n' 'Usage: install.sh [--yes] [--from-source [DIR]] [--tag TAG] [--state-dir DIR] [--bin-dir DIR]'
            exit 0 ;;
        *) die 2 "Unknown option: $1" ;;
    esac
done
case "$state" in /*) ;; *) die 2 '--state-dir must be absolute' ;; esac
case "$bin_dir" in ''|/*) ;; *) die 2 '--bin-dir must be absolute' ;; esac
case "$tag" in *[!a-zA-Z0-9._-]*) die 2 'Invalid release tag' ;; esac
receipt=$state/install/receipt.json
if "$upgrade" && [ ! -f "$receipt" ]; then die 2 'No install receipt; run install first'; fi

guided=false
if ! "$yes" && [ ! -f "$receipt" ]; then
    if { [ -t 0 ] && [ -t 1 ]; } || [ "${PROVENANT_INSTALL_TTY:-}" = 1 ]; then guided=true; fi
fi
umask 077
scratch=$(mktemp -d "${TMPDIR:-/tmp}/provenant-install.XXXXXX")
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
    if ! command -v rustup >/dev/null 2>&1 || ! rustup run "$toolchain" rustc --version >/dev/null 2>&1; then
        if ! "$yes"; then
            if [ -t 0 ] || [ "${PROVENANT_INSTALL_TTY:-}" = 1 ]; then
                ask "Install Rust $toolchain with rustup? y/n" n
                case "$answer" in y|Y|yes) ;; *) die 3 'Toolchain installation declined' ;; esac
            else die 3 'Pinned Rust toolchain missing; rerun with --yes to install it'; fi
        fi
        if ! command -v rustup >/dev/null 2>&1; then
            curl -fLsS --proto '=https' --tlsv1.2 https://sh.rustup.rs -o "$scratch/rustup.sh" || die 4 'Cannot download rustup'
            sh "$scratch/rustup.sh" -y --no-modify-path --default-toolchain none || die 5 'Cannot install rustup'
        fi
        rustup toolchain install "$toolchain" --profile minimal --component rustfmt --component clippy || die 5 'Cannot install toolchain'
    fi
    (
        cd "$workspace"
        CARGO_TARGET_DIR=$scratch/target
        CARGO_BUILD_BUILD_DIR=$scratch/target
        export CARGO_TARGET_DIR CARGO_BUILD_BUILD_DIR
        cargo build --release --locked --bin provenant &&
        cargo run --release --locked --example package-release -- --out "$release"
    ) || die 5 'Build or packaging failed'
else
    case "$(uname -s)/$(uname -m)" in
        Darwin/arm64) target=aarch64-apple-darwin ;;
        Darwin/x86_64) target=x86_64-apple-darwin ;;
        *) die 4 'No release package for this host; use --from-source in a checkout' ;;
    esac
    if [ -z "$tag" ] && command -v git >/dev/null 2>&1; then
        repo=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
        tag=$(git -C "$repo" describe --tags --exact-match HEAD 2>/dev/null || :)
    fi
    base=https://github.com/mblauberg/provenant/releases
    if [ -n "$tag" ]; then base=$base/download/$tag; else base=$base/latest/download; fi
    archive=provenant-$target.tar.gz
    curl -fLsS --proto '=https' --tlsv1.2 "$base/$archive" -o "$scratch/package.tar.gz" || die 4 'Cannot download release package'
    curl -fLsS --proto '=https' --tlsv1.2 "$base/$archive.sha256" -o "$scratch/checksum" || die 4 'Cannot download release checksum'
    expected=$(awk 'NR == 1 {print $1}' "$scratch/checksum")
    [ "${#expected}" -eq 64 ] || die 4 'Invalid checksum'
    case "$expected" in *[!0-9a-fA-F]*) die 4 'Invalid checksum' ;; esac
    actual=$(shasum -a 256 "$scratch/package.tar.gz")
    actual=${actual%% *}
    [ "$actual" = "$expected" ] || die 4 'Release checksum mismatch'
    mkdir "$release"
    tar -xzpf "$scratch/package.tar.gz" -C "$release" || die 4 'Cannot unpack release'
fi
[ -x "$release/bin/provenant" ] || die 4 'Release binary is missing'

# Existing receipts always use their recorded selection, paths and hooks.
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
"$release/bin/provenant" "$@"
if "$guided"; then
    case " $providers " in *' claude '*)
        for directory in "$HOME"/.claude-*; do
            [ -d "$directory" ] || continue
            if [ "$directory" != "${CLAUDE_CONFIG_DIR:-$HOME/.claude}" ]; then
                set -- --state-dir "$state" install --source "$release" --harness claude --hooks "$hooks"
                if [ -n "$bin_dir" ]; then set -- "$@" --bin-dir "$bin_dir"; fi
                CLAUDE_CONFIG_DIR=$directory "$release/bin/provenant" "$@"
            fi
        done ;;
    esac
fi
instructions_committed=true
if "$register"; then
    for directory in "$HOME"/.claude-*; do
        [ -d "$directory" ] || continue
        label=${directory##*/.claude-}
        "$release/bin/provenant" --state-dir "$state" accounts register "$label" --adapter claude --directory "$directory"
    done
fi
