#!/bin/sh
# Bootstrap a verified release. Exit: 2 usage, 3 toolchain consent, 4 download/checksum,
# 5 build/package; other installation failures retain the CLI's exit code.
# Parse the entire download before executing any of it.
main() {
set -eu

die() { printf '%s\n' "$2" >&2; exit "$1"; }
# App refresh is best effort: a missing or damaged asset must not stop the CLI
# upgrade. Print to stdout so `provenant upgrade` carries the warning in its
# captured host report, then leave the installed app untouched.
app_unavailable() {
    printf '%s\n' "Warning: $1; the app is left as-is and may be behind the CLI." >&2
    [ -z "$app_stage" ] || rm -rf "$app_stage" || :
    app_stage=
}
app_version() {
    /usr/bin/plutil -extract CFBundleShortVersionString raw -o - "$1/Contents/Info.plist" 2>/dev/null || printf 'unknown'
}
# Earlier bundles nested a login-item app, which macOS registered separately from
# the LaunchAgent. Remove that bundle and any surviving registration: the agent
# is the only launch path now. Every step is best effort and warns on failure.
clean_menu_login_item() {
    stale="$apps_dir/$app/Contents/Library/LoginItems/Provenant Menu.app"
    removed=false
    if [ -e "$stale" ] || [ -L "$stale" ]; then
        rm -rf "$stale" || printf '%s\n' "Warning: could not remove the old menu login item at $stale" >&2
        removed=true
    fi
    labels=$(launchctl print "gui/$(id -u)" 2>/dev/null | awk '/dev\.provenant\.menu\./ {
        for (i = 1; i <= NF; i++) {
            label = $i; gsub(/["=]/, "", label)
            if (label ~ /^application\.dev\.provenant\.menu\./) { print label; break }
        }
    }') || labels=
    for label in $labels; do
        removed=true
        launchctl bootout "gui/$(id -u)/$label" </dev/null >/dev/null 2>&1 ||
            printf '%s\n' "Warning: could not unregister the old menu login item $label; remove it from Login Items" >&2
    done
    if "$removed"; then
        printf '%s\n' "Warning: removed the old Provenant menu login item; the LaunchAgent is now the only launch path" >&2
    fi
}
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
preflight_only=false
non_interactive=false
source_dir=
tag=
# 2.0 runs from a fresh root. The first install copies the 1.x owner
# configuration out of the 1.x root and never writes that root.
default_state=$HOME/.local/share/provenant/v2
legacy_state=$HOME/.local/share/provenant/v1
state=${PROVENANT_STATE_DIR:-"$default_state"}
# Where the user keeps the bundled macOS app. The Rust side reads the same
# variable, so install, upgrade and doctor all name the same bundle.
apps_dir=${PROVENANT_APPS_DIR:-"$HOME/Applications"}
# `provenant upgrade` prints the app line from its own report; keep this script quiet there.
quiet_app=${PROVENANT_UPGRADE_QUIET_APP:-false}
bin_dir=
upgrade=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --yes) yes=true; shift ;;
        --source)
            source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
            shift
            if [ "$#" -gt 0 ] && [ "${1#--}" = "$1" ]; then source_dir=$1; shift; fi ;;
        --tag|--state-dir|--bin-dir)
            [ "$#" -ge 2 ] || die 2 "$1 requires a value"
            case "$1" in --tag) tag=$2 ;; --state-dir) state=$2 ;; --bin-dir) bin_dir=$2 ;; esac
            shift 2 ;;
        --upgrade) upgrade=true; shift ;;
        --preflight-only) preflight_only=true; shift ;;
        --non-interactive) non_interactive=true; shift ;;
        --help)
            printf '%s\n' 'Usage: install.sh [--yes] [--upgrade] [--source [DIR]] [--tag TAG] [--state-dir DIR] [--bin-dir DIR]'
            exit 0 ;;
        *) die 2 "Unknown option: $1" ;;
    esac
