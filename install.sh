#!/bin/sh
# Installs the latest Abla release into ~/.local/bin and adds it to the
# application menu. Safe to run again to update.
set -eu

REPO="levonlab/abla"
BIN_DIR="${ABLA_BIN_DIR:-$HOME/.local/bin}"

arch=$(uname -m)
case "$arch" in
    x86_64|aarch64) ;;
    *)
        echo "abla: there is no prebuilt binary for $arch. See README for building from source." >&2
        exit 1
        ;;
esac

url="https://github.com/$REPO/releases/latest/download/abla-$arch"
tmp=$(mktemp)
sums=$(mktemp)
trap 'rm -f "$tmp" "$sums"' EXIT

echo "Downloading Abla for $arch…"
if ! curl -fsSL "$url" -o "$tmp"; then
    echo "abla: download failed ($url)." >&2
    exit 1
fi

# Check the download against the checksums published with the release.
if curl -fsSL "https://github.com/$REPO/releases/latest/download/SHA256SUMS" -o "$sums"; then
    want=$(awk -v name="abla-$arch" '$2 == name || $2 == "*" name { print $1 }' "$sums")
    have=$(sha256sum "$tmp" | awk '{ print $1 }')
    if [ -z "$want" ] || [ "$want" != "$have" ]; then
        echo "abla: the download does not match its published checksum. Nothing was installed." >&2
        exit 1
    fi
else
    echo "Note: this release was published without checksums, so the download was not verified."
fi
mkdir -p "$BIN_DIR"
install -m 755 "$tmp" "$BIN_DIR/abla"

# Runtime pieces the binary cannot bring along.
missing=""
for lib in libgtk-4.so.1 libgtk4-layer-shell.so.0; do
    if ! ldconfig -p 2>/dev/null | grep -q "$lib"; then
        missing="$missing $lib"
    fi
done
command -v pw-record >/dev/null 2>&1 || missing="$missing pw-record"

if [ -n "$missing" ]; then
    echo
    echo "Missing on this system:$missing"
    echo "Install the runtime libraries, then start Abla:"
    if command -v dnf >/dev/null 2>&1; then
        echo "  sudo dnf install gtk4 gtk4-layer-shell pipewire-utils ydotool"
    elif command -v apt-get >/dev/null 2>&1; then
        echo "  sudo apt install libgtk-4-1 libgtk4-layer-shell0 pipewire-bin ydotool   (Debian 13, Ubuntu 25.10 or newer)"
    elif command -v pacman >/dev/null 2>&1; then
        echo "  sudo pacman -S gtk4 gtk4-layer-shell pipewire ydotool"
    else
        echo "  GTK 4, gtk4-layer-shell, PipeWire and (optionally) ydotool"
    fi
    echo
fi

"$BIN_DIR/abla" desktop

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "Note: $BIN_DIR is not on your PATH. The menu entry works regardless." ;;
esac

echo
echo "Installed $("$BIN_DIR/abla" --version). Start Abla from the application menu, or run:"
echo "  $BIN_DIR/abla"
echo "Then run \`abla doctor\` if anything misbehaves."
