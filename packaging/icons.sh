#!/usr/bin/env bash
# Regenerate every derived app icon from the two hand-drawn sources in assets/app-icon/ (512-unit
# full-bleed tiles with rx=112): filmagic.svg, the full artwork, for 40 px and up, and
# filmagic-small.svg, its simplified version, for 32 px and below and the scalable Linux icon.
#
# Needs: resvg (brew install resvg / cargo install resvg --locked). The .ico and .icns are packed
# by cargo xtask ico / icns on any OS. The outputs are committed, so builds and packaging never
# need these tools.
#
#   packaging/icons.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/assets/app-icon"
SVG="$DIR/filmagic.svg"
SMALL="$DIR/filmagic-small.svg"
APP_ID="io.github.vansyson1308.editfilm"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

command -v resvg >/dev/null || { echo "error: resvg not found (brew install resvg)" >&2; exit 1; }

# macOS icons follow Apple's grid: the tile body is 824 of 1024 px with a transparent margin, so
# the Dock shows it at the same size as other apps. 512 * 1024 / 824 = 636.27 units, margin 62.14.
mac_grid() {
  sed 's/viewBox="0 0 512 512"/viewBox="-62.14 -62.14 636.27 636.27"/' "$1" >"$2"
  grep -q 'viewBox="-62.14' "$2" || { echo "error: unexpected viewBox in $1" >&2; exit 1; }
}
MAC="$TMP/mac.svg"
MAC_SMALL="$TMP/mac-small.svg"
mac_grid "$SVG" "$MAC"
mac_grid "$SMALL" "$MAC_SMALL"

render() { local input="$1" size="$2" output="$3"; resvg -w "$size" -h "$size" "$input" "$output" </dev/null; }
# The source for a size: the simplified drawing up to 32 px, the full one above.
pick() { if [ "$1" -le 32 ]; then echo "$2"; else echo "$3"; fi; }

render "$SVG" 1024 "$DIR/filmagic-1024.png"
# Runtime window/Dock icon on macOS (apps/filmagic/src/main.rs embeds it).
render "$MAC" 512 "$DIR/filmagic-macos-512.png"

# Linux hicolor theme. Windows and Linux use the full-bleed tile.
for s in 16 24 32 48 64 128 256 512; do
  mkdir -p "$DIR/hicolor/${s}x${s}/apps"
  render "$(pick "$s" "$SMALL" "$SVG")" "$s" "$DIR/hicolor/${s}x${s}/apps/$APP_ID.png"
done
mkdir -p "$DIR/hicolor/scalable/apps"
cp "$SMALL" "$DIR/hicolor/scalable/apps/$APP_ID.svg"

# Web app favicon and loading image.
render "$SVG" 128 "$ROOT/apps/filmagic-web/web/favicon.png"

# Windows .ico.
ICO_PNGS=()
for s in 16 20 24 32 40 48 64 128 256; do
  render "$(pick "$s" "$SMALL" "$SVG")" "$s" "$TMP/ico-$s.png"
  ICO_PNGS+=("$TMP/ico-$s.png")
done
(cd "$ROOT" && cargo run -q -p xtask -- ico "$DIR/filmagic.ico" "${ICO_PNGS[@]}")

# macOS .icns, in Apple's grid: 16-1024 px, where 32, 64, 256, 512 and 1024 px also serve as the
# @2x images of 16, 32, 128, 256 and 512 pt.
ICNS_PNGS=()
for s in 16 32 64 128 256 512 1024; do
  render "$(pick "$s" "$MAC_SMALL" "$MAC")" "$s" "$TMP/icns-$s.png"
  ICNS_PNGS+=("$TMP/icns-$s.png")
done
(cd "$ROOT" && cargo run -q -p xtask -- icns "$DIR/filmagic.icns" "${ICNS_PNGS[@]}")
echo "icons written to $DIR"
