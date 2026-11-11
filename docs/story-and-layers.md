# Story tools and layers

Commands for judging an edit and for working on part of a picture. They are engine commands like
every other: run them from the CLI (`filmcraft-cli exec story.rhythm`), the control channel, or
MCP (`command_run`). The MCP server also has a `story_board` tool that returns the board as an
image. None of them has a menu entry or a panel yet.

Two rules hold for all of them.

- **They measure and assemble; they do not decide and do not invent.** A cut is judged first on
  what the audience should feel and on whether the story moves. Nothing computes that, so the
  commands report what can be measured and say what is left to the editor. No command generates
  picture content: every pixel comes from frames that exist.
- **One call, one undo step.** A command that edits either completes or leaves the project, its
  undo history and its redo history as they were.

## Story tools

The ideas follow Walter Murch's *In the Blink of an Eye*: the six criteria of a cut and their
order, the blink as the mark of a completed thought, cut rates set against blink rates, the
representative frame of a shot, and trying versions against each other.

A **shot** is read from the picture: at each moment, the topmost enabled clip of footage (media,
subclips, nested sequences). Titles, graphics and adjustment layers ride on a shot and do not start
one. A **cut** is where that clip changes. Black between clips counts as a shot, except at the
head and tail.

### `story.rhythm`

`{"window":seconds=10,"flashFrames":n=8}`. Read-only.

Cut rate (`cutsPerMinute`), shot lengths (mean, median, shortest, longest, and `variation`, the
standard deviation over the mean) and a `pace`:

| Pace | Cuts a minute | |
|---|---|---|
| `contemplative` | under 4 | slower than a calm blink rate |
| `dialogue` | 4 to 10 | conversation runs around 6 |
| `brisk` | 10 to 18 | |
| `action` | 18 to 40 | action runs around 25 |
| `frantic` | over 40 | beyond the range of blink rates |

`windows` gives the same per `window` seconds, so the pace can be followed through the film
(at most 600 windows: a longer film gets wider ones, and `windowSeconds` says how wide).
`flashes` lists shots shorter than `flashFrames`. `notes` calls out a metronome (six or more shots
with `variation` under 0.15), windows over 40 cuts a minute, flashes, and black between clips.
`shotList` lists up to 500 shots.

### `story.cuts`

`{"time"|"seconds"|"frame": the cut nearest?, "fromSeconds"?, "toSeconds"?, "limit":n=60, "emotion":0..1?, "story":0..1?}`.
Read-only. It renders four small frames per cut and mixes about a second of sound.

For each cut:

| Field | Meaning |
|---|---|
| `change` | How different the last outgoing and first incoming frames are (0…1) and what that reads as: `match` (below 0.035, the same picture), `jump` (0.035 to 0.13: changed, but not enough to read as a new shot), `clean` (a new picture). Inside a transition the frames outside it are compared. |
| `eye` | Where the eye most likely rests before and after (a saliency centroid in which movement counts ten times as much as contrast), and the `travel` between them as a fraction of the frame diagonal. |
| `direction` | Dominant horizontal movement on each side (−1 left, +1 right, 0 none) and whether it `reverses`. |
| `light` | Mean luma on each side. |
| `sound` | `pause`: whether the mix drops at least 12 dB at the cut against the half second before it (`null` without sound). `edit`: `straight`, `L`, `J`, `L+J`, or `null` when the clips have no linked sound. |
| `transition` | The video transition the cut sits in, if any. |
| `six` | The six criteria with their weights: emotion 51, story 23, rhythm 10, eye trace 7, planarity 5, space 4. `rhythm`, `eyeTrace` and `planarity` get a value from the measurements above when there is something to measure. `emotion` and `story` are `null` unless you pass them for one cut; `space` is never measured. `score` is the weighted mean of the criteria that have a value, and `covers` is the weight they add up to. |
| `issues` | Plain statements: a jump cut, a cut that barely changes the picture, a long eye travel, a reversal, a flash, a straight cut on a pause. |

`summary` counts them, and `notes` says so when most cuts change picture and sound together on a
pause. With more cuts than `limit`, `truncated` is true and `nextFromSeconds` is where to go on.

The thresholds are calibrated on synthetic pictures and a handful of real shots. Treat `change`,
`eye` and `direction` as evidence to look at, not as verdicts: a busy camera move reads as a clean
change, and a subject that the saliency model does not rank first can still be where people look.

### `story.blinkPoints`

`{"clip":id|"item":id, "fromSeconds"?, "toSeconds"?, "minPause":seconds=0.25, "picture":bool=true, "markers":bool=false}`.

Candidate cut points in a timeline clip (sequence time) or in a project item's rushes (media time):

- `phrase`: just after a phrase in the sound ends. Phrases are found from the track's own levels
  (10 ms steps, a threshold 35% of the way from its quiet floor to its loud level), so a noisy
  location track and a clean voice-over are read alike. Each point carries its `pause`.
- `movement`: where a burst of movement in the picture dies down to about a third and stops
  falling (about eight looks a second).
- `both`: a movement that completes within four frames of a phrase end.

A picture clip is read with its linked sound and a sound clip with its linked picture.
`markers: true` adds a sequence marker named "Blink point" at each (clip only, one undo step,
at most 200). One call analyses at most ten minutes; pass a range for more.

