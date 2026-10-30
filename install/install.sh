#!/bin/sh
# PocketGPU agent installer: curl -fsSL https://pocketgpu.app/install | sh
#
# Installs `pocketgpu` (alias `pgpu`) into ~/.local/bin, starts it at login (a systemd user
# service on Linux, a launchd agent on Apple silicon Macs) and offers to pair a phone. No sudo. Settings, all optional:
#   PGPU_VERSION=v1.0.0       a release tag instead of the latest
#   PGPU_INSTALL_DIR=/path    instead of ~/.local/bin
#   PGPU_NO_SERVICE=1         install only; don't start the service
#   PGPU_REPO=owner/name      GitHub repository with the releases
#   PGPU_DOWNLOAD_URL=url     a mirror holding the release files (used by tests)
set -eu

REPO="${PGPU_REPO:-pocketgpu/pocketgpu-releases}"
INSTALL_DIR="${PGPU_INSTALL_DIR:-$HOME/.local/bin}"
# Public key for the release signature (the same as RELEASE_PUBKEY in the agent). When
# minisign isn't installed, the download is checked against SHA256SUMS only.
MINISIGN_PUBKEY="RWQFc2OekDAK+RAphy78vna45XDYb5pbU3qu7B4d8ERcuzY82sYtkwV4"

has() { command -v "$1" >/dev/null 2>&1; }

# Colors and check marks on a terminal; plain text in logs.
if [ -t 1 ] && [ "${TERM:-dumb}" != "dumb" ] && [ -z "${NO_COLOR:-}" ]; then
    GREEN="$(printf '\033[32m')" DIM="$(printf '\033[2m')" RED="$(printf '\033[31m')"
    BOLD="$(printf '\033[1m')" BADGE="$(printf '\033[30;42m')" RESET="$(printf '\033[0m')"
    TICK="✓" CROSS="✗"
else
    GREEN="" DIM="" RED="" BOLD="" BADGE="" RESET="" TICK="+" CROSS="x"
fi
STEPS=5
STEP=0

say() { printf '%s\n' "$*"; }
fail() { printf '\n  %s%s %s%s\n\n' "$RED" "$CROSS" "$*" "$RESET" >&2; exit 1; }

banner() {
    printf '\n  %s >_ %s  %sPocketGPU%s\n' "$BADGE" "$RESET" "$BOLD" "$RESET"
    printf '        %sYour GPU machine, in your pocket%s\n\n' "$DIM" "$RESET"
}

# `  [2/5] Downloading the agent ....... `, followed by done or a progress bar.
step() {
    STEP=$((STEP + 1))
    label="$1 ......................................"
    printf '  %s[%s/%s]%s %.38s ' "$DIM" "$STEP" "$STEPS" "$RESET" "$label"
}
done_() { printf '%s%s%s %s%s%s\n' "$GREEN" "$TICK" "$RESET" "$DIM" "${1:-}" "$RESET"; }

# Questions read the terminal, since the script itself arrives on stdin.
ask() {
    [ -t 1 ] && [ -r /dev/tty ] || return 1
    printf '\n  %s?%s %s [Y/n] ' "$GREEN" "$RESET" "$1"
    read -r answer </dev/tty || return 1
    case "$answer" in [nN]*) return 1 ;; *) return 0 ;; esac
}

