# Filmagic export

The export pipeline renders sequence frames, encodes them and writes the selected container.
See [architecture](../../docs/architecture.md) for formats, settings, presets and queue behavior.

## MP4 upload compatibility (#3459)

MP4 video exports finalize with the existing ISO BMFF muxer's fast-start layout: `ftyp`, then
`moov` metadata before `mdat` media data. This lets progressive players and upload services read
the track metadata before downloading the media. The choice follows the container, so it applies
to H.264, HEVC and AV1 MP4 exports with software or hardware encoders. QuickTime MOV and
audio-only export layouts are unchanged.

The muxer shifts media in place with a bounded copy buffer and updates sample offsets; it does
not load the entire disk export into memory. Disk outputs therefore need read and write access.
Callers supplying `ExportSettings::output_file` for MP4 video must reserve a readable, writable
file. Buffered writes are flushed before reads through that same handle. In-memory download
sinks use their existing seekable byte buffer. Read, write and seek failures propagate as export
errors.

## Export speed: plain cuts, drafts and smart render (#18)

- **Plain cuts.** A frame that is one opaque 8-bit video picture as it is (one clip at its own size,
  no effects, no colour management, nothing over it) goes to the encoder as the picture's sRGB
  bytes (`filmagic_render::render_sequence_frame`), without building and converting its float image.
  The bytes are the same either way (`render`'s `region_tests` and
  `speed_tests::plain_cuts_skip_the_float_picture_and_write_the_same_file`, which compares whole
  files). SDR exports without alpha, export overlays or reframing only.
- **Drafts.** `presets::draft()` (preset `Draft – Half Size Review`, `file.exportMedia {"draft": true}`):
  half size, `H264Preset::Speed`, CRF 30, AAC 128 kbps.
- **Smart render** (`ExportSettings::smart_render`, `smart.rs`): H.264 in independent segments of one
  keyframe interval, cached by a SHA-256 over the encoder configuration, a probe encode and the
  digests of the Y'CbCr pictures the encoder is given. A re-export copies the segments whose pictures
  did not change; the file is byte-identical to one written with an empty cache
  (`smart_tests`: unchanged, one segment changed, a change across a boundary, other settings,
  damaged cache files, a mutation fuzz of the cache file parser, and an ffmpeg decode with strict
  error detection). Every frame is still rendered: the key comes from the encoder's input, so no
  change to the project, its media or fonts can be missed. Skipping the render of unchanged
  segments would need a key over everything a frame depends on (clips, effects, keyframes, nested
  sequences, source files, fonts, caption tracks, export effects) and is not done.
- **Result.** `Report::encoder` names the encoder, whether hardware was asked for and why each
  hardware encoder declined, GPU and CPU frame counts and the smart-render segment counts;
  `Report::stages` gives the seconds per stage and `Report::notes` what held the export back.

## Animated GIF transparency

Animated GIF supports the existing `ExportSettings::alpha` option (#3396). In Export mode,
choose Animated GIF and enable Video ▸ Include Alpha Channel. Engine and MCP exports use
`file.exportMedia` with `"format": "gif", "alpha": true`; queue entries and presets use the
same settings. The default remains off, flattening the picture over black.

GIF has binary transparency, not partial alpha. After rendering and scaling, RGBA8 alpha
0–127 becomes transparent and 128–255 becomes opaque. Visible pixels retain their straight
RGB color before palette quantization; transparent pixels share a cleared color. This avoids
darkening partially transparent edges, but cannot preserve smooth translucent edges or shadows.
The UI displays the threshold alongside the checkbox.

The existing `image` GIF encoder writes the animation; no codec implementation is added.
Format reference: [GIF89a, 31 July 1990, §23 Graphic Control Extension](https://www.w3.org/Graphics/GIF/spec-gif89a.txt).

`settings_tests::gif_alpha_threshold_and_disposal_roundtrip` exports synthetic moving stripes
with alpha 0, 127, 128 and 255, followed by an empty frame. Decoding verifies transparency,
color, frame timing and removal of previous-frame pixels, with alpha both enabled and disabled.
When FFmpeg is installed, its independent decode must agree on every pixel's alpha and opaque color.
`settings_tests::gif_palette_quantization_preserves_isolated_transparency_and_dark_pixels`
checks two frames with more than 256 distinct post-conversion RGBA colors, forcing palette
reduction. A single transparent pixel moves between frames, surrounded by opaque black and
near-black pixels. Decoded alpha must match exactly; opaque RGB channels allow at most 16/255
quantization error. This test uses the Rust decoder and needs no FFmpeg installation.

Run the GIF export tests with `cargo test -p filmagic-export --lib gif_ -- --nocapture`.
Run all export unit tests with `cargo test -p filmagic-export --lib`.

`filmagic-ui-egui`'s `export_ui::gif_export_exposes_binary_alpha_option` uses the control
channel to select GIF, toggle alpha and inspect the explanatory label. Set
`FILMAGIC_UI_SNAPSHOT_DIR` to render its Export-mode screenshot.
