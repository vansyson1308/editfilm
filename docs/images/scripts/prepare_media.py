#!/usr/bin/env python3
"""Download the public-domain / CC0 footage for the README screenshots and cut it into reels.

Nothing this script writes may be committed: it all goes under target/demo-media/ (git-ignored).
ffmpeg is used only as a fixture generator (cutting and re-encoding reels); Filmagic never links it.

    python3 docs/images/scripts/prepare_media.py            # download + cut everything
    python3 docs/images/scripts/prepare_media.py --no-download

Sources (check each page still marks the item as public domain / CC0 before using it):
  NASA (US Government works, public domain; NASA does not endorse this project), images.nasa.gov
  archive.org (Public Domain Mark films, CC0 Musopen recordings)
"""
import argparse
import os
import subprocess
import sys
import urllib.parse
import urllib.request

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
MEDIA = os.path.join(ROOT, "target", "demo-media")
SRC = os.path.join(MEDIA, "src")

NASA = "https://images-assets.nasa.gov/video/{id}/{id}~{rendition}.mp4"
ARCHIVE = "https://archive.org/download/{item}/{file}"

DOWNLOADS = {
    # file name: URL
    "broll.mp4": NASA.format(id="KSC-19690716-MH-NAS01-0001-Apollo_11_Historical_Footage_and_Broll-DVC_1560", rendition="medium"),
    "hm0083.mp4": NASA.format(id="KSC-19690716-MH-NAS01-0001-Apollo_11_Breakfast_C7Exit_Convoy_WhiteRmHatchClosing_Launch-HM_0083", rendition="medium"),
    "filmrolls.mp4": NASA.format(id="KSC-19690716-MH-NAS01-0001-Apollo_11_Mission_Control_Center_Film_Rolls1", rendition="large"),
    "moonwalk.mp4": NASA.format(id="Apollo_11_moonwalk_montage_720p", rendition="medium"),
    "earthviews.mp4": NASA.format(id="NHQ_2020_1221_Earth%20Views", rendition="medium"),
    "notld.mp4": ARCHIVE.format(item="night-of-the-living-dead-1968-english", file=urllib.parse.quote("Night of the Living Dead (1968) English.mp4")),
    "carnival.mp4": ARCHIVE.format(item="CarnivalOfSoulsVideoQualityUpgrade", file="CarnivalOfSoulsVideoUpgrade.mp4"),
    "charade.mp4": ARCHIVE.format(item="charade-1963-cary-grant-audrey-hepburn-comedy-mystery-romance-thriller-full-movie", file="Charade_READY.mp4"),
    "nocturne48.mp3": ARCHIVE.format(item="musopen-chopin", file=urllib.parse.quote("Nocturne Op. 48 no. 1 in C minor.mp3")),
    "ballade1.mp3": ARCHIVE.format(item="musopen-chopin", file=urllib.parse.quote("Ballade no. 1 - Op. 23.mp3")),
}

# The NASA transfers are pillarboxed and horizontally stretched: crop the active picture and
# restore 4:3 by scaling it to 1440x1080.
BROLL = "crop=1036:580:116:12,scale=1440:1080,setsar=1"
OTV = "crop=1020:580:142:8,scale=1440:1080,setsar=1"
FILMROLLS = "crop=960:720:160:0,scale=1440:1080,setsar=1,fps=30"
MOONWALK = "crop=960:720:160:0,scale=1440:1080,setsar=1"
FILM43 = "scale=1440:1080,setsar=1"
CHARADE = "crop=922:692:179:14,scale=1440:1080,setsar=1"

# (output, source, start s, duration s or None for all, video filter, frame rate)
REELS = [
    ("apollo11/A11 R01 Pre-launch.mp4", "broll.mp4", 120, 456, BROLL, 30),
    ("apollo11/A11 R02 Launch.mp4", "broll.mp4", 660, 320, BROLL, 30),
    ("apollo11/A11 R03 Translunar and Descent.mp4", "broll.mp4", 980, 270, BROLL, 30),
    ("apollo11/A11 R04 Lunar Surface.mp4", "broll.mp4", 1300, 420, BROLL, 30),
    ("apollo11/A11 R05 Recovery.mp4", "broll.mp4", 1740, 230, BROLL, 30),
    ("apollo11/A11 LCC Firing Room (silent).mp4", "filmrolls.mp4", 0, 420, FILMROLLS, 30),
    ("apollo11/A11 Launch OTV Countdown.mp4", "hm0083.mp4", 2340, 426, OTV, 30),
    ("apollo11/A11 EVA TV Montage.mp4", "moonwalk.mp4", 0, None, MOONWALK, 30),
    ("films/Night of the Living Dead (1968).mp4", "notld.mp4", 0, 720, FILM43, 24000 / 1001),
    ("films/Carnival of Souls (1962).mp4", "carnival.mp4", 600, 600, FILM43, 30000 / 1001),
    ("films/Charade (1963).mp4", "charade.mp4", 300, 600, CHARADE, 25),
    ("films/Earth Views from the ISS (NASA).mp4", "earthviews.mp4", 1600, 264, "setsar=1", 30),
]

AUDIO = [
    ("music/Chopin - Nocturne Op. 48 No. 1.mp3", "nocturne48.mp3"),
    ("music/Chopin - Ballade No. 1.mp3", "ballade1.mp3"),
]


def download(name, url):
    out = os.path.join(SRC, name)
    if os.path.exists(out) and os.path.getsize(out) > 0:
        return
    print("download", url, flush=True)
    tmp = out + ".part"
    with urllib.request.urlopen(url) as r, open(tmp, "wb") as f:
        while True:
            chunk = r.read(1 << 20)
            if not chunk:
                break
            f.write(chunk)
    os.replace(tmp, out)


def cut(out, src, start, dur, vf, rate):
    dst = os.path.join(MEDIA, out)
    if os.path.exists(dst):
        return
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    cmd = ["ffmpeg", "-v", "error", "-y", "-ss", str(start), "-i", os.path.join(SRC, src)]
    if dur:
        cmd += ["-t", str(dur)]
    cmd += ["-vf", vf, "-r", str(rate), "-c:v", "libx264", "-preset", "veryfast", "-crf", "20",
            "-profile:v", "high", "-pix_fmt", "yuv420p", "-g", str(round(rate)),
            "-c:a", "aac", "-b:a", "160k", "-ar", "48000", "-ac", "2",
            "-movflags", "+faststart", dst + ".tmp.mp4"]
    print("cut", out, flush=True)
    subprocess.run(cmd, check=True)
    os.replace(dst + ".tmp.mp4", dst)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--no-download", action="store_true")
    a = ap.parse_args()
    os.makedirs(SRC, exist_ok=True)
    if not a.no_download:
        for name, url in DOWNLOADS.items():
            download(name, url)
    for reel in REELS:
        cut(*reel)
    for out, src in AUDIO:
        dst = os.path.join(MEDIA, out)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        if not os.path.exists(dst):
            with open(os.path.join(SRC, src), "rb") as i, open(dst, "wb") as o:
                o.write(i.read())
    return 0


if __name__ == "__main__":
    sys.exit(main())
