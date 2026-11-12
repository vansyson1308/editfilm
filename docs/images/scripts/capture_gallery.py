#!/usr/bin/env python3
"""Capture the eight gallery screenshots from the trailer project (build_trailer.py).

Each shot starts a fresh app (launch.py) at the density it needs: the full-window shots at 1.5x
(1600x1000 points = 2400x1500 pixels), the panel crops at 2x.

    python3 docs/images/scripts/capture_gallery.py            # all eight
    python3 docs/images/scripts/capture_gallery.py export     # just one
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
import fmctl  # noqa: E402
import build_trailer  # noqa: E402
import launch  # noqa: E402

IMAGES = os.path.join(fmctl.ROOT, "docs", "images")
RAW = os.path.join(fmctl.MEDIA, "shots")
HEADER = 23  # points between a panel group's top edge and its content (the tab row)


class Shots:
    def __init__(self, scale):
        self.c = fmctl.Filmagic()
        fmctl.check_scale(self.c, scale)
        self.scale = scale
        self.size = None
        self.items = {}

    # --- helpers -----------------------------------------------------------------------------
    def open_project(self, width, height):
        c = self.c
        c.call("ui.resize", {"width": width, "height": height})
        self.size = (round(width * self.scale), round(height * self.scale))
        c.run("file.open", {"path": build_trailer.PROJECT})
        rows = c.run("project.items")["rows"]
        self.items = {r["name"]: r["item"] for r in rows if "item" in r}
        c.ui_set(playbackRes="Full", program={"res": "Full", "paused_res": "Full"},
                 source={"res": "Full", "paused_res": "Full"})

    def sequence(self, name):
        self.c.run("sequence.open", {"item": self.items[name]})
        self.c.ui_set(timeline={"fit": True})

    def clip_at(self, seconds, track="V1"):
        r = self.c.run("sequence.clips", {"track": track, "fromSeconds": seconds, "toSeconds": seconds + 0.01})
        clips = [k["clip"] for t in r["tracks"] for k in t["clips"]]
        if not clips:
            raise SystemExit(f"no clip on {track} at {seconds} s: rebuild the project with build_trailer.py")
        return clips[0]

    def select(self, clip):
        self.c.run("timeline.select", {"clips": [clip], "add": False, "toggle": False, "linked": False})

    def tab(self, panel):
        self.c.call("ui.click", {"id": f"panel.tab.{panel}"})

    def meters_live(self, at, seconds=1.5):
        """Play a moment so the loudness meters hold readings, then park the playhead at `at`."""
        c = self.c
        c.run("playhead.set", {"seconds": max(0.0, at - seconds)})
        c.call("ui.playback", {"action": "play"})
        time.sleep(seconds)
        c.call("ui.playback", {"action": "stop"})
        c.run("playhead.set", {"seconds": at})

    def fit_group(self, panel, size):
        """Resize the window until the tab group holding `panel` is `size` pixels (tab row included)."""
        c = self.c
        tw, th = size[0] / self.scale, size[1] / self.scale
        w, h = self.size[0] / self.scale, self.size[1] / self.scale
        for _ in range(6):
            r = self.rect(f"panel.{panel}")
            gw, gh = r[2], r[3] + HEADER
            if abs(gw - tw) < 0.3 and abs(gh - th) < 0.3:
                return
            # the group grows in proportion to the window, less the fixed panels and gutters
            w += (tw - gw) * w / gw
            h += (th - gh) * h / gh
            c.call("ui.resize", {"width": round(w * 2) / 2, "height": round(h * 2) / 2})
            self.size = (round(round(w * 2) / 2 * self.scale), round(round(h * 2) / 2 * self.scale))
            c.settle(1)
        raise SystemExit(f"could not size the {panel} panel to {size[0]}x{size[1]} px")

    def rect(self, element):
        for e in self.c.elements(element):
            if e["id"] == element:
                return e["rect"]
        raise SystemExit(f"no element {element}")

    def window(self, name, hover="timeline.ruler"):
        c = self.c
        if hover:
            fmctl.widen_meters(c)
            c.ui_set(timeline={"fit": True})  # after the workspace change, which re-zooms it
            c.call("ui.move", {"id": hover})  # the status bar shows the timeline's hint
        c.settle(8)  # thumbnails, waveforms and decoded frames
        path = os.path.join(IMAGES, f"filmagic-{name}.png")
        c.screenshot(path)
        fmctl.check_size(path, *self.size)
        fmctl.png_recompress(path)
        return path

    def crop(self, name, panels, size, play_from=None):
        """Crop `size` pixels from the top-left of the panel groups `panels` (tab rows included).

        With `play_from`, the window is captured 0.8 s after playback starts there (live meters)."""
        c = self.c
        if "AudioMeters" in panels:
            fmctl.widen_meters(c)
        c.ui_set(timeline={"fit": True})
        c.call("ui.move", {"id": "timeline.ruler"})
        c.settle(8)
        raw = os.path.join(RAW, f"{name}-window.png")
        os.makedirs(RAW, exist_ok=True)
        if play_from is not None:
            c.run("playhead.set", {"seconds": play_from})
            c.settle(2)
            c.call("ui.playback", {"action": "play"})
            time.sleep(0.8)
        c.screenshot(raw)
        if play_from is not None:
            c.call("ui.playback", {"action": "stop"})
        fmctl.check_size(raw, *self.size)
        rects = [self.rect(f"panel.{p}") for p in panels]
        x = min(r[0] for r in rects)
        y = min(r[1] for r in rects) - HEADER
        img = fmctl.png_read(raw)
        px = min(max(0, round(x * self.scale)), img[0] - size[0])  # a panel at the window edge
        py = min(max(0, round(y * self.scale)), img[1] - size[1])
        out = fmctl.crop(img, px, py, *size)
        path = os.path.join(IMAGES, f"filmagic-{name}.png")
        fmctl.png_write(path, *out)
        return path

    # --- shots at 1.5x -------------------------------------------------------------------------
    def assembly(self):
        c = self.c
        self.open_project(1600, 1000)
        self.sequence("Trailer")
        c.ui_set(workspace="Assembly")
        c.run("project.view.set", {"view": "icon"})
        c.run("source.open", {"item": self.items["Carnival of Souls (1962).mp4"]})
        c.run("markers.markIn", {"target": "source", "seconds": 405})
        c.run("markers.markOut", {"target": "source", "seconds": 413})
        c.run("source.setPlayhead", {"seconds": 410})
        self.meters_live(6.4)
        return self.window("assembly")

    def keyframes(self):
        c = self.c
        self.open_project(1600, 1000)
        self.sequence("Trailer")
        c.ui_set(workspace="Editing")
        c.run("project.view.set", {"view": "icon"})
        title = self.clip_at(1.0)
        self.select(title)
        self.tab("EffectControls")
        c.settle(1)
        c.call("ui.click", {"id": "effectControls.motion.scale.graphs"})
        self.meters_live(2.4)
        return self.window("keyframes")

    def export(self):
        c = self.c
        self.open_project(1600, 1000)
        self.sequence("Trailer")
        c.run("playhead.set", {"seconds": 15.5})
        c.ui_set(mode="export")
        c.settle(1)
        # a neutral folder: the Summary shows the full output path
        c.ui_set(export={"fileName": "Night of the Living Dead - Trailer", "location": "/media/exports"})
        return self.window("export", hover=None)

    def color(self):
        c = self.c
        self.open_project(1600, 1000)
        self.sequence("Charade - Grade")
        c.ui_set(workspace="Color", panels={"scopes": {"shown": ["waveform", "vectorscopeYuv"]}})
        c.run("project.view.set", {"view": "icon"})
        self.select(self.clip_at(1.0))
        self.meters_live(1.6)
        return self.window("color")

    # --- crops at 2x ---------------------------------------------------------------------------
    def timeline(self):
        c = self.c
        self.open_project(1650, 1072)  # Timeline + Audio Meters = 1100 points (widening the meters narrows the Timeline)
        self.sequence("Trailer")
        c.ui_set(workspace="Editing")
        self.select(self.clip_at(25.0))
        c.call("ui.click", {"id": "panel.tab.Timeline"})
        return self.crop("timeline", ["Timeline", "AudioMeters"], (2200, 849), play_from=24.7)

    def scopes(self):
        c = self.c
        self.open_project(1660, 1030)
        self.sequence("Charade - Grade")
        c.ui_set(workspace="Color", panels={"scopes": {"shown": ["waveform", "vectorscopeYuv"]}})
        c.run("playhead.set", {"seconds": 1.6})
        c.settle(1)
        self.fit_group("LumetriScopes", (1100, 1115))  # the panel alone, no neighbours
        return self.crop("scopes", ["LumetriScopes"], (1100, 1115))

    def color_panel(self):
        c = self.c
        self.open_project(1620, 1000)
        self.sequence("Charade - Grade")
        c.ui_set(workspace="Color")
        self.select(self.clip_at(1.0))
        c.settle(1)
        # open Color Wheels & Match (its Color Match controls exist only while it is open)
        if not c.elements("lumetri.match.reference"):
            c.call("ui.click", {"id": "lumetri.section.Color Wheels & Match"})
            c.settle(0.5)
        if not c.elements("lumetri.match.reference"):
            raise SystemExit("the Color Wheels & Match section did not open")
        return self.crop("color-panel", ["LumetriColor"], (680, 1564))

    def effects(self):
        c = self.c
        self.open_project(1200, 800)
        self.sequence("Earth Views - Look")
        c.ui_set(workspace="Effects")
        self.select(self.clip_at(6.0))
        c.run("playhead.set", {"seconds": 5.97})
        c.settle(1)
        # twirl Motion and Opacity up so Time Remapping and Color Grading fit in the crop
        for fx, first_param in [("motion", "effectControls.motion.0.position.name"),
                                ("opacity", "effectControls.opacity.1.opacity.name")]:
            if c.elements(first_param):
                c.call("ui.click", {"id": f"effectControls.effect.{fx}.twirl"})
                c.settle(0.5)
        bottom = self.rect("panel.EffectControls")[1] + 706 / self.scale - HEADER
        for row in ["effectControls.effect.motion", "effectControls.effect.opacity",
                    "effectControls.effect.time_remap", "effectControls.effect.lumetri"]:
            r = self.rect(row)
            if r[1] + r[3] > bottom:
                raise SystemExit(f"{row} is not inside the crop")
        return self.crop("effects", ["EffectControls", "Program", "Effects"], (2400, 706))


# shot: (method, pixels per point)
SHOTS = {"assembly": ("assembly", 1.5), "keyframes": ("keyframes", 1.5), "export": ("export", 1.5),
         "color": ("color", 1.5), "timeline": ("timeline", 2), "scopes": ("scopes", 2),
         "color-panel": ("color_panel", 2), "effects": ("effects", 2)}


def main():
    names = sys.argv[1:] or list(SHOTS)
    for n in names:
        method, scale = SHOTS[n]
        launch.stop()
        launch.start(scale=scale)  # a fresh app for every shot
        try:
            print("wrote", os.path.relpath(getattr(Shots(scale), method)(), fmctl.ROOT), flush=True)
        finally:
            launch.stop()


if __name__ == "__main__":
    main()
