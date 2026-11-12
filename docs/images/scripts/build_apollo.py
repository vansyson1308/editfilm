#!/usr/bin/env python3
"""Build "Apollo 11 - Tranquility", the documentary cut in the README hero shot.

Every edit is a Filmagic command sent over the control channel; run it against a fresh app
started by launch.py, after prepare_media.py has cut the reels:

    python3 docs/images/scripts/build_apollo.py

It saves target/demo-media/Apollo 11 - Tranquility.fcproj. Times below are sequence seconds
(30 fps); source in points are seconds into the reels prepare_media.py cuts.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
import fmctl  # noqa: E402

FPS = 30
TPF = fmctl.TICKS // FPS  # ticks per frame
A11 = os.path.join(fmctl.MEDIA, "apollo11")
MUSIC = os.path.join(fmctl.MEDIA, "music")
PROJECT = os.path.join(fmctl.MEDIA, "Apollo 11 - Tranquility.fcproj")
SEQ_NAME = "APOLLO 11 - Tranquility - v07 picture lock"

SELECTS = ["A11 R01 Pre-launch.mp4", "A11 R02 Launch.mp4", "A11 R03 Translunar and Descent.mp4",
           "A11 R04 Lunar Surface.mp4", "A11 R05 Recovery.mp4", "A11 LCC Firing Room (silent).mp4"]
MISSION = ["A11 Launch OTV Countdown.mp4", "A11 EVA TV Montage.mp4"]
SCORE = "Chopin - Nocturne Op. 48 No. 1.mp3"

# The picture cut on V1: (reel, source in s, length s). Lengths add up to 3:12.
V1 = [
    # Pre-launch, 0:00-0:46.5
    ("R01", 220.0, 5.0), ("R01", 206.6, 4.0), ("R01", 18.0, 4.0), ("R01", 23.0, 3.5), ("R01", 30.0, 3.5),
    ("R01", 227.0, 4.0), ("R01", 276.0, 4.0), ("R01", 296.0, 4.0), ("R01", 305.0, 3.5), ("R01", 319.0, 4.0),
    ("R01", 368.0, 4.0), ("R01", 382.0, 3.0),
    # Launch, 0:46.5-1:34; ignition at 1:13 (T-0), the liftoff shot from 1:16
    ("R02", 6.8, 4.0), ("R02", 17.5, 4.0), ("R02", 24.3, 3.0), ("R02", 57.0, 4.0), ("R02", 13.0, 3.5),
    ("R02", 99.6, 2.5), ("R02", 102.5, 2.5), ("R02", 105.5, 3.0), ("R02", 72.0, 3.0), ("R02", 180.0, 6.0),
    ("R02", 140.0, 3.5), ("R02", 223.0, 3.0), ("R02", 236.0, 6.5),
    # Translunar coast and descent, 1:34-2:05
    ("R03", 3.0, 4.0), ("R03", 106.0, 4.0), ("R03", 142.0, 3.5), ("R03", 193.0, 4.0), ("R03", 215.0, 4.0),
    ("R03", 227.0, 4.0), ("R03", 257.0, 3.5), ("R03", 261.8, 4.0),
    # Lunar surface, 2:05-2:47
    ("R04", 20.0, 4.0), ("R04", 60.0, 6.0), ("R04", 110.0, 5.0), ("R04", 150.0, 4.5), ("R04", 190.0, 5.0),
    ("R04", 240.0, 5.0), ("R04", 305.0, 5.0), ("R04", 338.0, 4.0), ("R04", 353.0, 3.5),
    # Recovery, 2:47-3:12
    ("R05", 10.0, 4.0), ("R05", 93.0, 4.0), ("R05", 138.0, 4.0), ("R05", 162.0, 3.0), ("R05", 193.0, 3.5),
    ("R05", 209.0, 3.0), ("R05", 225.6, 2.5),
]
LIFTOFF = 21  # index in V1 of the shot the hero shows

# B-roll cutaways on V2 (picture only): (reel, source in, sequence time, length)
V2 = [("LCC", 40.0, 52.0, 3.0), ("LCC", 120.0, 62.0, 2.5), ("LCC", 300.0, 113.0, 3.0), ("LCC", 360.0, 134.0, 2.5)]

# Lower thirds and section titles on V3: (time, length, title, subtitle)
TITLES = [
    (1.5, 5.0, "APOLLO 11", "TRANQUILITY"),
    (74.0, 9.0, "LAUNCH COMPLEX 39A  ·  KENNEDY SPACE CENTER", "16 JULY 1969   09:32 EDT   T+00:00:00"),
    (95.5, 5.0, "COLUMBIA AND EAGLE", "TRANSLUNAR COAST"),
    (125.5, 5.0, "TRANQUILITY BASE", "20 JULY 1969"),
    (172.0, 5.0, "SPLASHDOWN", "24 JULY 1969  ·  PACIFIC OCEAN"),
]

# Subtitles from NASA's Apollo 11 Technical Air-to-Ground Voice Transcription (speaker codes:
# CDR Armstrong, LMP Aldrin, CC the capsule communicator in Houston), with the GET of each line.
CAPTIONS = [
    (74.2, 2.6, "ARMSTRONG: Roger. Clock."),                      # 00 00 00 04
    (76.8, 3.7, "ARMSTRONG: Roger. We got a roll program."),       # 00 00 00 13
    (86.0, 3.8, "HOUSTON: Apollo 11, Houston. You're good at 1 minute."),  # 00 00 01 02
    (114.5, 3.0, "ARMSTRONG: It's a 1202."),                        # 04 06 38 30
    (119.5, 2.5, "ALDRIN: Contact light."),                          # 04 06 45 40
    (122.2, 3.0, "ARMSTRONG: Houston, Tranquility Base here."),      # 04 06 45 59
    (125.4, 3.0, "ARMSTRONG: The Eagle has landed."),                # 04 06 46 04
    (128.6, 4.4, "HOUSTON: Roger, Tranquility. We copy you on the ground."),  # 04 06 46 06
    (133.2, 4.6, "HOUSTON: You got a bunch of guys about to turn blue. We're breathing again. Thanks a lot."),
    (140.0, 5.5, "ARMSTRONG: That's one small step for (a) man, one giant leap for mankind."),  # 04 13 24 48
    (150.0, 2.6, "ALDRIN: Beautiful view!"),                         # 04 13 43 16
    (152.8, 3.2, "ALDRIN: Magnificent desolation."),                 # 04 13 43 24
]

MARKERS = [
    (0.0, "Head", "Mango"), (5.0, "Pad at dawn", "Blue"), (40.0, "Crew walkout", "Blue"),
    (64.0, "T-9 s", "Forest"), (73.0, "Ignition", "Rose"), (76.0, "Liftoff", "Red"),
    (88.5, "Tower clear", "Violet"), (104.0, "Earthrise", "Iris"), (114.5, "1202 alarm", "Mango"),
    (125.4, "Landing", "Forest"), (140.0, "First step", "Blue"), (167.0, "Homeward", "Blue"),
    (177.0, "Splashdown", "Cerulean"),
]

# Mission audio on A2 (sound only): (file, source in, time, length). T-0 of the countdown
# (207 s into the OTV reel) lands on the ignition cut at 1:13.
A2 = [(MISSION[0], 180.5, 46.5, 47.5), (MISSION[1], 0.0, 138.0, 28.0)]


def f(s):
    return round(s * FPS)


def main():
    c = fmctl.Filmagic()
    c.run("file.newProject", {"name": "Apollo 11 - Tranquility"}, check=False)
    bins = {}
    for name in ["01 Selects - NASA film", "02 Mission audio", "03 Music", "04 Sequences"]:
        bins[name] = c.run("file.newBin", {"name": name})["bin"]
    items = {}
    for names, folder, b in [(SELECTS, A11, "01 Selects - NASA film"), (MISSION, A11, "02 Mission audio"),
                             ([SCORE], MUSIC, "03 Music")]:
        r = c.run("file.import", {"paths": [os.path.join(folder, n) for n in names], "bin": bins[b]})
        if r.get("errors"):
            raise SystemExit(f"import failed: {r['errors']}")
        items.update(zip(names, r["items"]))
    reel = {n.split()[1]: items[n] for n in SELECTS}

    seq = c.run("file.newSequence", {"name": SEQ_NAME, "width": 1920, "height": 1080, "fps": FPS,
                                     "video": 3, "audio": 3, "bin": bins["04 Sequences"]})["sequence"]
    for track, name in [("V1", "Picture"), ("V2", "B-roll"), ("V3", "Titles"),
                        ("A1", "Nat sound"), ("A2", "Mission audio"), ("A3", "Music")]:
        c.run("timeline.setTrack", {"track": track, "name": name})

    # V1 + A1: the picture cut with its own sound
    t, v1 = 0, []
    for r, sin, dur in V1:
        res = c.run("timeline.place", {"item": reel[r], "track": "V1", "audioTrack": "A1", "frame": t,
                                       "sourceIn": {"seconds": sin}, "duration": {"frame": f(dur)}})
        v1.append(res["clips"])
        t += f(dur)
    # V2 cutaways, picture only
    for r, sin, at, dur in V2:
        c.run("timeline.place", {"item": reel[r], "track": "V2", "frame": f(at), "audio": False,
                                 "sourceIn": {"seconds": sin}, "duration": {"frame": f(dur)}})
    # A2 mission audio, A3 music bed
    a2 = []
    for name, sin, at, dur in A2:
        a2 += c.run("timeline.place", {"item": items[name], "track": "A2", "frame": f(at), "video": False,
                                       "sourceIn": {"seconds": sin}, "duration": {"frame": f(dur)}})["clips"]
    music = c.run("timeline.place", {"item": items[SCORE], "track": "A3", "frame": 0,
                                     "sourceIn": {"seconds": 0}, "duration": {"frame": t}})["clips"]

    # Dissolves at the section changes and a few inside them; dips from and to black at the ends
    for i in [0, 1, 12, 20, 25, 33, 34, 42, 43, 47]:
        c.run("sequence.applyVideoTransition", {"clip": v1[i][0], "effect": "cross_dissolve", "frames": 20, "edge": "in"})
    c.run("sequence.applyVideoTransition", {"clip": v1[-1][0], "effect": "Dip to Black", "frames": 30, "edge": "out"})
    for clip in a2 + music:
        for edge in ["in", "out"]:
            c.run("sequence.applyAudioTransition", {"clip": clip, "effect": "constant_power", "frames": 24, "edge": edge})

    # Titles (Inter Bold over Inter Regular, lower-left) on V3
    for at, dur, title, sub in TITLES:
        top = c.run("graphics.newText", {"text": title, "track": 2, "time": f(at) * TPF, "seconds": dur,
                                         "size": 46, "font": "Inter", "fontStyle": "Bold", "position": [300, 790]})
        clip = top["clip"]
        c.run("graphics.set", {"clip": clip, "layer": 0, "props": {"tracking": 60, "fill_color": "#ffffff",
                                                                    "shadow": True, "background": True}})
        c.run("graphics.newText", {"text": sub, "clip": clip, "size": 30, "font": "Inter", "fontStyle": "Regular",
                                   "position": [304, 850]})
        c.run("graphics.set", {"clip": clip, "layer": 1, "props": {"tracking": 120, "fill_color": "#f2c14e",
                                                                    "shadow": True}})
        c.run("clip.rename", {"clip": clip, "name": title.split("  ")[0].title()})

    # Caption track
    track = c.run("captions.newTrack", {"format": "Subtitle", "name": "EN - Air-to-ground", "language": "en"})["track"]
    c.run("captions.setStyle", {"track": track, "font": "Inter", "fontStyle": "SemiBold", "size": 44,
                                "background": False, "outline": 3, "outlineColor": "#000000", "margin": 0.06})
    for at, dur, text in CAPTIONS:
        c.run("captions.add", {"track": track, "text": text, "seconds": at, "durationSeconds": dur})

    # Grades: a warm print look on the launch, the liftoff shot graded by hand with a push-in
    for i in range(12, 25):
        c.run("lumetri.applyPreset", {"name": "Warm Negative", "clips": [v1[i][0]]})
    for i in range(33, 42):
        c.run("lumetri.applyPreset", {"name": "Silver Retention", "clips": [v1[i][0]]})
    hero = v1[LIFTOFF][0]
    for p, v in [("temperature", -6), ("tint", -2), ("saturation", 112), ("contrast", 22), ("highlights", -34),
                 ("shadows", 10), ("whites", -8), ("blacks", -10), ("vignette_on", True), ("vignette_amount", -1.2)]:
        c.run("effects.setParam", {"clip": hero, "effect": "lumetri", "param": p, "value": v})
    start = f(76.0) * TPF
    for dt, scale, exposure in [(0, 100.0, -0.9), (3.0, 104.0, -0.5), (5.9, 108.0, 0.1)]:
        tt = start + round(dt * fmctl.TICKS)
        c.run("effects.setParam", {"clip": hero, "effect": "motion", "param": "scale", "value": scale,
                                   "time": tt, "keyframe": True})
        c.run("effects.setParam", {"clip": hero, "effect": "lumetri", "param": "exposure", "value": exposure,
                                   "time": tt, "keyframe": True})

    # Sound: dialogue on the mission audio, music ducked under it, nat sound lowered
    c.run("essentialSound.setType", {"clips": a2, "type": "dialogue"})
    c.run("essentialSound.setType", {"clips": music, "type": "music"})
    c.run("essentialSound.set", {"clips": music, "values": {"ducking.enabled": True, "ducking.against": ["dialogue"],
                                                             "ducking.reduceDb": -14, "ducking.fadeS": 0.8}})
    c.run("essentialSound.generateDucking", {"clips": music})
    c.run("clip.audioGain", {"clips": [x[1] for x in v1 if len(x) > 1], "mode": "adjust", "db": -9})

    for at, name, color in MARKERS:
        c.run("markers.add", {"target": "program", "frame": f(at), "name": name, "color": color})
    c.run("markers.markIn", {"timecode": "00:00:46:15"})
    c.run("markers.markOut", {"timecode": "00:01:34:00"})
    c.run("file.saveAs", {"path": PROJECT})
    c.run("sequence.renderInToOut", {})  # a background job; with llvmpipe it takes minutes
    c.wait_jobs(limit=3600)
    c.run("file.save", {})
    print("saved", os.path.basename(PROJECT), "with", len(V1), "picture cuts,", round(t / FPS, 1), "s")


if __name__ == "__main__":
    main()
