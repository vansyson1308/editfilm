# Filmagic's bridge to VieNeu-TTS (https://github.com/pnnbao97/VieNeu-TTS, Apache-2.0).
#
# VieNeu-TTS is an optional external engine, used the way ffmpeg is: Filmagic never links,
# bundles or installs it by itself. `crates/tts/src/vieneu.rs` writes this file to
# `<models>/vieneu/bridge/` and runs it with a Python interpreter that has `vieneu` installed
# (`python vieneu_bridge.py <op>`), sends a JSON request on
# stdin and reads one JSON line from stdout. Everything else (pause markers, plausible lengths,
# pace and pitch, silence, WAV files, the project) is done in Rust; this file only calls the
# package's public API: `Vieneu()`, `list_preset_voices()`, `get_preset_voice()` and `infer()`.
#
# ops:
#   probe   -> {"ok", "python", "vieneu": version or null}          (does not import vieneu)
#   voices  -> {"ok", "sampleRate", "voices": [...]}                  (loads the model: may download)
#   synth   -> {"ok", "sampleRate", "segments": [{"samples", "takes"}]}; segment i is written as
#              raw little-endian float32 mono to <out>/seg-<i>.f32
# On failure: {"ok": false, "kind": "...", "error": "..."} and exit status 1.

import json
import os
import pathlib
import sys


def reply(obj, status=0):
    sys.stdout.write(json.dumps(obj) + "\n")  # ASCII only (ensure_ascii), safe on any console
    sys.stdout.flush()
    sys.exit(status)


def fail(kind, message, **extra):
    reply(dict(ok=False, kind=kind, error=str(message)[:2000], **extra), 1)


def probe():
    import importlib.metadata
    import importlib.util

    version = None
    if importlib.util.find_spec("vieneu") is not None:
        try:
            version = importlib.metadata.version("vieneu")
        except importlib.metadata.PackageNotFoundError:
            version = "unknown"
    reply(dict(ok=True, python="%d.%d.%d" % sys.version_info[:3], vieneu=version))


def load(req):
    try:
        from vieneu import Vieneu
    except ImportError as e:
        fail("missing", "the Python package `vieneu` is not installed for %s (%s)" % (sys.executable, e))
    cache = req.get("cache")
    if cache:
        # Download into real files under Filmagic's models directory rather than Hugging Face
        # cache symlinks: onnxruntime refuses external weights that resolve outside the model's
        # folder. Only when this version of vieneu has the hook; otherwise HF_HOME (set by
        # Filmagic) still keeps the downloads in the models directory.
        try:
            from vieneu._v3_turbo_engine import onnx_runtime_lite as orl

            def fetch(repo, files, subfolder):
                from huggingface_hub import hf_hub_download

                last = None
                for fn in files:
                    try:
                        last = hf_hub_download(repo, fn, subfolder=subfolder or None, local_dir=os.path.join(cache, repo.replace("/", "--")))
                    except Exception:
                        if fn.endswith(".json"):
                            continue
                        raise
                return pathlib.Path(last).parent

            if hasattr(getattr(orl, "OnnxV3LiteEngine", None), "_fetch"):
                orl.OnnxV3LiteEngine._fetch = staticmethod(fetch)
        except ImportError:
            pass
    try:
        return Vieneu(mode=req.get("mode") or "v3turbo")
    except Exception as e:
        fail("model", "VieNeu-TTS could not load its model: %s" % e)


def voices(tts):
    out = []
    for label, name in tts.list_preset_voices():
        meta = {}
        try:
            meta = tts.get_preset_voice(name) or {}
        except Exception:
            pass
        out.append(dict(name=name, label=label, gender=meta.get("gender"), description=meta.get("description"), region=meta.get("region")))
    return out


def synth(req):
    import numpy as np

    tts = load(req)
    rate = int(tts.sample_rate)
    voice, reference = req.get("voice"), req.get("reference")
    kwargs = {}
    if reference:
        kwargs["ref_audio"] = reference
    else:
        if tts.resolve_voice_name(voice) is None:
            fail("unknownVoice", "unknown VieNeu voice %r" % voice, voices=[v["name"] for v in voices(tts)])
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
                wav = np.asarray(tts.infer(seg["text"], **kwargs), dtype="<f4").reshape(-1)
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
