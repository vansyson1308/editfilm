# Filmagic's bridge to VieNeu-TTS (https://github.com/pnnbao97/VieNeu-TTS, Apache-2.0).
#
# VieNeu-TTS is an optional external engine: Filmagic never links, bundles or ships it, and the
# owner asked for it to be integrated through a Python bridge (issue #20). `crates/tts/src/
# vieneu.rs` writes this file to Filmagic's engine directory and runs it as
# `python -E -s -X utf8 vieneu_bridge.py <op>` with an interpreter that has `vieneu` installed,
# sends a JSON request on stdin, and reads one line from stdout that starts with
# "FILMAGIC-REPLY ". Everything else (pause markers, plausible lengths, pace and pitch, silence,
# WAV files, the project) is done in Rust. This file calls only `vieneu`'s documented methods:
# `Vieneu()`, `list_preset_voices()`, `get_preset_voice()` and `infer()`. Downloads go where
# Filmagic's `HF_HOME` points (its models directory).
#
# ops:
#   probe   -> {"ok", "python", "executable", "vieneu": version or null}   (does not import vieneu)
#   voices  -> {"ok", "sampleRate", "voices": [...]}                         (loads the model: may download)
#   synth   -> {"ok", "sampleRate", "segments": [{"samples", "takes"}]}; segment i is written as
#              raw little-endian float32 mono to <out>/seg-<i>.f32
# On failure: {"ok": false, "kind": "...", "error": "..."} and exit status 1.

import json
import os
import sys
import unicodedata

# Libraries print progress to stdout; only the reply goes there, everything else to stderr.
REPLY = sys.stdout
sys.stdout = sys.stderr


def reply(obj, status=0):
    REPLY.write("FILMAGIC-REPLY " + json.dumps(obj) + "\n")  # ASCII only (ensure_ascii)
    REPLY.flush()
    sys.exit(status)


def fail(kind, message, **extra):
    reply(dict(ok=False, kind=kind, error=str(message)[:2000], **extra), 1)


def nfc(s):
    return unicodedata.normalize("NFC", s) if isinstance(s, str) else s


def probe():
    import importlib.metadata
    import importlib.util

    version = None
    if importlib.util.find_spec("vieneu") is not None:
        try:
            version = importlib.metadata.version("vieneu")
        except importlib.metadata.PackageNotFoundError:
            version = None
    reply(dict(ok=True, python="%d.%d.%d" % sys.version_info[:3], executable=sys.executable, vieneu=version))


def load(req):
    try:
        from vieneu import Vieneu
    except ImportError as e:
        fail("missing", "the Python package `vieneu` is not installed for %s (%s)" % (sys.executable, e))
    try:
        return Vieneu(mode=req.get("mode") or "v3turbo")
    except Exception as e:
        fail("model", "VieNeu-TTS could not load its model: %s" % e)


def preset_names(tts):
    return [nfc(name) for _, name in tts.list_preset_voices()]


def voices(tts):
    out = []
    for label, name in tts.list_preset_voices():
        meta = {}
        try:
            meta = tts.get_preset_voice(name) or {}
        except Exception:
            pass
        out.append(dict(name=nfc(name), label=label, gender=meta.get("gender"), description=meta.get("description"), region=meta.get("region")))
    return out


def synth(req):
    import numpy as np

    tts = load(req)
    rate = int(tts.sample_rate)
    voice, reference = nfc(req.get("voice")), req.get("reference")
    kwargs = {}
    if reference:
        kwargs["ref_audio"] = reference
    else:
        names = preset_names(tts)
        if voice not in names:
            fail("unknownVoice", "unknown VieNeu voice %r" % voice, voices=names)
        kwargs["voice"] = tts.get_preset_voice(voice)
    takes = max(1, min(int(req.get("takes") or 3), 5))
    report = []
    for i, seg in enumerate(req["segments"]):
        lo, hi, want = float(seg["minSeconds"]), float(seg["maxSeconds"]), float(seg["expectedSeconds"])
        best, best_err, n = None, None, 0
        # a generative voice now and then babbles or swallows words: take again, keep the take
        # whose length is closest to what the text needs
        for n in range(1, takes + 1):
            try:
                wav = np.asarray(tts.infer(nfc(seg["text"]), **kwargs), dtype="<f4").reshape(-1)
            except Exception as e:
                fail("synthesis", "VieNeu-TTS failed on segment %d: %s" % (i + 1, e))
            seconds = len(wav) / rate
            err = abs(np.log(max(seconds, 1e-3) / max(want, 1e-3)))
            if best is None or err < best_err:
                best, best_err = wav, err
            if lo <= seconds <= hi:
                break
        best.tofile(os.path.join(req["out"], "seg-%d.f32" % i))
        report.append(dict(samples=int(len(best)), takes=n))
    reply(dict(ok=True, sampleRate=rate, segments=report))


def main():
    op = sys.argv[1] if len(sys.argv) > 1 else ""
    if op == "probe":
        probe()
    try:
        req = json.loads(sys.stdin.buffer.read().decode("utf-8"))
    except Exception as e:
        fail("request", "bad request: %s" % e)
    if op == "voices":
        tts = load(req)
        reply(dict(ok=True, sampleRate=int(tts.sample_rate), voices=voices(tts)))
    if op == "synth":
        synth(req)
    fail("request", "unknown op %r" % op)


main()
