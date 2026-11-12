# README screenshot scripts

These scripts rebuild the two demo projects and retake the nine screenshots in `docs/images/`.
Every edit is a Filmagic command sent over the app's own control channel
([docs/control-protocol.md](../../control-protocol.md)); the pictures are taken with its
`ui.screenshot` method. They use the Python 3 standard library only.

| Script | What it does |
|---|---|
| `prepare_media.py` | downloads the footage and music below into `target/demo-media/src/` and cuts the reels the projects use (ffmpeg as a fixture generator only) |
| `launch.py` | starts `target/release/filmagic --control <port>` with a fresh token and data folder; on Linux without a display it starts Xvfb first |
| `fmctl.py` | the control-channel client, and a small PNG reader/writer (crop, halve, lossless re-compression) |
| `build_apollo.py` | builds `Apollo 11 - Tranquility.fcproj`, the 3:12 documentary cut in the hero shot |
| `build_trailer.py` | builds `Night of the Living Dead - Trailer.fcproj` (sequences Trailer, Charade - Grade, Earth Views - Look) for the gallery |
| `capture_hero.py` | sets up the Color workspace on the Apollo cut and captures `filmagic-hero.png` |
| `capture_gallery.py` | captures the other eight shots, each in a fresh app |

Nothing these scripts download or write may be committed: media, projects, render previews and raw
captures all stay under `target/demo-media/` (git-ignored). Only the nine PNGs in `docs/images/`
are kept, with their `.attribution` sidecars.

## What is downloaded

Check each source page before use: the item must be the one named here and still be marked public
domain (or CC0).

| File in `target/demo-media/src/` | Work | Source | Status |
|---|---|---|---|
| `broll.mp4` | Apollo 11 25th Anniversary "B" Roll Footage (NASA KSC), `~medium.mp4` | https://images.nasa.gov/details/KSC-19690716-MH-NAS01-0001-Apollo_11_Historical_Footage_and_Broll-DVC_1560 | US Government work, public domain |
| `hm0083.mp4` | Apollo 11 Activities: breakfast … launch count and launch (NASA KSC), `~medium.mp4` | https://images.nasa.gov/details/KSC-19690716-MH-NAS01-0001-Apollo_11_Breakfast_C7Exit_Convoy_WhiteRmHatchClosing_Launch-HM_0083 | US Government work, public domain |
| `filmrolls.mp4` | Apollo 11 Mission Control Center - Film Rolls (NASA KSC, no audio), `~large.mp4` | https://images.nasa.gov/details/KSC-19690716-MH-NAS01-0001-Apollo_11_Mission_Control_Center_Film_Rolls1 | US Government work, public domain |
| `moonwalk.mp4` | Apollo 11 moonwalk montage (NASA HQ), `~medium.mp4` | https://images.nasa.gov/details/Apollo_11_moonwalk_montage_720p | US Government work, public domain |
| `earthviews.mp4` | Earth Views from the ISS - No Music (NASA HQ, NHQ_2020_1221), `~medium.mp4` | https://images.nasa.gov/details/NHQ_2020_1221_Earth%20Views | US Government work, public domain |
| `notld.mp4` | Night of the Living Dead (1968) | https://archive.org/details/night-of-the-living-dead-1968-english | Public Domain Mark |
| `carnival.mp4` | Carnival of Souls (1962) | https://archive.org/details/CarnivalOfSoulsVideoQualityUpgrade | Public Domain Mark |
| `charade.mp4` | Charade (1963) | https://archive.org/details/charade-1963-cary-grant-audrey-hepburn-comedy-mystery-romance-thriller-full-movie | Public Domain Mark |
| `nocturne48.mp3`, `ballade1.mp3` | Chopin, Nocturne Op. 48 No. 1 and Ballade No. 1, recorded for Musopen | https://archive.org/details/musopen-chopin | CC0-1.0 |

NASA does not endorse this project. The subtitles in the hero shot quote NASA's *Apollo 11
Technical Air-to-Ground Voice Transcription* (July 1969, a US Government work):
https://www.nasa.gov/wp-content/uploads/static/history/alsj/a11/a11transcript_tec.html. The lines
and their GET are listed in `build_apollo.py`.

## How the reels are cut

`prepare_media.py` re-encodes each excerpt as H.264 High, 8-bit 4:2:0, a keyframe every second,
AAC 48 kHz stereo. The NASA transfers are pillarboxed and stretched horizontally, so the script
crops the active picture and scales it back to 4:3 (1440x1080):

