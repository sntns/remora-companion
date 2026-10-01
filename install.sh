#!/usr/bin/env bash
#
# Installs the right remora-etcher release binary for the current OS/arch.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/sntns/remora-companion/main/install.sh | bash
#   curl -fsSL .../install.sh | bash -s -- v0.1.0        # install a specific tag
#
# Env vars:
#   REMORA_ETCHER_VERSION   release tag to install (default: latest)
#   REMORA_ETCHER_INSTALL_DIR   where to put the binary (default: see below)

set -euo pipefail

REPO="sntns/remora-companion"
BIN_NAME="remora-etcher"
VERSION="${REMORA_ETCHER_VERSION:-${1:-latest}}"

err() {
    echo "error: $*" >&2
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || err "'$1' is required but not found in PATH"
}

need_cmd curl
need_cmd tar
need_cmd mktemp

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux) os_part="unknown-linux-gnu" ;;
        Darwin) os_part="apple-darwin" ;;
        *) err "unsupported OS '$os' — see README.md for release assets and install manually" ;;
    esac

    case "$arch" in
        x86_64 | amd64) arch_part="x86_64" ;;
        aarch64 | arm64) arch_part="aarch64" ;;
        *) err "unsupported architecture '$arch' — see README.md for release assets and install manually" ;;
    esac

    echo "${arch_part}-${os_part}"
}

resolve_version() {
    if [ "$VERSION" != "latest" ]; then
        echo "$VERSION"
        return
    fi

    api_url="https://api.github.com/repos/${REPO}/releases/latest"
    tag="$(curl -fsSL "$api_url" | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
    [ -n "$tag" ] || err "couldn't resolve the latest release from $api_url (pass a tag explicitly, e.g. REMORA_ETCHER_VERSION=v0.1.0)"
    echo "$tag"
}

pick_install_dir() {
    if [ -n "${REMORA_ETCHER_INSTALL_DIR:-}" ]; then
        echo "$REMORA_ETCHER_INSTALL_DIR"
    elif [ -w "/usr/local/bin" ]; then
        echo "/usr/local/bin"
    else
        echo "${HOME}/.local/bin"
    fi
}

main() {
    target="$(detect_target)"
    tag="$(resolve_version)"
    asset="${BIN_NAME}-${target}.tar.gz"
    url="https://github.com/${REPO}/releases/download/${tag}/${asset}"
    install_dir="$(pick_install_dir)"

    echo "remora-etcher: installing ${tag} (${target}) into ${install_dir}"

    tmp_dir="$(mktemp -d)"
    trap 'rm -rf "$tmp_dir"' EXIT

    curl -fsSL "$url" -o "${tmp_dir}/${asset}" \
        || err "failed to download ${url} (does this release publish a ${target} build?)"

    tar -xzf "${tmp_dir}/${asset}" -C "$tmp_dir"
    [ -f "${tmp_dir}/${BIN_NAME}" ] || err "'${BIN_NAME}' not found inside ${asset}"

    mkdir -p "$install_dir"
    install -m 755 "${tmp_dir}/${BIN_NAME}" "${install_dir}/${BIN_NAME}"

    echo "remora-etcher: installed to ${install_dir}/${BIN_NAME}"
    case ":$PATH:" in
        *":${install_dir}:"*) ;;
        *) echo "warning: ${install_dir} is not in your PATH — add it, e.g. export PATH=\"${install_dir}:\$PATH\"" >&2 ;;
    esac

    "${install_dir}/${BIN_NAME}" --version 2>/dev/null || true
}

main "$@"