main() {
    banner
    step "Checking this machine"
    [ "$(id -u)" != "0" ] || fail "run this as your normal user, not root. PocketGPU never runs as root; in a container, use the Docker image"
    case "$(uname -s)" in
        Linux) system=linux ;;
        Darwin) system=macos ;;
        *) fail "this installer is for Linux and macOS; on Windows use install.ps1" ;;
    esac

    case "$(uname -m)" in
        x86_64 | amd64) arch="x86_64" ;;
        aarch64 | arm64) arch="aarch64" ;;
        *) fail "unsupported CPU $(uname -m); PocketGPU runs on x86_64 and arm64" ;;
    esac
    if [ "$system" = macos ]; then
        [ "$arch" = aarch64 ] || fail "the macOS agent needs an Apple silicon Mac (M1 or newer)"
        os="macOS $(sw_vers -productVersion 2>/dev/null)"
    else
        check_glibc
        # shellcheck disable=SC1091
        os="$( (. /etc/os-release 2>/dev/null && printf '%s' "${PRETTY_NAME:-Linux}") || printf 'Linux')"
    fi
    done_ "$os, $arch"

    if [ -n "${PGPU_DOWNLOAD_URL:-}" ]; then
        base="$PGPU_DOWNLOAD_URL"
    elif [ -n "${PGPU_VERSION:-}" ]; then
        base="https://github.com/$REPO/releases/download/$PGPU_VERSION"
    else
        base="https://github.com/$REPO/releases/latest/download"
    fi
    file="pocketgpu-$arch-$system.tar.gz"

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT INT TERM
    step "Downloading the agent (${PGPU_VERSION:-latest})"
    printf '\n'
    download "$base/SHA256SUMS" "$tmp/SHA256SUMS" quiet
    download "$base/$file" "$tmp/$file"

    step "Verifying the download"
    signed="$(verify_signature "$base" "$tmp")" || exit 1
    verify_checksum "$tmp" "$file"
    done_ "SHA-256 matches${signed}"

    step "Installing"
    tar -xzf "$tmp/$file" -C "$tmp"
    [ -f "$tmp/pocketgpu" ] || fail "the download has no pocketgpu binary"
    mkdir -p "$INSTALL_DIR"
    # Replace through a temporary name so a running agent isn't hit by a half-written file.
    cp "$tmp/pocketgpu" "$INSTALL_DIR/.pocketgpu.new"
    chmod 755 "$INSTALL_DIR/.pocketgpu.new"
    mv -f "$INSTALL_DIR/.pocketgpu.new" "$INSTALL_DIR/pocketgpu"
    ln -sf pocketgpu "$INSTALL_DIR/pgpu"
    version="$("$INSTALL_DIR/pocketgpu" --version | awk '{print $NF}')"
    case "$INSTALL_DIR" in "$HOME"/*) where="~${INSTALL_DIR#"$HOME"}" ;; *) where="$INSTALL_DIR" ;; esac
    done_ "pgpu $version in $where"

    step "Starting the agent"
    running=""
    if [ -n "${PGPU_NO_SERVICE:-}" ]; then
        done_ "skipped (PGPU_NO_SERVICE)"
    elif [ "$system" = macos ]; then
        state="$("$INSTALL_DIR/pocketgpu" service install --json | sed -n 's/.*"state":"\([^"]*\)".*/\1/p')"
        [ "$state" = "active" ] || fail "the agent did not start ($state). Check with: pgpu doctor"
        running=1
        done_ "running, and starts when you log in"
    elif has systemctl && systemctl --user show-environment >/dev/null 2>&1; then
        state="$("$INSTALL_DIR/pocketgpu" service install --json | sed -n 's/.*"state":"\([^"]*\)".*/\1/p')"
        [ "$state" = "active" ] || fail "the agent did not start ($state). Check with: pgpu doctor"
        running=1
        if [ "$(loginctl show-user "${USER:-$(id -un)}" --property=Linger --value 2>/dev/null)" = "no" ]; then
            done_ "running; stops when you log out"
            linger=1
        else
            done_ "running, and starts at boot"
        fi
    else
        done_ "no systemd here; run it with: pgpu agent"
    fi

    printf '\n  %s%s PocketGPU %s is installed.%s\n' "$GREEN" "$TICK" "$version" "$RESET"

    # shellcheck disable=SC2016
    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *)
            # zsh is the default shell on macOS.
            profile="$HOME/.profile"
            [ "$system" = macos ] && profile="$HOME/.zprofile"
            printf '\n  %sAdd it to your PATH:%s echo '"'"'export PATH="%s:$PATH"'"'"' >> %s\n' "$DIM" "$RESET" "$INSTALL_DIR" "${profile#"$HOME"/}"
            ;;
    esac
    if [ -n "${linger:-}" ]; then
        printf '  %sKeep it running after logout and start it at boot:%s sudo loginctl enable-linger %s\n' "$DIM" "$RESET" "${USER:-$(id -un)}"
    fi

    paired="$("$INSTALL_DIR/pocketgpu" status --json 2>/dev/null | sed -n 's/.*"paired":\([0-9]*\).*/\1/p')"
    if [ -n "$running" ] && [ "${paired:-1}" = "0" ] && ask "Pair your phone now?"; then
        printf '\n'
        "$INSTALL_DIR/pocketgpu" pair </dev/tty
        return
    fi
    printf '\n  %sNext%s\n' "$BOLD" "$RESET"
    [ "${paired:-1}" != "0" ] || printf '    %s%-24s%s%s\n' "$GREEN" "pgpu pair" "$RESET" "${DIM}connect your phone${RESET}"
    printf '    %s%-24s%s%s\n' "$GREEN" "pgpu status" "$RESET" "${DIM}what this machine shares${RESET}"
    printf '    %s%-24s%s%s\n\n' "$GREEN" "pgpu run -- <command>" "$RESET" "${DIM}watch a job from your phone${RESET}"
}

