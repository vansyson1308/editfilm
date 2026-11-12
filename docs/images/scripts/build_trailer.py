#!/usr/bin/env python3
"""Build "Night of the Living Dead - Trailer", the project of the eight gallery screenshots.

Every edit is a Filmagic command sent over the control channel. Run it against a fresh app started
by launch.py, after prepare_media.py:

    python3 docs/images/scripts/build_trailer.py

Sequences: "Trailer" (a 32 s trailer cut from Night of the Living Dead with a Carnival of Souls
insert and a Chopin score), "Charade - Grade" (graded shots of Charade with the Ballade) and
"Earth Views - Look" (NASA Earth Views with a Color Grading look). It saves
target/demo-media/Night of the Living Dead - Trailer.fcproj.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
import fmctl  # noqa: E402

FILMS = os.path.join(fmctl.MEDIA, "films")
MUSIC = os.path.join(fmctl.MEDIA, "music")
PROJECT = os.path.join(fmctl.MEDIA, "Night of the Living Dead - Trailer.fcproj")
MEDIA = ["Night of the Living Dead (1968).mp4", "Charade (1963).mp4", "Carnival of Souls (1962).mp4",
         "Earth Views from the ISS (NASA).mp4"]
SCORES = ["Chopin - Nocturne Op. 48 No. 1.mp3", "Chopin - Ballade No. 1.mp3"]

FPS = 24000 / 1001
# Trailer V1: (source in s, length in frames at 23.976). The first shot is the title card.
TRAILER = [(40.0, 108), (240.0, 72), (310.0, 60), (360.0, 72), (371.0, 60), (401.0, 60), (441.0, 72),
           (501.0, 72), (581.0, 72), (611.0, 72), (711.0, 47)]
DISSOLVES = [1, 4, 7, 10]
CARNIVAL_INSERT = (52.0, 660, 60)  # source in s, sequence frame, length in frames: Mary at the wheel
MARKERS = [(0, "Open", "Mango"), (120, "Cemetery", "Blue"), (324, "Attack", "Forest"),
           (528, "Run", "Red"), (768, "Tag", "Blue")]

# Charade - Grade at 25 fps: (source in s, length in frames)
CHARADE = [(40.0, 75), (12.0, 75), (31.0, 75), (63.0, 50), (82.0, 75), (100.5, 75)]
EARTH = [(30.0, 360), (130.0, 360)]  # Earth Views - Look at 30 fps


def place(c, item, track, frame, sin, frames, **extra):
    p = {"item": item, "track": track, "frame": frame, "sourceIn": {"seconds": sin}, "duration": {"frame": frames}}
    p.update(extra)
    return c.run("timeline.place", p)["clips"]


def main():
    c = fmctl.Filmagic()
    c.run("file.newProject", {"name": "Night of the Living Dead - Trailer"}, check=False)
    r = c.run("file.import", {"paths": [os.path.join(FILMS, n) for n in MEDIA] + [os.path.join(MUSIC, n) for n in SCORES]})
    if r.get("errors"):
        raise SystemExit(f"import failed: {r['errors']}")
    notld, charade, carnival, earth, nocturne, ballade = r["items"]

    # --- Trailer -----------------------------------------------------------------------------
    c.run("file.newSequence", {"name": "Trailer", "width": 1440, "height": 1080, "fps": 23.976, "video": 3, "audio": 3})
    t, v1 = 0, []
    for sin, n in TRAILER:
        v1.append(place(c, notld, "V1", t, sin, n, audioTrack="A1"))
        t += n
    sin, at, n = CARNIVAL_INSERT
    place(c, carnival, "V2", at, sin, n, audioTrack="A3")
    place(c, nocturne, "A2", 0, 0.0, t, video=False)
    for i in DISSOLVES:
        c.run("sequence.applyVideoTransition", {"clip": v1[i][0], "effect": "cross_dissolve", "frames": 12, "edge": "in"})
    title = v1[0][0]
    # the eased push-in on the title card (bezier keyframes give the velocity curve)
    tpf = fmctl.TICKS * 1001 // 24000
    for frame, scale in [(0, 100.0), (107, 180.0)]:
        c.run("effects.setParam", {"clip": title, "effect": "motion", "param": "scale", "value": scale,
                                   "time": frame * tpf, "keyframe": True})
    # keyframes live in media time: the clip's source in plus the offset into the clip
    for frame, ease in [(0, "easeOut"), (107, "easeIn")]:
        c.run("effects.setInterpolation", {"clip": title, "effect": "motion", "param": "scale",
                                           "mediaTime": round(TRAILER[0][0] * fmctl.TICKS) + frame * tpf,
                                           "interpolation": ease})
    for i in [2, 6, 9]:
        c.run("lumetri.applyPreset", {"name": "Hard Mono", "clips": [v1[i][0]]})
    for f, name, color in MARKERS:
        c.run("markers.add", {"target": "program", "frame": f, "name": name, "color": color})

    # --- Charade - Grade ---------------------------------------------------------------------
    c.run("file.newSequence", {"name": "Charade - Grade", "width": 1440, "height": 1080, "fps": 25, "video": 2, "audio": 2})
    t, cv = 0, []
    for sin, n in CHARADE:
        cv.append(place(c, charade, "V1", t, sin, n, audioTrack="A1"))
        t += n
    place(c, ballade, "A2", 0, 0.0, t, video=False)
    first = cv[0][0]
    c.run("effects.apply", {"clips": [first], "effect": "lumetri"})
    for p, v in [("temperature", 4), ("tint", -2), ("exposure", 0.2), ("contrast", 14), ("highlights", -22),
                 ("shadows", 12), ("whites", 6), ("blacks", -8), ("saturation", 106)]:
        c.run("effects.setParam", {"clip": first, "effect": "lumetri", "param": p, "value": v})
    for p, v in [("wheels_on", True), ("wheel_shadows", [0.18, -0.12]), ("wheel_highlights", [0.12, 0.08]),
                 ("vignette_on", True), ("vignette_amount", -0.8)]:
        c.run("effects.setParam", {"clip": first, "effect": "lumetri", "param": p, "value": v}, check=False)
    for k in cv[1:]:
        c.run("lumetri.applyPreset", {"name": "Teal Shadows, Warm Skin", "clips": [k[0]]})

    # --- Earth Views - Look ------------------------------------------------------------------
    c.run("file.newSequence", {"name": "Earth Views - Look", "width": 1280, "height": 720, "fps": 30, "video": 2, "audio": 2})
    t, ev = 0, []
    for sin, n in EARTH:
        ev.append(place(c, earth, "V1", t, sin, n, audio=False))
        t += n
    c.run("sequence.applyVideoTransition", {"clip": ev[1][0], "effect": "cross_dissolve", "frames": 30, "edge": "in"})
    for k in ev:
        c.run("lumetri.applyPreset", {"name": "Golden Dusk", "clips": [k[0]]})

    c.run("file.saveAs", {"path": PROJECT})
    print("saved", os.path.basename(PROJECT))


if __name__ == "__main__":
    main()
