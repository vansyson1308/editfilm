#!/usr/bin/env python3
"""Start Filmagic with its control channel for the screenshot scripts (Linux, X11 or Xvfb).

    eval "$(python3 docs/images/scripts/launch.py --scale 1.5)"   # prints the env to export
    eval "$(python3 docs/images/scripts/launch.py --stop)"        # stops it and unsets that env

When there is no X display (DISPLAY unset, or naming a display that is not running) it starts
Xvfb, a virtual X server, so the captures run on a headless machine with software Vulkan (Mesa
lavapipe, package mesa-vulkan-drivers). `--scale` is the window's pixels per point
(WINIT_X11_SCALE_FACTOR, which only X11 honours), so `ui.screenshot` captures at that density.
The capture scripts check the app reports that scale and that each capture has the expected size.
The app gets a fresh data folder under target/demo-media/appdata, so no recent projects, user
presets or names from this machine appear in the window.
"""
import argparse
import json
import os
import secrets
import shutil
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
import fmctl  # noqa: E402

STATE = os.path.join(fmctl.MEDIA, "launch.json")  # outside appdata, which start() empties


def _display_alive(display):
    """Is a local X display (":77") running? Checked through its socket."""
    if not display or not display.startswith(":"):
        return bool(display)  # a remote display: trust it
    return os.path.exists(f"/tmp/.X11-unix/X{display[1:].split('.')[0]}")


def stop():
    """Stop the app and the Xvfb that start() launched; return the shell lines that undo its env."""
    lines = ["unset FILMAGIC_CONTROL_TOKEN FILMAGIC_CONTROL_PORT"]
    if os.path.exists(STATE):
        state = json.load(open(STATE))
        for pid in [state.get("app"), state.get("xvfb")]:
            if pid:
                try:
                    os.kill(pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
        if state.get("xvfb"):
            lines.append("unset DISPLAY")
        os.remove(STATE)
        time.sleep(1.5)
    return lines


def start(port=9876, scale=1.5, display=":77"):
    """Start Filmagic (and Xvfb when no X display is running); return (env, token)."""
    if not sys.platform.startswith("linux"):
        raise SystemExit("these scripts set the scale through X11 (WINIT_X11_SCALE_FACTOR): run them on Linux")
    exe = os.path.join(fmctl.ROOT, "target", "release", "filmagic")
    if not os.path.exists(exe):
        raise SystemExit("build first: cargo build --release -p filmagic -p filmagic-cli")
    if os.path.exists(STATE):
        stop()
    data = os.path.join(fmctl.MEDIA, "appdata")
    shutil.rmtree(data, ignore_errors=True)
    os.makedirs(data)
    env = dict(os.environ)
    state = {"app": None, "xvfb": None}
    if not _display_alive(env.get("DISPLAY")):
        xvfb = subprocess.Popen(["Xvfb", display, "-screen", "0", "5200x3000x24", "-nolisten", "tcp"],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        state["xvfb"] = xvfb.pid
        env["DISPLAY"] = display
        for _ in range(50):
            if _display_alive(display):
                break
            if xvfb.poll() is not None:
                raise SystemExit(f"Xvfb {display} exited ({xvfb.returncode})")
            time.sleep(0.1)
    token = secrets.token_hex(32)
    env["FILMAGIC_CONTROL_TOKEN"] = token
    env["FILMAGIC_CONTROL_PORT"] = str(port)
    env["WINIT_X11_SCALE_FACTOR"] = str(scale)
    env.pop("WAYLAND_DISPLAY", None)
    log = open(os.path.join(data, "stdout.log"), "wb")
    app = subprocess.Popen([exe, "--control", str(port), "--data-dir", data, "--empty", "--no-recover"],
                           env=env, stdout=log, stderr=subprocess.STDOUT)
    state["app"] = app.pid
    with open(STATE, "w") as f:
        json.dump(state, f)
    ctl = fmctl.Filmagic(port=port, token=token, timeout=30)
    for _ in range(240):
        try:
            ctl.call("ui.inspect")
            break
        except (OSError, fmctl.ControlError):  # not listening yet, or "still starting"
            if app.poll() is not None:
                raise SystemExit(f"filmagic exited ({app.returncode}); see {log.name}")
            time.sleep(0.5)
    else:
        raise SystemExit("the control channel did not answer")
    os.environ.update({"FILMAGIC_CONTROL_TOKEN": token, "FILMAGIC_CONTROL_PORT": str(port)})
    # for scripts run by hand against this app: `. target/demo-media/appdata/control.env`
    fd = os.open(os.path.join(data, "control.env"), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(f"export DISPLAY={env['DISPLAY']} FILMAGIC_CONTROL_PORT={port} FILMAGIC_CONTROL_TOKEN={token}\n")
    return env, token


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=9876)
    ap.add_argument("--scale", type=float, default=1.5, help="pixels per point (default 1.5)")
    ap.add_argument("--display", default=":77", help="Xvfb display to start when no X display is running")
    ap.add_argument("--stop", action="store_true", help="stop the app and Xvfb started earlier")
    a = ap.parse_args()
    if a.stop:
        print("\n".join(stop()))
        return 0
    env, token = start(a.port, a.scale, a.display)
    print(f"export DISPLAY={env['DISPLAY']} FILMAGIC_CONTROL_PORT={a.port} FILMAGIC_CONTROL_TOKEN={token}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