There is no eye or face detection here. The points are where a thought or a movement completes,
which is where a listener would blink; an actor's own blinks are not looked for.

### `story.board`

`{"path":str, "item":id?, "count":n=12, "fromSeconds"?, "toSeconds"?, "columns":n=4, "width":px=320, "max":n=48, "labels":bool=true}`.
Read-only apart from the PNG it writes to `path`.

Without `item`: one frame per shot of the active sequence, the one of five frames sampled across
the shot that is most like the other four. With `item`: `count` frames spread evenly across that
clip's media. Cells run left to right, top to bottom, labelled with their number and time, and
`cells` says what each is. A board of more than 16 million pixels is refused before anything is
rendered: lower `width`, `max` or `count`. The MCP tool `story_board` takes the same options (snake case, no
`path`) and returns the image.

### `story.splitEdit`

`{"kind":"L"|"J", "frames":n=12, "roll":"audio"|"video"="audio", "time"|"seconds"|"frame": the cut nearest?}`.

Rolls the sound cut of the two clips at the nearest cut later (`L`: the outgoing sound runs on
under the new picture) or earlier (`J`: the incoming sound arrives first), leaving the picture cut
where it is; `roll: "video"` moves the picture cut instead. Both clips need linked sound that meets
at one cut on one track. The roll stops at the end of the clips' media; `frames` reports how far it
went and `clamped` whether that was less than asked.

### `story.shadowCut` and `story.compare`

`story.shadowCut {"name":str?}` copies the active sequence into the project as
"<name> (shadow cut N)" and leaves the active sequence open.
`story.compare {"with": sequenceItemId}` reports the rhythm of both, the difference in length, the
cuts they share (within half a frame) and the cuts only one of them has.

## Layers

A region of a clip, treated as a layer of its own. A `region` is given in fractions of the clip's
own picture, from its top-left corner:

```json
{"shape": "rectangle", "x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4}
{"shape": "ellipse",   "x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4}
{"points": [[0.1, 0.1], [0.5, 0.1], [0.3, 0.8]]}
```

The commands are built from masks, mask tracking, tracks, clip speed and effects, which stay
editable afterwards.

| Command | Parameters | What it does |
|---|---|---|
| `layers.extract` | `clip`, `region`, `feather`=12, `follow`=false, `cutOut`=false, `name`? | Copies the clip onto a new track above and masks the copy to the region. `follow` tracks the mask through the clip. `cutOut` puts the inverse mask on the original. |
| `layers.cleanPlate` | `clip`, `path` (PNG), `samples`=15, `region`?, `feather`=24, `place`=true | Writes the per-pixel median of `samples` frames across the clip, imports it and lays it on a new track over the clip, masked to `region` when one is given. Reports `stillness`. |
| `layers.lift` | `clip`, `region`, `path`, `feather`=12, `samples`=15, `follow`=false | A plate over the region, then the region as a layer above it: the layer can be moved, scaled, re-timed or switched off without a double underneath. |
| `layers.retime` | `clip`, `region`, `speed` (per cent), `feather`=12, `interpolation`? | Extracts the region and plays it at `speed`, trimmed to end with the shot. |
| `layers.adjust` | `clip`, `region`, `effect`, `params`?, `feather`=12, `follow`=false | Applies an effect with a mask of the region. |

What to expect:

- **A clean plate is a statistic.** It needs a camera that does not move and a subject that does.
  A pixel the subject covers in half the samples or more keeps the subject. `stillness.settled`
  is the fraction of the picture the samples agree on; under 0.9 the result warns that the shot is
  not locked off. `stillness.disturbed` is how much of the frame something moved through; near
  zero the result warns that nothing moved.
- **`layers.retime` is a split screen in time.** It reads as one picture while the subject stays
  inside the region and the camera holds still. A subject that crosses the region's edge is cut
  by it; lift the subject first and re-time the lifted layer.
- **`follow` uses the mask tracker**, which needs media of its own (not a nested sequence) and
  enough detail inside the region. `tracking.error` says when it lost the subject, and keeps the
  frames it tracked.
- Each frame of a plate is held in memory while it is made (at most 768 MB of frames; fewer
  samples are used for very large frames).
- The plate is a PNG on disk at `path`, imported like any still. If a later step of the command is
  refused the project is put back as it was, but the file stays where it was written.

## `color.relight`

`{"clip":id, "time":"HH:MM"|hours, "toTime"?, "strength":0..1=1, "light":[x,y]?, "lightStrength":0..1=0.5}`.

Grades the clip toward the light of an hour of the day. Color Grading takes the white balance,
tint, exposure, contrast, saturation, highlights and shadows of that hour from a table of the day;
Lighting Effects adds one soft light where the sun or the moon stands, or at `light`. With
`toTime` both are keyframed across the clip through every hour of the table in between. Running it
again rewrites the same two effects.

It is a grade: it assumes the shot was photographed in neutral daylight and moves its colour and
exposure. It casts no new shadows and lights no windows.

## What these do not do

Some editing products now replace an object with a generated one, change a material from a
slider, or show a place in another decade. Those results come from a generative video model.
FilmCraft has none, and these commands do not imitate one: they move, mask, re-time and grade the
pixels a camera recorded.
