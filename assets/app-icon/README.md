# Filmagic app icon

**Motif:** film + magic. A strip of film crosses the violet tile at a 12° tilt, and a large
four-pointed sparkle bursts from its middle frame, past both edges of the strip. Two smaller
sparkles (gold top right, pale bottom left) balance it. There are no letters, logos or monograms.

## Palette

| Colour | Hex | Used for |
|---|---|---|
| Filmagic violet (app colour) | `#8b5cf6` | the full-bleed field and the sprocket holes (cut through to the field) |
| Ink | `#0b0b0c` | the film strip |
| Frame violet | `#3b2672`, `#4a2f8f` (middle frame) | the picture frames on the strip |
| Pale | `#fff3d6` | the main sparkle's fill, the small bottom-left sparkle |
| Gold | `#ffcf70` | the main sparkle's outline, the small top-right sparkle |

The violet is the app colour used elsewhere (the updater accent `[0x8b, 0x5c, 0xf6]` in
`crates/ui-egui/src/lib.rs`, the DMG window); keep it.

## Geometry

- A 512-unit tile, viewBox exactly `0 0 512 512`, clipped to a rounded square `rx=112`, the violet
  full bleed (no frame or border). `packaging/icons.sh` derives the macOS Apple-grid version by
  rewriting that viewBox, so keep it as is.
- The strip (`rotate(-12 256 256)`): 220 units tall, centred on the tile, two rows of 30 × 28
  sprocket holes on a 56-unit pitch, three 150 × 106 frames.
- The sparkles are four quadratic curves between the four tips, each pulled toward the centre.
  Main sparkle: centre (256, 256), tips 138 units out, 9-unit gold outline.
- `filmagic-small.svg` keeps the tile, the strip and the main sparkle but simplifies them for
  small sizes: a 240-unit strip with 52 × 44 holes on a 92-unit pitch, no frames, no small
  sparkles, and a bigger sparkle (tips 176 units out, 16-unit outline), so it holds at 16 px.

## Which source makes which size

| Size | Source |
|---|---|
| 16, 20, 24, 32 px (`.ico`, `.icns`, hicolor) and the scalable hicolor icon | `filmagic-small.svg` |
| 40 px and up (`.ico`, `.icns`, hicolor, `filmagic-1024.png`, `filmagic-macos-512.png`, the web favicon) | `filmagic.svg` |

The `.icns` and `filmagic-macos-512.png` put the tile in Apple's grid (824 of 1024 px, transparent
margin). Windows, Linux and the web use the full-bleed tile.

## Provenance

Original work for Filmagic by the Filmagic contributors (2026-11-12): hand-written SVG shapes and
paths, drawn from scratch. A film strip with sprocket holes and a four-pointed star are generic
conventions; the drawing is not traced from or modelled on any other product's icon. Licence:
[LICENSE.txt](LICENSE.txt) (MIT OR Apache-2.0).

## Files

| File | What |
|---|---|
| `filmagic.svg` | canonical artwork (about 3 KB) |
| `filmagic-small.svg` | simplified artwork for 32 px and below |
| `filmagic-1024.png` | 1024 px render |
| `filmagic-macos-512.png` | runtime Dock icon on macOS (embedded by `apps/filmagic/src/main.rs`) |
| `filmagic.icns` | macOS bundle icon (`CFBundleIconFile`): 16, 32, 64, 128, 256, 512 and 1024 px, the larger ones also as the @2x images |
| `filmagic.ico` | Windows icon, 16–256 px (embedded in `filmagic.exe` by `apps/filmagic/build.rs`) |
| `hicolor/<size>/apps/ai.storyteller.filmagic.png`, `hicolor/scalable/…svg` | Linux icon theme, used by `packaging/linux/ai.storyteller.filmagic.desktop` |

The web app's `apps/filmagic-web/web/favicon.png` (128 px) comes from the same artwork, and the
macOS DMG window (`packaging/macos/dmg/background.svg`) redraws the motif in its left panel.

## How the icon reaches each OS

- **macOS:** the window icon set at startup (`ViewportBuilder::with_icon`) is the Dock and app-switcher
  icon when the app runs unbundled. The `.app` bundle points `CFBundleIconFile` at `filmagic.icns`.
- **Windows:** `build.rs` embeds `filmagic.ico` as the exe's resource icon (Explorer, Start menu,
  pinned taskbar), and the runtime icon covers the window, taskbar and Alt-Tab.
- **Linux:** the runtime icon plus the app id, which Wayland matches to the `.desktop` file. Install
  the `hicolor/` tree under `/usr/share/icons/hicolor/` and the desktop file under
  `/usr/share/applications/`.

## Regenerate

`packaging/icons.sh` on any OS (needs `resvg`: `cargo install resvg --locked`). It renders every
PNG, copies the scalable icon, and packs the `.ico` and `.icns` with `cargo xtask ico` and
`cargo xtask icns` (no `iconutil` needed). To change the artwork, edit `filmagic.svg` and
`filmagic-small.svg`, then run the script. Every derived file needs its `.attribution` sidecar and
a row in `ATTRIBUTION.md`.