done
case "$state" in /*) ;; *) die 2 '--state-dir must be absolute' ;; esac
case "$apps_dir" in /*) ;; *) die 2 'PROVENANT_APPS_DIR must be absolute' ;; esac
case "$bin_dir" in ''|/*) ;; *) die 2 '--bin-dir must be absolute' ;; esac
case "$tag" in *[!a-zA-Z0-9._-]*) die 2 'Invalid release tag' ;; esac
if [ -z "$source_dir" ]; then
    case "$(uname -s)/$(uname -m)" in
        Darwin/arm64) target=aarch64-apple-darwin ;;
        *) die 4 'only macOS arm64 release packages are published; use --source' ;;
    esac
fi
# A 1.x upgrade names the 1.x root; 2.0 installs at its own default instead.
if [ "$state" = "$legacy_state" ]; then
    printf '%s\n' "Provenant 2 keeps its state in $default_state; $legacy_state stays in place and its configuration is copied." >&2
    state=$default_state
fi
receipt=$state/install/receipt.json
# The 1.x receipt counts until the first 2.0 install copies it.
installed() {
    [ -f "$receipt" ] || { [ "$state" = "$default_state" ] && [ -f "$legacy_state/install/receipt.json" ]; }
}
if "$upgrade" && ! installed; then die 2 'No install receipt; run install first'; fi

if "$preflight_only" && ! "$upgrade"; then die 2 '--preflight-only requires --upgrade'; fi

guided=false
if ! "$non_interactive" && ! "$yes" && ! installed; then
    if { [ -t 1 ] && ( : </dev/tty ) 2>/dev/null; } || [ "${PROVENANT_INSTALL_TTY:-}" = 1 ]; then guided=true; fi
fi
umask 077
scratch=$(mktemp -d "${TMPDIR:-/tmp}/provenant-install.XXXXXX")
app_stage=
app='Provenant Console.app'
app_backup=
old_app_inode=
agent_temp=
app=
agent=
service=
app_agent_stopped=false
app_upgrade_committed=false
swap_helper=
helper_executable='Contents/MacOS/quota-menubar'
agent_backup=
instruction_count=0
instructions_committed=false
# Take the live LaunchAgent plist off its path, but only while a backup copy of it exists.
# Never fails: sets agent_note to what happened so the recovery message can say so.
agent_note=
quarantine_agent() {
    agent_note=
    if [ ! -e "$agent" ] && [ ! -L "$agent" ]; then return 0; fi
    if [ -z "$agent_backup" ] || [ ! -f "$agent_backup" ]; then
        agent_note=" The live LaunchAgent plist at $agent was left in place because no backup of it exists."
    elif [ -n "$app_stage" ] && mv -f "$agent" "$app_stage/$1" 2>/dev/null; then
        agent_note=" The live LaunchAgent plist was moved to $app_stage/$1."
    elif rm -f "$agent" 2>/dev/null && [ ! -e "$agent" ] && [ ! -L "$agent" ]; then
        agent_note=' The live LaunchAgent plist was removed.'
    else
        agent_note=" Could not move or remove the live LaunchAgent plist at $agent; remove it with: rm -f '$agent'."
    fi
    return 0
}
# Print the manual recovery instructions, naming every retained recovery copy.
recovery_failed() {
    retained=
    steps=
    if [ -n "$app_backup" ]; then
        retained="previous app $app_backup"
        steps="restore $app_backup to $apps_dir/$app"
    fi
    if [ -n "$agent_backup" ]; then
        retained="${retained:+$retained; }previous LaunchAgent plist $agent_backup"
        steps="${steps:+$steps and }restore $agent_backup to $agent"
    fi
    printf '%s\n' "$1; agent remains stopped. Retained recovery copies: ${retained:-none}.$agent_note${steps:+ Before bootstrapping, $steps.}" >&2
}
cleanup() {
    status=$?
    # Once recovery starts, finish it even if another termination signal arrives.
    trap '' INT TERM HUP
    # Every step below is best effort: set -e must not cut recovery or its report short.
    if "$app_upgrade_committed" && [ -n "$app_backup" ] && [ -e "$app_backup" ]; then
        printf '%s\n' "Warning: previous Provenant app backup remains at $app_backup" >&2
        app_stage=
    fi
    if ! "$instructions_committed"; then
        while [ "$instruction_count" -gt 0 ]; do
            record=$scratch/instructions/$instruction_count
            file=$(cat "$record/path") || file=
            if [ -n "$file" ]; then
                rm -f "$file" || :
                if [ -e "$record/original" ] || [ -L "$record/original" ]; then
                    cp -Pp "$record/original" "$file" || printf '%s\n' "Could not restore $file from $record/original" >&2
                fi
            fi
            instruction_count=$((instruction_count - 1))
        done
    fi
    if [ -n "$agent_temp" ]; then rm -f "$agent_temp" || :; fi
    if ! "$app_upgrade_committed"; then
        recovered=true
        if "$app_agent_stopped"; then launchctl bootout "$service" </dev/null >/dev/null 2>&1 || :; fi
        if [ -n "$agent_backup" ] && [ -f "$agent_backup" ]; then
            agent_temp=$(mktemp "${agent}.restore.XXXXXX") || { recovered=false; agent_temp=; }
            if "$recovered" && ! { cp -p "$agent_backup" "$agent_temp" && mv -f "$agent_temp" "$agent"; }; then recovered=false; fi
            if ! "$recovered"; then
                [ -z "$agent_temp" ] || rm -f "$agent_temp" || :
                quarantine_agent agent.unrecovered.plist
                recovery_failed 'Could not restore the menu bar LaunchAgent'
                app_stage=
            fi
        fi
        if "$recovered" && [ -n "$app_backup" ] && [ -e "$app_backup" ]; then
            # The marker precedes the helper; exchange only if it moved the old app.
            if backup_inode=$(ls -di "$app_backup"); then
                backup_inode=${backup_inode%% *}
            else
                recovered=false
            fi
            if "$recovered" && [ "$backup_inode" = "$old_app_inode" ]; then
                if [ -e "$apps_dir/$app" ]; then
                    "$swap_helper" --swap-path "$app_backup" "$apps_dir/$app" || recovered=false
                else
                    mv "$app_backup" "$apps_dir/$app" || recovered=false
                fi
            fi
            if ! "$recovered"; then
                # Never leave a plist for a helper the new app may lack on its live path.
                quarantine_agent agent.recovery.plist
                recovery_failed 'Could not restore the previous Provenant app'
                app_stage=
            fi
        fi
        if "$recovered" && "$app_agent_stopped" && [ -f "$agent" ] && [ -d "$apps_dir/$app" ]; then
            launchctl bootout "$service" </dev/null >/dev/null 2>&1 || :
            launchctl bootstrap "gui/$(id -u)" "$agent" </dev/null >/dev/null 2>&1 || :
            launchctl kickstart -k "$service" </dev/null >/dev/null 2>&1 || :
        fi
    fi
    if [ -n "$app_stage" ]; then rm -rf "$app_stage" || :; fi
    rm -rf "$scratch" || :
    exit "$status"
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
    # A source build has no signed app asset; only the release workflow builds
    # and publishes it. Say so instead of leaving a stale app unexplained.
    app_unavailable 'This checkout has no bundled app archive'
    PATH=$HOME/.cargo/bin:$PATH
    export PATH
    if ! command -v rustup >/dev/null 2>&1 || ! rustup run "$toolchain" rustc --version </dev/null >/dev/null 2>&1; then
        if ! "$yes"; then
            if ! "$non_interactive" && { ( : </dev/tty ) 2>/dev/null || [ "${PROVENANT_INSTALL_TTY:-}" = 1 ]; }; then
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
        cargo run --release --locked --bin package-release -- --out "$release" </dev/null
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
    if [ "$target" = aarch64-apple-darwin ]; then
        stage_app() {
            apps_archive=provenant-apps-aarch64-apple-darwin.tar.gz
            apps_status=$(curl -LsS --proto '=https' --tlsv1.2 -w '%{http_code}' "$base/$apps_archive" -o "$scratch/$apps_archive" </dev/null) || {
                app_unavailable 'Cannot download bundled app archive'; return 0; }
            case "$apps_status" in
                200) ;;
                404) app_unavailable 'This release has no bundled app archive'; return 0 ;;
                *) app_unavailable "Cannot download bundled app archive (HTTP $apps_status)"; return 0 ;;
            esac
            curl -fLsS --proto '=https' --tlsv1.2 "$base/$apps_archive.sha256" -o "$scratch/apps-checksum" </dev/null || {
                app_unavailable 'Cannot download bundled app checksum'; return 0; }
            apps_expected=$(awk 'NR == 1 {print $1}' "$scratch/apps-checksum")
            [ "${#apps_expected}" -eq 64 ] || { app_unavailable 'Invalid bundled app checksum'; return 0; }
            case "$apps_expected" in
                *[!0-9a-fA-F]*) app_unavailable 'Invalid bundled app checksum'; return 0 ;;
            esac
            apps_actual=$(shasum -a 256 "$scratch/$apps_archive")
            apps_actual=${apps_actual%% *}
            [ "$apps_actual" = "$apps_expected" ] || { app_unavailable 'Bundled app checksum mismatch'; return 0; }
            mkdir "$scratch/apps" || { app_unavailable 'Cannot unpack bundled app archive'; return 0; }
            tar -xzpf "$scratch/$apps_archive" -C "$scratch/apps" </dev/null || {
                app_unavailable 'Cannot unpack bundled app archive'; return 0; }
            app='Provenant Console.app'
            [ -x "$scratch/apps/$app/Contents/MacOS/provenant-console" ] || {
                app_unavailable 'Bundled app executable is missing: provenant-console'; return 0; }
            [ -x "$scratch/apps/$app/$helper_executable" ] || {
                app_unavailable 'Bundled app executable is missing: quota-menubar'; return 0; }
            mkdir -p "$apps_dir" || { app_unavailable 'Cannot create the app directory'; return 0; }
            app_stage=$(mktemp -d "$apps_dir/.provenant-retained.XXXXXX") || {
                app_unavailable 'Cannot stage the app'; return 0; }
            cp -R "$scratch/apps/$app" "$app_stage/$app" || {
                app_unavailable 'Cannot stage the app'; app_stage=; return 0; }
        }
        stage_app
    fi
    mkdir "$release"
    tar -xzpf "$scratch/package.tar.gz" -C "$release" </dev/null || die 4 'Cannot unpack release'
fi
[ -x "$release/bin/provenant" ] || die 4 'Release binary is missing'

if "$upgrade"; then
    # The staged binary previews every target and checks the release first.
    if "$release/bin/provenant" --state-dir "$state" install --source "$release" --upgrade-preflight </dev/null; then
        :
    else
        preflight_status=$?
        die "$preflight_status" 'Upgrade preflight failed; nothing was changed'
    fi
fi
if "$preflight_only"; then exit 0; fi

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
# This script owns the app swap and its report line, so the inner install skips its own.
PROVENANT_INSTALLER_OWNS_APP=true "$release/bin/provenant" "$@" </dev/null
instructions_committed=true
if [ -n "$app_stage" ]; then
    agent="$HOME/Library/LaunchAgents/dev.provenant.quota-menubar.plist"
    service="gui/$(id -u)/dev.provenant.quota-menubar"
    # Keep the new signed executable outside both paths for swap and rollback.
    swap_helper=$scratch/swap-path
    cp -p "$app_stage/$app/Contents/MacOS/provenant-console" "$swap_helper" || die 4 'Could not prepare atomic app swap helper'
    if [ -f "$agent" ]; then
        app_agent_stopped=true
        launchctl bootout "$service" </dev/null >/dev/null 2>&1 || :
    fi
    if [ -e "$apps_dir/$app" ]; then
        old_app_inode=$(ls -di "$apps_dir/$app")
        old_app_inode=${old_app_inode%% *}
        app_backup=$app_stage/$app
        if ! "$swap_helper" --swap-path "$app_backup" "$apps_dir/$app"; then
            die 4 'Could not atomically swap the Provenant app'
        fi
    elif ! mv "$app_stage/$app" "$apps_dir/$app"; then
        die 4 'Could not install the refreshed Provenant app'
    fi
    if [ -f "$agent" ]; then
        # Migrate the agent in place: its executable becomes the nested menu helper app (#2473, #2469).
        cp -p "$agent" "$app_stage/agent.plist.tmp" || die 4 'Could not back up the Provenant menu bar LaunchAgent'
        mv "$app_stage/agent.plist.tmp" "$app_stage/agent.plist" || die 4 'Could not publish the Provenant menu bar LaunchAgent backup'
        agent_backup=$app_stage/agent.plist
        helper_path="$apps_dir/$app/$helper_executable"
        agent_temp=$(mktemp "${agent}.update.XXXXXX") || die 4 'Could not prepare the menu bar LaunchAgent update'
        cp -p "$agent" "$agent_temp" || die 4 'Could not prepare the menu bar LaunchAgent update'
        plutil -replace ProgramArguments -array "$agent_temp" || die 4 'Could not reset the Provenant menu bar LaunchAgent arguments'
        plutil -insert ProgramArguments.0 -string "$helper_path" "$agent_temp" || die 4 'Could not update the Provenant menu bar LaunchAgent'
        [ "$(plutil -extract ProgramArguments.0 raw -o - "$agent_temp")" = "$helper_path" ] || die 4 'Could not verify the Provenant menu bar LaunchAgent'
        if plutil -extract ProgramArguments.1 raw -o - "$agent_temp" >/dev/null 2>&1; then
            die 4 'Could not verify the Provenant menu bar LaunchAgent arguments'
        fi
        mv -f "$agent_temp" "$agent" || die 4 'Could not publish the menu bar LaunchAgent'
        agent_temp=
        [ -x "$helper_path" ] || die 4 'The installed Provenant app has no menu bar helper'
        attempts=0
        until launchctl bootstrap "gui/$(id -u)" "$agent" </dev/null; do
            attempts=$((attempts + 1))
            [ "$attempts" -lt 5 ] || die 4 'Could not restart the Provenant menu bar app'
            sleep 0.3
        done
        launchctl kickstart -k "$service" </dev/null || die 4 'Could not restart the Provenant menu bar app'
        launchctl print "$service" </dev/null >/dev/null || die 4 'Could not verify the Provenant menu bar app'
    fi
    # The new app and migrated agent are now committed; cleanup must not roll them back.
    app_upgrade_committed=true
    clean_menu_login_item
    [ "$quiet_app" = true ] || printf '%s\n' "app: $apps_dir/$app ($(app_version "$apps_dir/$app"))"
    if [ -n "$app_backup" ] && ! rm -rf "$app_backup"; then
        printf '%s\n' "Warning: could not remove the previous Provenant app backup; staging remains at $app_stage" >&2
        app_backup=
        agent_backup=
        app_stage=
    else
        app_backup=
        rm -f "$agent_backup"
        agent_backup=
        rmdir "$app_stage" || printf '%s\n' "Warning: could not remove the Provenant app stage at $app_stage" >&2
        app_stage=
    fi
elif [ -e "$apps_dir/$app" ]; then
    [ "$quiet_app" = true ] || printf '%s\n' "app: $apps_dir/$app ($(app_version "$apps_dir/$app")); not refreshed"
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