# The release is built for glibc 2.28 and newer (Debian 10, Ubuntu 18.10, RHEL 8).
check_glibc() {
    has ldd || return 0
    version="$(ldd --version 2>&1 | head -n 1 | grep -oE '[0-9]+\.[0-9]+$' || true)"
    [ -n "$version" ] || fail "this system doesn't use glibc (musl, such as Alpine?). Use the Docker image instead"
    major="${version%%.*}"
    minor="${version#*.}"
    if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 28 ]; }; then
        fail "glibc $version is too old; PocketGPU needs 2.28 or newer"
    fi
}

# A progress bar on a terminal; `quiet` for small files.
download() {
    if has curl; then
        if [ -t 2 ] && [ "${3:-}" != "quiet" ]; then
            curl -fL --retry 3 --progress-bar -o "$2" "$1" || fail "could not download $1"
        else
            curl -fsSL --retry 3 -o "$2" "$1" || fail "could not download $1"
        fi
    elif has wget; then
        if [ -t 2 ] && [ "${3:-}" != "quiet" ]; then
            wget -q --show-progress -O "$2" "$1" || fail "could not download $1"
        else
            wget -q -O "$2" "$1" || fail "could not download $1"
        fi
    else
        fail "install curl or wget first"
    fi
}

verify_checksum() {
    want="$(grep " $2\$" "$1/SHA256SUMS" | cut -d ' ' -f 1)"
    [ -n "$want" ] || fail "SHA256SUMS has no entry for $2"
    if has sha256sum; then
        got="$(sha256sum "$1/$2" | cut -d ' ' -f 1)"
    elif has shasum; then
        got="$(shasum -a 256 "$1/$2" | cut -d ' ' -f 1)"
    else
        fail "install sha256sum (coreutils) to check the download"
    fi
    [ "$want" = "$got" ] || fail "checksum mismatch for $2; the download is damaged or was changed"
}

verify_signature() {
    [ -n "$MINISIGN_PUBKEY" ] || return 0
    if ! has minisign; then
        printf ', signature not checked (install minisign to check it)'
        return 0
    fi
    download "$1/SHA256SUMS.minisig" "$2/SHA256SUMS.minisig" quiet
    minisign -Vqm "$2/SHA256SUMS" -P "$MINISIGN_PUBKEY" >/dev/null || fail "the release signature is not valid"
    printf ', signature valid'
}

main "$@"
