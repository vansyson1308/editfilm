#!/usr/bin/env python3
"""Start Filmagic with its control channel for the screenshot scripts.

    eval "$(python3 docs/images/scripts/launch.py --scale 1.5)"   # prints the env to export
    python3 docs/images/scripts/launch.py --stop

On Linux without a display it starts Xvfb (a virtual X server) first, so the captures can run on
a headless machine with software Vulkan (Mesa lavapipe, package mesa-vulkan-drivers). `--scale`
is the window's pixels per point (WINIT_X11_SCALE_FACTOR), so `ui.screenshot` captures at that
density. The app gets a fresh data folder under target/demo-media/appdata, so no recent projects,
user presets or names from this machine appear in the window.
"""
import argparse
import os
import secrets
import shutil
import signal
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
import fmctl  # noqa: E402

STATE = os.path.join(fmctl.MEDIA, "appdata", "launch.pids")


def stop():
    """Stop the app and the Xvfb that start() launched."""
    if os.path.exists(STATE):
        for pid in reversed(open(STATE).read().split()):
            try:
                os.kill(int(pid), signal.SIGTERM)
            except (ProcessLookupError, ValueError):
                pass
        os.remove(STATE)
        time.sleep(1.5)


def start(port=9876, scale=1.5, display=":77"):
    """Start Filmagic (and Xvfb when there is no display); return (env, token)."""
    exe = os.path.join(fmctl.ROOT, "target", "release", "filmagic")
    if not os.path.exists(exe):
        raise SystemExit("build first: cargo build --release -p filmagic -p filmagic-cli")
    data = os.path.join(fmctl.MEDIA, "appdata")
    shutil.rmtree(data, ignore_errors=True)
    os.makedirs(data)
    env = dict(os.environ)
    pids = []
    if not env.get("DISPLAY") and sys.platform.startswith("linux"):
        xvfb = subprocess.Popen(["Xvfb", display, "-screen", "0", "5200x3000x24", "-nolisten", "tcp"],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        pids.append(xvfb.pid)
        env["DISPLAY"] = display
        time.sleep(1.5)
    token = secrets.token_hex(32)
    env["FILMAGIC_CONTROL_TOKEN"] = token
    env["FILMAGIC_CONTROL_PORT"] = str(port)
    env["WINIT_X11_SCALE_FACTOR"] = str(scale)
    env.pop("WAYLAND_DISPLAY", None)
    log = open(os.path.join(data, "stdout.log"), "wb")
    app = subprocess.Popen([exe, "--control", str(port), "--data-dir", data, "--empty", "--no-recover"],
                           env=env, stdout=log, stderr=subprocess.STDOUT)
    pids.append(app.pid)
    with open(STATE, "w") as f:
        f.write(" ".join(map(str, pids)))
    ctl = fmctl.Filmagic(port=port, token=token, timeout=30)
    for _ in range(120):
        try:
            ctl.call("ui.inspect")
            break
        except OSError:
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
    ap.add_argument("--display", default=":77", help="Xvfb display to start when $DISPLAY is unset")
    ap.add_argument("--stop", action="store_true", help="stop the app and Xvfb started earlier")
    a = ap.parse_args()
    if a.stop:
        stop()
        return 0
    env, token = start(a.port, a.scale, a.display)
    print(f"export DISPLAY={env['DISPLAY']} FILMAGIC_CONTROL_PORT={a.port} FILMAGIC_CONTROL_TOKEN={token}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
