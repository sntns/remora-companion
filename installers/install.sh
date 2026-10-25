#!/bin/sh
# Installs rmra or remora-etcher from a remora-companion release, then
# enables its shell completion:
#
#   curl --proto '=https' --tlsv1.2 -LsSf \
#     https://github.com/sntns/remora-companion/releases/latest/download/install.sh | sh -s rmra
#
# The binary comes from the release's own installer (dist's
# <binary>-installer.sh: same install directory, same PATH setup, same
# environment variables); the completion from `<binary> completion
# --install`, which adds one guarded line to your shell's startup file,
# once. NO_COMPLETION=1 skips that part.
set -eu

release="@RELEASE@"
binary="${1:-rmra}"
case "$binary" in
    rmra | remora-etcher) ;;
    *)
        echo "install.sh: unknown binary '$binary' (rmra or remora-etcher)" >&2
        exit 2
        ;;
esac

# A script published with a release installs that release; one run from the
# repository, the latest.
case "$release" in
    @*) base="https://github.com/sntns/remora-companion/releases/latest/download" ;;
    *) base="https://github.com/sntns/remora-companion/releases/download/$release" ;;
esac

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
curl --proto '=https' --tlsv1.2 -LsSf "$base/$binary-installer.sh" -o "$work/installer.sh"
# The installer says where it put the binary; its output still shows. A
# pipeline's status is its last command's, so the installer's own is kept.
{
    status=0
    sh "$work/installer.sh" || status=$?
    echo "$status" >"$work/status"
} | tee "$work/log"
status="$(cat "$work/status")"
if [ "$status" != "0" ]; then
    exit "$status"
fi

if [ "${NO_COMPLETION:-0}" != "0" ]; then
    exit 0
fi
dir="$(sed -n 's/^installing to //p' "$work/log" | tail -n 1)"
if [ -n "$dir" ] && [ -x "$dir/$binary" ]; then
    executable="$dir/$binary"
elif command -v "$binary" >/dev/null 2>&1; then
    executable="$(command -v "$binary")"
else
    echo "install.sh: installed, but $binary wasn't found to enable its completion: run \`$binary completion --install\`" >&2
    exit 0
fi
# Never fails the install: completion is a nicety.
"$executable" completion --install || true
