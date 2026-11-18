# Highlights and speed ramps

Two commands for sports and event footage (#21):

- `story.highlights` finds the best moments of long footage from what it sounds and looks like.
- `clip.speedRamp` makes a smooth speed change, such as 100 % easing down to 40 %, in one command.

Both work only from the frames and sound that exist: neither generates picture content.

## `story.highlights`: find the best moments

Scene detection finds nothing in single-camera footage, because a whole match is one shot. This
command scores a timeline clip (`clip`, with its linked sound) or a project item's media (`item`)
over time from four cues, and returns ranked segments.

```json
{"command": "story.highlights", "params": {"item": 12, "picture": false, "fromSeconds": 0, "toSeconds": 600, "wait": true}}
{"command": "story.highlights", "params": {"clip": 40, "mode": "rally", "cadence": [0.4, 2.0], "markers": true}}
{"command": "story.highlights", "params": {"job": 7}}
```

| Param | Default | Meaning |
|---|---|---|
| `clip` or `item` | | a timeline clip (sequence time), or a project item's media (media time) |
| `fromSeconds`, `toSeconds` | the whole clip or item | the span to analyse, at most 7200 s; call again for the rest |
| `mode` | `auto` | `rally` (runs of impacts only), `peaks` (cheering and bursts of motion only), `auto` (both) |
| `cadence` | `[0.3, 2.5]` | seconds between two impacts of a rally |
| `minHits` | 3 | impacts a rally has at least |
| `minSeconds`, `maxSeconds` | 2, 45 | segment length |
| `limit` | 10 | segments listed (at most 100) |
| `picture` | true | read the picture (motion, scoreboard); `false` reads only the sound and is much faster |
| `fps` | 4 | frames sampled a second (1–15) |
| `markers` | false | a marker for each listed segment: sequence markers for a clip, clip markers on the media for an item (one undo step) |
| `wait` | false | answer when the analysis is done |
| `job` | | read the result of a call that did not wait |

The analysis runs as a job. Without `wait` the reply holds the `job` id at once. `jobs.list` shows
its progress, `jobs.cancel` stops it without changing anything, and
`story.highlights {"job": id}` answers `running` until it can give the result. The results of the
last 16 calls are kept.

Over MCP (headless or through the app), `wait:true` does not hold the session: the server runs
the call as a job, polls it, reports its progress and stops it when the request is cancelled, then
answers with the result, as it does for long exports. Called directly on the engine, `wait:true`
blocks until the analysis ends, which for two hours of video with the picture read takes minutes:
there, start the job without `wait` and read it with `{"job": id}`.

A job whose project is closed or replaced before its result is read keeps nothing (its markers are
not written), and so does one cancelled after its analysis finished.

Each segment has:

- `rank` and `score` (0–1);
- `kind`: `rally` or `peak`;
- `start` and `end` (`seconds`, `ticks`, `frame`), and for a clip also `media` start and end;
- `suggestedIn` and `suggestedOut`. These start before the first impact and end after the last,
  and after the cheering that follows. They sit at the quietest picture within 1.5 s, and never
  inside another rally, so the cut does not land mid-motion. One that would cut a sentence moves
  to where the speech ends or begins (up to 3 s);
- `cues`: those that fired, from `impacts`, `rally`, `cheer`, `motion`, `scoreboard (heuristic)`
  and `speech`;
- `impacts`, `cadenceSeconds`, `cheerDb` (the rise over the surrounding level), `motion` (0–1)
  and `scoreboardChange`.

The result also lists every impact heard (`impactTimes`, at most 2000; counting paddle hits needs
no other tool), the stretches of `cheers` and of `speech`, and the `scoreboardChanges`.

### Cost

- Sound is read decimated to 16 kHz.
- A few frames a second are sampled, at 256 pixels wide.
- A call analyses at most two hours.
- The cues of the last 8 analyses are cached, keyed on the media (path, size, modification time),
  the clip and the span. Asking again with another `mode`, `cadence`, `limit` or `markers` reads no
  media (`"cached": true`).

Reading the picture means decoding the video, which is the slow part on long footage. Use
`picture:false` for a first pass that only listens.

### What each cue can and cannot detect

These are measurements, not judgement. None of them knows what it hears or sees.

| Cue | Method | Detects | Does not |
|---|---|---|---|
| Impacts | Onsets of the log-compressed spectral flux, picked above a moving mean plus 1.5 moving deviations (Bello et al. 2005; Dixon 2006). An impact must also rise 6 dB over the moment before and fall 6 dB within 80 ms. | Short percussive sounds: a paddle or racket hit, a ball bounce, a clap, a footstep on a hard court | Tell a paddle from a door slam, a dropped ball or a clap. Hear hits under loud music or a continuous roar. Count a voice, a held note or a sound that stops |
| Rally | A run of at least `minHits` impacts whose gaps all lie within `cadence` | Racket-sport rallies at a steady exchange | Tell two players' hits from one person bouncing a ball. A rally with a long lob breaks in two unless `cadence` is widened |
| Cheering | A sustained (≥ 0.6 s) rise of the median level 6 dB or more over the 20th percentile of the surrounding 20 s, with a high spectral flatness (noise-like sound) | Applause, a crowd cheering, a shout of a group | Tell a cheer from wind, rain, traffic or a crowd talking loudly. Hear a quiet crowd, or one that never pauses |
| Motion | Mean absolute luma difference of successive sampled frames on a 128×72 grid, scaled to the footage's busiest 5 % | How much of the picture changes: players running, a burst of action | See what moves. A pan, a zoom or a lighting change reads as motion; a small fast ball barely does |
| Speech | The speech detector of `audio.speechSegments` (voicing and syllabic level modulation, no model) | Someone talking: a player's call, a commentator. A suggested In or Out inside a sentence moves to its edge, up to 3 s | Tell who speaks or what is said. A sung melody can read as speech |
| Scoreboard (heuristic) | A 16×9 cell of the frame that holds still with fine detail for most of the footage, then changes for a moment while most other such cells do not | A burned-in score or clock that changes between points | Read digits. Find a scoreboard that moves, fades, or sits on moving video. An animated logo reads as a change |

## `clip.speedRamp`: one command for a speed ramp

```json
{"command": "clip.speedRamp", "params": {"clip": 40, "from": 100, "to": 40, "at": {"seconds": 12.0}, "duration": {"seconds": 0.5}, "ripple": true}}
{"command": "clip.speedRamp", "params": {"clip": 40, "segments": [{"at": {"seconds": 12}, "speed": 100}, {"at": {"seconds": 12.5}, "speed": 30}, {"at": {"seconds": 15}, "speed": 30}, {"at": {"seconds": 15.5}, "speed": 100}], "ripple": true}}
```

| Param | Default | Meaning |
|---|---|---|
| `clip` | the selected video clip | the picture clip; its linked sound follows (see `audio`) |
| `segments` | | points `{"at", "speed", "ease"}`: at sequence time `at` the clip plays at `speed` per cent (0 holds the frame), eased from the point before. Before the first point the clip plays at the first point's speed, after the last at the last one's. At most 64 points, in time order, within the clip |
| `from`, `to`, `at`, `duration` | the clip's speed, —, the clip's start, 1 s | the same as two points: `from` at `at`, `to` at `at + duration` |
| `ease` | `inOut` | `linear`, `in` (leaves the old speed gently), `out` (arrives at the new speed gently), `inOut` (both), `hold` (jumps at `at`) |
| `ripple` | false | let the clip's end move so it still shows all of its media. A slowed clip gets longer, and later clips move (sync-locked tracks too) |
| `audio` | `mute` | linked sound: `mute` switches it off (Clip ▸ Enable), `keep` leaves it playing at its own speed |
| `clear` | false | remove the ramp |

Speeds are per cent of normal speed, whatever the clip's Speed/Duration was. Times take ticks or
`{"seconds"|"frame"|"timecode"}`, in sequence time.

The result reports:

- the new `duration`, `previousDuration` and `end`;
- `rippled`, and `laterClipsMoved` in seconds;
- the `media` the clip now shows;
- what happened to the linked `audio`;
- `warnings`, `notes` and `pictureChange`.

Without `ripple`, a slowed clip keeps its length. It no longer reaches the end of its media, and a
warning says how much no longer shows. A sped-up clip needs media it may not have: it is refused,
and the error suggests `ripple:true`. With `ripple`, a ramp that ends at 0 % is refused, because
the clip would never end.

### How it works: Time Remapping

The ramp is Time Remapping keyframes on the clip's Speed (the intrinsic `time_remap` effect). The
renderer plays them: the media time shown at each moment is the integral of the speed curve. On
linear and hold segments this is exact (closed form). On eased ones it uses Simpson's rule, with
an error of the order of 1e-7 of the segment's length (about 60 ns on a 1 s eased segment). So the
speed glides instead of jumping between razored pieces.

**Which clock keyframes use.** Every clip keyframe, Time Remapping's own Speed included, is timed
and evaluated in the clip's *linear* media time: its media time at its constant speed, as if there
were no ramp (`effect_time_at`). Time Remapping changes which frame is shown, not when a keyframe
happens: Motion, Opacity, effect and mask keyframes stay where they are on the timeline, and keep
animating through a freeze. A keyframe set at a time (`effects.setParam`, `effects.addKeyframe`,
Effect Controls) lands at that time's linear media time. Graphics and adjustment layers have no frames to
ramp: `clip.speedRamp` refuses them.

The curve is anchored at its first keyframe in play order, which `clip.speedRamp` puts on
the clip's first frame, so that frame stays where it was. Razoring the clip or trimming its head
keeps every remaining frame where it was, on both pieces.

- **Reverse.** A reversed clip ramps from its first frame, backwards through the media. When a
  slowed reversed clip ripples, its stored `sourceIn` (the linear clock the keyframes are timed
  in) can fall below 0. The media it shows still starts at 0 or later.
- **Trims.** Trimming a ramped clip's tail extends it at the ramp's last speed. Media limits are
  checked at the clip's own speed, so a trim may reach past the media's end, which then holds its
  last frame. Run `clip.speedRamp` again to set the ramp for the new length.
- **Frame hold.** A frame-hold clip shows one frame, so it is refused. A point at 0 % holds the
  frame inside a ramp.
- **Sound.** Sound does not follow a varying speed, so linked sound is switched off by default,
  and its length follows the picture's with `ripple`. `audio:"keep"` keeps it on, and it drifts
  from the picture after the ramp starts.
- **Other keyframes.** The clip's other keyframes (opacity, Motion…) stay on the timeline (see
  *Which clock keyframes use*).
