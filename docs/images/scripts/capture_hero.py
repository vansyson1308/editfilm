#!/usr/bin/env python3
"""Capture docs/images/filmagic-hero.png from the Apollo 11 project (build_apollo.py).

Start the app at 2x first, so the window is 2400x1285 points = 4800x2570 pixels:

    eval "$(python3 docs/images/scripts/launch.py --scale 2)"
    python3 docs/images/scripts/capture_hero.py

The shot is taken 0.6 s after playback starts at 00:01:16:27, so the loudness meters are live,
and halved to 2400 px wide.
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
import fmctl  # noqa: E402
import build_apollo  # noqa: E402

OUT = os.path.join(fmctl.ROOT, "docs", "images", "filmagic-hero.png")
RAW = os.path.join(fmctl.MEDIA, "shots", "hero-2x.png")


def setup(c):
    c.call("ui.resize", {"width": 2400, "height": 1285})
    c.run("file.open", {"path": build_apollo.PROJECT})
    c.ui_set(workspace="Color")
    c.settle(1)
    c.call("ui.click", {"id": "panel.tab.EffectControls"})
    # Project panel: list view, the columns of the old shot, every bin twirled open
    c.run("project.view.set", {"view": "list"})
    c.run("project.columns.set", {"columns": ["Name", "Frame Rate", "Media Start", "Media Duration"]})
    c.settle(1)
    bins = [int(e["id"].split(".")[2]) for e in c.elements("project.bin.") if e["id"].endswith(".toggle")]
    for b in bins:
        if b not in c.inspect()["ui"]["expanded_bins"]:  # the twirl toggles: click closed ones only
            c.call("ui.click", {"id": f"project.bin.{b}.toggle"})
            c.settle(0.5)
    closed = sorted(set(bins) - set(c.inspect()["ui"]["expanded_bins"]))
    if len(bins) != 4 or closed:
        raise SystemExit(f"expected four open bins; bins {bins}, still closed {closed}")
    # the liftoff shot selected, so Effect Controls and Color Grading show its grade
    clips = c.run("sequence.clips", {"track": "V1", "fromSeconds": 77.0, "toSeconds": 77.1})
    found = [k["clip"] for t in clips["tracks"] for k in t["clips"]]
    if not found:
        raise SystemExit("no clip on V1 at 77 s: rebuild the project with build_apollo.py")
    hero = found[0]
    c.run("timeline.select", {"clips": [hero], "add": False, "toggle": False, "linked": False})
    c.run("timeline.decreaseVideoHeight")
    fmctl.widen_meters(c)
    c.ui_set(timeline={"fit": True}, playbackRes="Full", program={"res": "Full", "paused_res": "Full"})
    c.call("ui.move", {"id": "timeline.ruler"})  # the status bar shows the timeline's hint
    c.settle(2)


def main():
    c = fmctl.Filmagic()
    fmctl.check_scale(c, 2)
    setup(c)
    os.makedirs(os.path.dirname(RAW), exist_ok=True)
    c.run("playhead.set", {"timecode": "00:01:16:27"})
    c.settle(12)  # timeline thumbnails and waveforms
    c.call("ui.playback", {"action": "play"})
    time.sleep(0.6)
    c.screenshot(RAW)
    c.call("ui.playback", {"action": "stop"})
    fmctl.check_size(RAW, 4800, 2570)
    img = fmctl.png_read(RAW)
    fmctl.png_write(OUT, *fmctl.halve(img))
    print("wrote", os.path.relpath(OUT, fmctl.ROOT))


if __name__ == "__main__":
    main()