| Reel (`target/demo-media/…`) | From | Start, length |
|---|---|---|
| `apollo11/A11 R01 Pre-launch.mp4` | `broll.mp4` | 120 s, 456 s |
| `apollo11/A11 R02 Launch.mp4` | `broll.mp4` | 660 s, 320 s |
| `apollo11/A11 R03 Translunar and Descent.mp4` | `broll.mp4` | 980 s, 270 s |
| `apollo11/A11 R04 Lunar Surface.mp4` | `broll.mp4` | 1300 s, 420 s |
| `apollo11/A11 R05 Recovery.mp4` | `broll.mp4` | 1740 s, 230 s |
| `apollo11/A11 LCC Firing Room (silent).mp4` | `filmrolls.mp4` (59.94 fps, to 30 fps) | 0 s, 420 s |
| `apollo11/A11 Launch OTV Countdown.mp4` | `hm0083.mp4` | 2340 s, 426 s |
| `apollo11/A11 EVA TV Montage.mp4` | `moonwalk.mp4` | whole file |
| `films/Night of the Living Dead (1968).mp4` | `notld.mp4` (to 1440x1080, 23.976 fps) | 0 s, 720 s |
| `films/Carnival of Souls (1962).mp4` | `carnival.mp4` (to 1440x1080, 29.97 fps) | 600 s, 600 s |
| `films/Charade (1963).mp4` | `charade.mp4` (4:3 centre crop to 1440x1080, 25 fps) | 300 s, 600 s |
| `films/Earth Views from the ISS (NASA).mp4` | `earthviews.mp4` (1280x720, 30 fps) | 1600 s, 264 s |

The two MP3s are copied to `target/demo-media/music/` under their titles.

## Running it

Build first (Linux needs the system packages listed in [docs/contributing.md](../../contributing.md)):

```sh
cargo build --release -p filmagic -p filmagic-cli
python3 docs/images/scripts/prepare_media.py
```

The scripts run on **Linux under X11 or Xvfb** only: they set the density with
`WINIT_X11_SCALE_FACTOR`, which only X11 honours, and they stop with an error if the app reports
another pixels-per-point value or a capture comes out at another size (a display smaller than the
window shrinks it). On a machine without a GPU or display (a CI runner or cloud container), install
Xvfb and Mesa's software Vulkan driver (`apt-get install -y xvfb mesa-vulkan-drivers
libxkbcommon-x11-0`); `launch.py` starts Xvfb on `:77` when no X display is running, and wgpu picks
up llvmpipe. The window needs a screen of at least 4800x2570 pixels (Xvfb is started at 5200x3000).

Build the projects (each in a fresh app; the Apollo render of In to Out takes several minutes on
llvmpipe):

```sh
eval "$(python3 docs/images/scripts/launch.py --scale 1.5)"
python3 docs/images/scripts/build_apollo.py
eval "$(python3 docs/images/scripts/launch.py --stop)"
eval "$(python3 docs/images/scripts/launch.py --scale 1.5)"
python3 docs/images/scripts/build_trailer.py
eval "$(python3 docs/images/scripts/launch.py --stop)"
```

`launch.py --stop` prints the `unset` lines for what `launch.py` exported (`DISPLAY` too when it
started Xvfb), so the `eval` leaves the shell as it was.

Capture:

```sh
eval "$(python3 docs/images/scripts/launch.py --scale 2)"
python3 docs/images/scripts/capture_hero.py      # 2400x1285 points at 2x, halved to 2400 px
eval "$(python3 docs/images/scripts/launch.py --stop)"
python3 docs/images/scripts/capture_gallery.py   # starts its own app for each of the eight shots
```

`capture_gallery.py name…` retakes single shots (`assembly`, `keyframes`, `export`, `color`,
`timeline`, `scopes`, `color-panel`, `effects`). The full-window shots are taken at 1.5x
(1600x1000 points = 2400x1500 px); the crops at 2x, cut from the window at the panel's corner
(the Scopes window is resized until the Video Scopes group is exactly 1100x1115 px). Every shot
widens the Audio Meters to 170 points with a docking action, so their tab title fits.

Open every PNG before committing it: right content, no rendering glitches, sharp text, and nothing
from the capturing machine (no paths in the title, status bar, bins or Export summary, no user
names). The scripts set the export location to the neutral `/media/exports` and hover the timeline
so the status bar shows its hint rather than the last saved path.
