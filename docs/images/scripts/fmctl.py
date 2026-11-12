"""A small client for Filmagic's control channel, and the PNG helpers the capture scripts use.

Standard library only. The app is started with `filmagic --control <port>` and the token in
FILMAGIC_CONTROL_TOKEN (docs/control-protocol.md); these scripts read the same two variables:

    FILMAGIC_CONTROL_PORT   (default 9876)
    FILMAGIC_CONTROL_TOKEN  (required: the 64 hex characters the app was started with)
"""
import json
import os
import socket
import struct
import sys
import time
import zlib

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
MEDIA = os.path.join(ROOT, "target", "demo-media")

TICKS = 254_016_000_000  # filmagic_time::Tick per second


class ControlError(RuntimeError):
    pass


class Filmagic:
    def __init__(self, port=None, token=None, timeout=300):
        self.port = int(port or os.environ.get("FILMAGIC_CONTROL_PORT") or 9876)
        self.token = token or os.environ.get("FILMAGIC_CONTROL_TOKEN")
        if not self.token:
            raise SystemExit("set FILMAGIC_CONTROL_TOKEN to the token the app was started with")
        self.timeout = timeout
        self.sock = None
        self.buf = b""
        self.next_id = 1
        self.verbose = bool(os.environ.get("FMCTL_VERBOSE"))

    def _connect(self):
        self.sock = socket.create_connection(("127.0.0.1", self.port), timeout=self.timeout)
        self.buf = b""

    def call(self, method, params=None, check=True):
        req = {"id": self.next_id, "token": self.token, "method": method}
        if params is not None:
            req["params"] = params
        self.next_id += 1
        line = (json.dumps(req) + "\n").encode()
        for attempt in range(2):
            try:
                if self.sock is None:
                    self._connect()
                self.sock.sendall(line)
                reply = self._read_line()
                break
            except (OSError, ConnectionError):
                self.sock = None
                if attempt:
                    raise
        msg = json.loads(reply)
        if self.verbose:
            print(method, json.dumps(params)[:200], "->", json.dumps(msg)[:300], file=sys.stderr)
        if check and not msg.get("ok"):
            raise ControlError(f"{method} {json.dumps(params)[:400]}: {msg.get('error')}")
        return msg.get("result") if msg.get("ok") else msg

    def _read_line(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise ConnectionError("connection closed")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return line

    # --- conveniences -------------------------------------------------------------------
    def run(self, command, params=None, check=True):
        """Run an engine or UI command by id (`engine.execute`)."""
        return self.call("engine.execute", {"command": command, "params": params or {}}, check=check)

    def ui_set(self, **fields):
        return self.call("ui.set", fields)

    def inspect(self):
        return self.call("ui.inspect")

    def elements(self, prefix=""):
        r = self.call("ui.elements", {"prefix": prefix})
        return r.get("elements", []) if isinstance(r, dict) else r

    def screenshot(self, path, panel=None, element=None):
        p = {"path": os.path.abspath(path)}
        if panel:
            p["panel"] = panel
        if element:
            p["id"] = element
        return self.call("ui.screenshot", p)

    def settle(self, seconds=1.0):
        """Let the app draw a few frames (thumbnails, waveforms, decoded pictures)."""
        time.sleep(seconds)
        self.call("ui.inspect")

    def wait_jobs(self, limit=600):
        """Wait until background jobs (render, conform, waveforms) are finished."""
        t0 = time.time()
        while time.time() - t0 < limit:
            jobs = self.run("jobs.list", check=False)
            items = jobs.get("jobs", jobs) if isinstance(jobs, dict) else jobs
            if not items or all(j.get("finished") for j in items if isinstance(j, dict)):
                return
            time.sleep(1)


def secs(s):
    """A time parameter in seconds."""
    return {"seconds": s}


def tc(t):
    return {"timecode": t}


# --- PNG (8-bit RGB / RGBA, non-interlaced) ----------------------------------------------
def png_read(path):
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path}: not a PNG")
    pos, idat, w = 8, [], None
    while pos < len(data):
        n, kind = struct.unpack(">I4s", data[pos : pos + 8])
        body = data[pos + 8 : pos + 8 + n]
        pos += 12 + n
        if kind == b"IHDR":
            w, h, depth, ctype, _, _, interlace = struct.unpack(">IIBBBBB", body)
            if depth != 8 or ctype not in (2, 6) or interlace:
                raise ValueError(f"{path}: unsupported PNG format")
            bpp = 3 if ctype == 2 else 4
        elif kind == b"IDAT":
            idat.append(body)
        elif kind == b"IEND":
            break
    raw = zlib.decompress(b"".join(idat))
    stride = w * bpp
    rows, prev = [], bytearray(stride)
    i = 0
    for _ in range(h):
        f = raw[i]
        line = bytearray(raw[i + 1 : i + 1 + stride])
        i += 1 + stride
        if f == 1:
            for x in range(bpp, stride):
                line[x] = (line[x] + line[x - bpp]) & 255
        elif f == 2:
            for x in range(stride):
                line[x] = (line[x] + prev[x]) & 255
        elif f == 3:
            for x in range(stride):
                a = line[x - bpp] if x >= bpp else 0
                line[x] = (line[x] + ((a + prev[x]) >> 1)) & 255
        elif f == 4:
            for x in range(stride):
                a = line[x - bpp] if x >= bpp else 0
                b = prev[x]
                c = prev[x - bpp] if x >= bpp else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[x] = (line[x] + pr) & 255
        rows.append(line)
        prev = line
    if bpp == 4:  # drop alpha: the window is opaque
        rows = [bytearray(b for k, b in enumerate(r) if k % 4 != 3) for r in rows]
    return w, h, rows


def _filtered(rows):
    """Each row with the PNG filter (None, Sub or Up) that leaves the smallest residuals."""
    out, prev = [], bytes(len(rows[0])) if rows else b""
    for r in rows:
        r = bytes(r)
        sub = bytes((r[i] - (r[i - 3] if i >= 3 else 0)) & 255 for i in range(len(r)))
        up = bytes((a - b) & 255 for a, b in zip(r, prev))
        best = min(((0, r), (1, sub), (2, up)), key=lambda c: sum(min(x, 256 - x) for x in c[1]))
        out.append(bytes([best[0]]) + best[1])
        prev = r
    return b"".join(out)


def png_write(path, w, h, rows):
    raw = _filtered(rows)

    def chunk(kind, body):
        c = struct.pack(">I", len(body)) + kind + body
        return c + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)

    out = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    out += chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(out)


def crop(img, x, y, w, h):
    iw, ih, rows = img
    if x < 0 or y < 0 or x + w > iw or y + h > ih:
        raise ValueError(f"crop {x},{y} {w}x{h} outside {iw}x{ih}")
    return w, h, [r[x * 3 : (x + w) * 3] for r in rows[y : y + h]]


def halve(img):
    """Downscale by exactly 2 (2x2 box filter): a 2x capture to 1x size."""
    w, h, rows = img
    w2, h2 = w // 2, h // 2
    out = []
    for y in range(h2):
        a, b = rows[2 * y], rows[2 * y + 1]
        line = bytearray(w2 * 3)
        for x in range(w2):
            i = 6 * x
            for c in range(3):
                line[3 * x + c] = (a[i + c] + a[i + 3 + c] + b[i + c] + b[i + 3 + c] + 2) >> 2
        out.append(line)
    return w2, h2, out


def png_recompress(path):
    """Rewrite a PNG losslessly with row filters and zlib level 9 (ui.screenshot stores it raw)."""
    png_write(path, *png_read(path))