- **Ramping again.** A second `clip.speedRamp` replaces the first, and the reply says so. A clip
  that already had a ramp (a razored right piece) keeps the frame it starts on; `clear:true` does
  too. `clear:true` on a clip without a ramp changes nothing and adds no undo step.
- **Limits.** A ramp's speeds are a factor of the clip's own speed, at most 1000×: a point above
  that is refused (raise the clip's speed with `clip.speedDuration` first). Keys that all say
  100 % are no ramp.
- **Mask tracking.** Ramp first, then track. On a ramped clip `masks.track` steps through the
  sequence's frames, follows the frame each one shows, and keys it in the linear clock, so the
  mask stays on its subject through the ramp. Keys that followed the subject before a ramp (a
  tracked mask, an auto-reframe's Motion keys) no longer line up with the frames shown after
  it: `clip.speedRamp` warns, and they need tracking or reframing again.
- **Sound clips.** Time Remapping is a picture effect. A sound clip that carries a ramp (only a
  hand-edited project makes one) is silent.
- **Speed/Duration.** Changing a ramped clip's speed with `clip.speedDuration` scales the ramp
  with it.
- **Constant remap speeds.** A Time Remapping speed that is not keyframed changes nothing. A
  constant speed is the clip's Speed, which is what every importer turns a constant remap into.
- **Export to other editors.** Speed ramps are not written to EDL or FCP7 XML; the FCP7 export
  warns about this.
