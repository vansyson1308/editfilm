# Story tools and layers

Commands for judging an edit and for working on part of a picture. They are engine commands like
every other: run them from the CLI (`filmcraft-cli exec story.rhythm`), the control channel, or
MCP (`command_run`). The MCP server also has a `story_board` tool that returns the board as an
image. None of them has a menu entry or a panel yet.

Three rules hold for all of them.

- **They measure and assemble; they do not decide and do not invent.** A cut is judged first on
  what the audience should feel and on whether the story moves. Nothing computes that, so the
  commands report what can be measured and say what is left to the editor. No command generates
  picture content: every pixel comes from frames that exist.
- **One call, one undo step.** A command that edits either completes or leaves the project, its
  undo history and its redo history as they were.
- **A result says what it could not do.** Read `warnings` and `notes`. Commands that change the
  picture also report `pictureChange`, so a change nobody will see is not mistaken for one that
  was made.

## Finding your way: `sequence.clips`

`{"fromSeconds"?, "toSeconds"?, "track": "V1"|id?}`. Read-only.

The clips of the active sequence, track by track (video from the top down, then audio), in a
form short enough to read: `clip`, `name`, `item`, `start`, `end` (seconds), `startFrame`,
`endFrame`, `sourceIn`, and only what is unusual about a clip (`link`, `speed`, `reverse`,
`disabled`, `held`, and the `effects` that were added to it). Each track lists its transitions
with the clips they join. Use it to pick ids; use `sequence.inspect` when you need every
parameter.

## Story tools

The ideas follow Walter Murch's *In the Blink of an Eye*: the six criteria of a cut and their
order, the blink as the mark of a completed thought, cut rates set against blink rates, the
representative frame of a shot, and trying versions against each other.

A **shot** is read from the picture: at each moment, the topmost enabled clip of footage (media,
subclips, nested sequences) that shows a whole picture. Titles, graphics and adjustment layers
ride on a shot and do not start one. Neither does footage that shows only part of a picture:

- a clip masked down to a region, which is what `layers.extract`, `layers.lift` and
  `layers.retime` make;
- footage with an alpha channel: a logo, a watermark, a plate;
- a clip placed over less than half the frame: a picture in a picture, a face camera in a
  corner. (Letterboxed footage covers most of the frame and is a shot.)

These ride on the shot under them, so a watermark across the whole film hides no cut. With
nothing whole on screen, the lowest of them is the shot. A **cut** is where the shot's clip
changes. Black between clips counts as a shot, except at the head and tail.

A cut need not fall on a frame of the sequence (a clip re-timed to 66.67 frames, a trim made in
ticks). The programme is seen frame by frame, so a cut is read from the last frame on screen
before it and the first frame at or after it.

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
(at most 600 windows: a longer film gets wider ones, and `windowSeconds` says how wide). Each
window says how many `seconds` it covers: a tail shorter than half a window is counted with the
window before it, so that one cut in the last second and a half does not read as forty a minute.
`flashes` lists shots shorter than `flashFrames`. `notes` calls out a metronome (six or more shots
with `variation` under 0.15), windows over 40 cuts a minute (a measurement, not a fault: a montage,
a trailer or a music video is cut that way on purpose), flashes, and black between clips.
`shotList` lists up to 500 shots.

### `story.cuts`

`{"time"|"seconds"|"frame": the cut nearest?, "fromSeconds"?, "toSeconds"?, "limit":n=60, "emotion":0..1?, "story":0..1?}`.
Read-only. It renders four small frames per cut and mixes about a second of sound.

For each cut:

| Field | Meaning |
|---|---|
| `change` | How different the last outgoing and first incoming frames are (`difference`, 0…1), how alike they are in what is where (`likeness`, −1…1, `null` for a frame too even to have a pattern), and what that reads as: `match` (difference below 0.035, the same picture), `jump` (0.035 to 0.13: changed, but not enough to read as a new shot), `clean` (a new picture). `likeness` qualifies a `jump`: at 0.6 or more it is the same picture, shifted, and a jump cut for certain; below that, or with no pattern to compare, it may also be a new shot in the same tones (two night exteriors, two shots in one grey room), which two frames alone cannot tell from a subject that hopped on a plain backdrop, and the issue says to look. A cut to or from black is never called a jump cut. Inside a transition the frames outside it are compared. |
| `eye` | Where the eye most likely rests before and after (fractions of the frame's width and height), and the `travel` between them as a fraction of the frame's diagonal as displayed, so that it means the same in a wide frame and a vertical one. Contrast draws the eye; movement draws it ten times as hard, but only movement gathered in one place. When the whole picture moves (water, foliage, a pan, a hand-held frame) it leads the eye nowhere and is left out. |
| `direction` | Dominant horizontal movement on each side (−1 left, +1 right, 0 none) and whether it `reverses`. |
| `light` | Mean luma on each side. |
| `sound` | `pause`: whether the mix drops at least 12 dB at the cut against the half second before it (`null` without sound). `edit`: `straight`, `L`, `J`, `L+J`, `continuous`, or `null` when neither clip has linked sound. `continuous` is one unbroken recording running under the cut on every track it is on (a film cut into pieces after it was mixed): nothing changes there for the ear, and it is not a split edit. A clip's sound is all of its linked sound, on however many tracks. |
| `transition` | The video transition this cut sits in, if any: one that joins the two clips of the cut. A transition on another clip that merely spans the same time (a title fading in above) is not counted. |
| `offline` | True when the frames compared were the offline slate: the media is missing, and the measurements say nothing about the footage. |
| `six` | The six criteria with their weights: emotion 51, story 23, rhythm 10, eye trace 7, planarity 5, space 4. `rhythm`, `eyeTrace` and `planarity` get a value from the measurements above when there is something to measure. `emotion` and `story` are `null` unless you pass them for one cut; `space` is never measured. `score` is the weighted mean of the criteria that have a value, and `covers` is the weight they add up to. |
| `issues` | Plain statements: a jump cut (or a change too small in sum to say which), a cut that barely changes the picture, a long eye travel, a reversal, a flash, a straight cut on a pause, offline media. |

`summary` counts them (`jump`, `match`, `clean`, `directionReversals`, `straightCutsOnAPause`,
`continuousSound`, `splitEdits`, `offline`), and `notes` says so when most cuts change picture and
sound together on a pause. With more cuts than `limit`, `truncated` is true and `nextFromSeconds`
is the `fromSeconds` that goes on with the next cut.

The thresholds are calibrated on synthetic pictures and a handful of real shots. Treat `change`,
`eye` and `direction` as evidence to look at, not as verdicts: a busy camera move reads as a clean
change, and a subject that the saliency model does not rank first can still be where people look.

### `story.blinkPoints`

`{"clip":id|"item":id, "fromSeconds"?, "toSeconds"?, "minPause":seconds=0.25, "picture":bool=true, "markers":bool=false}`.

Candidate cut points in a timeline clip (sequence time) or in a project item's rushes (media time):

- `phrase`: just after a phrase in the sound ends. Phrases are found from the track's own levels
  (10 ms steps, a threshold 35% of the way from its quiet floor to its loud level), so a noisy
  location track and a clean voice-over are read alike. The sound is read first in the band a
  voice lives in (300 Hz to 3 kHz), where speech shows its pauses over an engine, wind or surf;
  sound with nothing there (a drum, a door) is read whole. Each point carries its `pause`.
- `movement`: where a burst of movement in the picture dies down to about a third and stops
  falling (about eight looks a second). A burst is measured against the two and a half seconds
  around it, so a glance or a turn of the head is found in a hand-held shot that never holds
  quite still.
- `both`: a movement that completes within four frames of a phrase end.

A picture clip is read with all the sound linked to it, and a sound clip with its linked picture
(where that picture clip is on the timeline).
Sound is looked for only where the clip has sound: after a split edit, or under a sound clip
shorter than its picture, the stretch with nothing in it is not taken for a pause, and `notes`
says so. `analysed` says whether sound and picture could actually be read; `notes` says why
nothing was found (no pause in the sound, nothing that starts and stops in the picture, media that
could not be read).

A subclip is a stretch of its parent's media: its times, and `fromSeconds`/`toSeconds`, are the
parent's media times, from its In point to its Out point.

`markers: true` adds a sequence marker named "Blink point" at each (clip only, one undo step,
at most 200); without it the command only reads. One call analyses at most ten minutes; pass a
range for more.

There is no eye or face detection here. The points are where a thought or a movement completes,
which is where a listener would blink; an actor's own blinks are not looked for.

### `story.board`

`{"path":str, "item":id?, "count":n=12, "fromSeconds"?, "toSeconds"?, "columns":n=4, "width":px=320, "max":n=48, "labels":bool=true}`.
Read-only apart from the PNG it writes to `path`.

Without `item`: one frame per shot of the active sequence, the one of five frames sampled across
the shot that is most like the other four (always a frame the shot itself is on screen for). With
`item`: `count` frames spread evenly across that clip's media (for a subclip, across the subclip).
Cells run left to right, top to bottom, labelled with their number and time, and `cells` says what
each is. Frames are laid out as they are shown: anamorphic footage is not squeezed. `path` must
end in `.png` and may not be a file the project reads. A board whose sheet (whole rows of cells, with the gaps between them) would exceed
16 million pixels is refused before anything is rendered, and the message gives the size it would
have had: lower `width`, `max` or `count`. The MCP tool `story_board` takes the same options
(snake case, no `path`) and returns the image.

### `story.splitEdit`

`{"kind":"L"|"J", "frames":n=12, "roll":"audio"|"video"="audio", "time"|"seconds"|"frame": the cut nearest?}`.

Rolls the sound cut of the two clips at the nearest cut later (`L`: the outgoing sound runs on
under the new picture) or earlier (`J`: the incoming sound arrives first), leaving the picture cut
where it is; `roll: "video"` moves the picture cut instead.

- Both clips need linked sound. Every audio track on which their sounds meet at one cut is rolled,
  by one amount (a clip recorded on two microphones keeps them together); `pairs` lists what was
  rolled, and `warnings` names a track where the two clips' sounds do not meet and were left alone.
- The roll stops at the end of the clips' media; `frames` reports how far it went and `clamped`
  whether that was less than asked.
- It is refused when the two clips are one continuous recording cut in two: moving the cut between
  them changes nothing anyone can see or hear. A split edit needs two separately recorded clips.
- A plate or a layer over a shot has no sound of its own; the cut is split with the sound of the
  clip it was made from. With one lying over the shot, `roll: "video"` is refused (it would pull
  the plate away from its clip).

### `story.shadowCut` and `story.compare`

`story.shadowCut {"name":str?}` copies the active sequence into the project and leaves the active
sequence open. Without `name` the copy is called "<sequence> (shadow cut N)", with the first N no
item has; a `name` is used as it is given.

`story.compare {"with": sequenceItemId, "picture":bool=true, "limit":n=60}` says what differs
between the active sequence and another:

- `shots`: each shot of the active sequence lined up with the shot of the other version that shows
  the same footage (footage used twice in a film is lined up use for use). `status` is `same`,
  `changed` or `new`; `what` is a list of `moved`, `shorter`, `longer`, `looks different` (empty
  when nothing is); `there` is the other version's shot, `shiftSeconds` how far it moved; a `new`
  row says why in `note`. A shot that plays reversed or holds a frame cannot be lined up by
  time: it is the same shot where the other version plays the same stretch the same way, and
  `new` otherwise.
- With `picture`, up to `limit` shots are also rendered in both versions and compared
  (`picture.reads`: `none`, `subtle`, `visible`, `strong`), which is how a grade or a layer shows
  up. When the two sequences' frames are not the same shape (a 16:9 master against a vertical
  version) no picture can be compared: `picture` is `null` and `notes` says so.
- `shotsOnlyThere`: shots of the other version this one no longer has.
- `summary` counts every shot. At most 500 rows are listed on each side; `truncated` and `notes`
  say when there are more.
- `cutsShared`, `cutsOnlyHere`, `cutsOnlyThere` (cuts within half a frame are the same cut), the
  rhythm of both, and `secondsDifference`.
- `sameShots`: the same stretches of the same footage at the same times, with the same effects on
  them. `identical`: nothing on any track differs at all, sound and titles included.

## Layers

A region of a clip, treated as a layer of its own. A `region` is given in fractions, from the
top-left corner:

```json
{"shape": "rectangle", "x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4}
{"shape": "ellipse",   "x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4}
{"points": [[0.1, 0.1], [0.5, 0.1], [0.3, 0.8]]}
{"space": "frame", "atSeconds": 12.5, "x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4}
```

The fractions are of the **clip's own picture**, which is what a mask is drawn on. While the clip
fills the frame that is the same as a fraction of the frame. Once the clip is moved, zoomed,
turned or letterboxed it is not, so a region read off a rendered frame takes `"space": "frame"`:
fractions of the programme frame as seen, carried back through the clip's Motion, Scale to Frame
Size and pixel shape. For a clip whose Motion is animated, `atSeconds` is the sequence time of the
frame the region was read from (default: the clip's first frame). A region that falls beside the
clip's picture is refused.

The commands are built from masks, mask tracking, tracks, clip speed and effects, which stay
editable afterwards.

| Command | Parameters | What it does |
|---|---|---|
| `layers.extract` | `clip`, `region`, `feather`=12, `follow`=false, `cutOut`=false, `name`? | Copies the clip onto a new track above it and masks the copy to the region. `follow` tracks the mask through the clip. `cutOut` takes the region out of the original. The picture is unchanged until the layer is moved or altered. |
| `layers.cleanPlate` | `clip`, `path` (PNG), `samples`=15, `region`?, `feather`=24, `place`=true | Writes the per-pixel median of `samples` frames across the clip, imports it and lays it on a new track above the clip, masked to `region` when one is given. Reports `stillness`. |
| `layers.lift` | `clip`, `region`, `path`, `feather`=12, `samples`=15, `follow`=false | A plate over the region, then the region as a layer above it: the layer can be moved, scaled, re-timed or switched off without a double underneath. |
| `layers.retime` | `clip`, `region`, `speed` (per cent), `feather`=12, `interpolation`? | Extracts the region and plays it at `speed`. Slower, the layer is trimmed to end with the shot; faster, it runs out before the shot does (`untilShotEnd` false, `seconds` says how long it covers). |
| `layers.adjust` | `clip`, `region`, `effect`, `params`?, `feather`=12, `follow`=false | Applies an effect with a mask of the region. |
| `layers.transform` | `layer`, `scale`? (per cent), `offset`? (`[dx, dy]`), `anchor`=`"bottom"`, `mask`? | Makes a layer larger or smaller, or shifts it, in place: sets the layer's Motion Scale and keys its Position so that the anchor of its mask's bounds stays where it is in the frame. |

Every one of them returns `warnings` (a list, empty when there is nothing to watch) and
`pictureChange` (`layers.cleanPlate` with `place` false changes no picture and reports none).

What to expect:

- **New tracks go above the clip and above what was already made from it** (its plates and
  masked layers, in the order they were made), and under whatever else lies over it: a cutaway,
  a title, or a second clip of the same footage (a punch-in) still covers them.
- **A layer and a plate take everything that shapes the clip's picture**: its place in its media,
  speed and direction, Motion, Opacity, effects and masks. A trimmed, re-timed, reversed, graded,
  flipped or animated clip gets a plate that stays in step with it.
- **Masks combine.** A cut-out is a Subtract mask, so a second cut-out on the same clip leaves the
  first in place; a region kept on a clip that already has masks narrows it instead of widening it.
- **A clean plate is a statistic.** It needs a camera that does not move and a subject that does.
  A pixel the subject covers in half the samples or more keeps the subject. `stillness.settled`
  is the fraction of the picture the samples agree on; under 0.9 the result warns that the shot is
  not locked off. `stillness.disturbed` is how much of the frame something moved through; near
  zero the result warns that nothing moved.
- **`pictureChange`** compares the programme before and after at three moments of the clip:
  `mean`, `strongest` (the cell of a 32×18 grid that changed most) and `reads`: `none`, `subtle`
  (visible only side by side), `visible`, `strong`. An adjustment meant to be noticed that reads
  `subtle` needs a larger amount.
- **`layers.retime` is a split screen in time.** It reads as one picture while the subject stays
  inside the region and the camera holds still. A subject that crosses the region's edge is cut
  by it; lift the subject first and re-time the lifted layer. A reversed shot's layer plays
  reversed too.
- **`layers.transform` scales a subject where it stands.** Motion scales a clip about its Anchor
  Point, which for a layer is the middle of the whole picture, not the subject: scaled there, a
  subject that crosses the frame crosses it faster and leaves the place it was cut from. This
  command reads the layer's opacity mask instead. At each keyframe of the mask path (one per frame
  after `follow`; a path that does not move gives one position and no keyframes) it takes the
  `anchor` of the path's bounds and writes a Motion Position that keeps it where it is:
  `"bottom"` (the default: bottom centre, where a standing subject meets the ground), `"center"`,
  `"top"`, or `[fx, fy]` in fractions (0 to 1) of the bounds from their top-left corner.

  ```json
  {"layer": 21, "scale": 150}
  {"layer": 21, "scale": 60, "anchor": "center", "offset": [0, -40]}
  ```

  - `scale` is the layer's Motion Scale (0.1 to 10000), as Effect Controls shows it: a layer cut
    from a clip at 100 is half as large again at 150, and 100 puts it back. Asked twice for the
    same scale, the second call changes nothing: `changed` is false and no undo step is made.
    `offset` moves the subject by `[dx, dy]` sequence pixels from where it is now, so asking
    twice moves it twice. Give either or both.
  - For a layer that fills the frame unturned, the position at each keyframe is
    `(1 - s) * anchor + s * A + offset`, with `s` the scale as a factor and `A` the Motion Anchor
    Point. In general the bounds are the ones seen in the frame, through the layer's own Motion:
    the bottom of a layer that is turned is its bottom as it shows.
  - While only the mask path moves, the Position gets a keyframe at each of the path's, with the
    path's easing, and holds where the path holds (a path that changes its number of points
    between two keyframes jumps). When the layer's own Motion is keyframed as well (Position,
    Rotation, Anchor Point, or a Scale that is left alone), two animations decide where the anchor
    falls and no keyframes at theirs follow both in between: the Position then gets a keyframe on
    every frame the layer is on screen, and the layer's other keyframes stay as they are. That
    is refused for a layer longer than 20000 frames. With only `offset` the Position's own
    keyframes are moved and keep their easing.
  - A Scale that is keyframed is refused when `scale` is given: take the keyframes off, or give
    only `offset`.
  - A subject made smaller, or shifted, uncovers the place it stood: the clip the layer was made
    from shows the subject there a second time, or the hole it was cut from. `warnings` says so
    unless a plate lies between that clip and the layer, which is what `layers.lift` makes. A
    still under the clip is a backdrop and hides nothing of it. A layer whose clip is gone or
    switched off has nothing behind it to show twice, and gets no warning.
  - It returns `scale` and `was`, the `position` and `anchorAt` (where the anchor is in the
    frame) at the layer's first frame, the number of Position `keyframes` it holds, and `changed`.
    `mask` picks one of several opacity masks by index; the default is the last that keeps a
    region. `clip` names the layer as well as `layer` does.
  - It is one undo step. A clip with no opacity mask is refused, as are a clip on a locked track,
    one whose Motion is switched off, and an adjustment layer (its masks are drawn on the frame,
    and its Motion does not move them).
- **`follow` uses the mask tracker**, which needs media of its own (not a nested sequence) and
  enough detail inside the region. It tracks every frame before the command returns, so it is
  refused on a clip longer than 3000 frames: cut the clip down to the stretch that matters, or add
  the mask without `follow` and run `masks.track`. `tracking.error` says when it lost the subject,
  and keeps the frames it tracked.
- **Transitions belong to the clip they were put on.** A layer or a plate does not fade with the
  clip's transitions; `warnings` says so when the clip has any.
- **Limits.** Each frame of a plate is held in memory while it is made (at most 768 MB of frames;
  fewer samples are used for very large frames). The plate is made in Rec. 709; in an HDR
  sequence its colour will not match exactly, and `warnings` says so. A multi-camera clip and a
  clip that starts before the beginning of the sequence are refused.
- **The plate is a PNG on disk at `path`**, imported as one still. A path the project already
  reads is refused (it would change that media under its clips). If a later step of the command
  is refused the project is put back as it was, but the file stays where it was written.

## `color.relight`

`{"clip":id, "time":"HH:MM"|hours, "toTime"?, "strength":0..1=1, "light":[x,y]?, "lightStrength":0..1=0.5}`.

Grades the clip toward the light of an hour of the day. Color Grading takes the white balance,
tint, exposure, contrast, saturation, highlights and shadows of that hour from a table of the day;
Lighting Effects adds one soft light where the sun or the moon stands, or at `light`. With
`toTime` both are keyframed across the clip through every hour of the table in between.

- **The clock runs forward.** A `toTime` earlier in the day than `time` is the next day: 22:00 to
  02:00 is four hours through midnight, not twenty back through noon. `hours` says how long the
  move is.
- **It writes over what is there.** It uses the clip's Color Grading and Lighting Effects if it
  has them, replacing the Basic Correction values and the first light; `warnings` says when a
  grade that was already set is replaced. Running it again rewrites the same two effects. An
  effect with a mask is a region's own adjustment (`layers.adjust`): it is left alone, and the
  shot gets a grade of its own.
- `lightStrength` 0 adds no light, and switches off one that is there (keeping its settings).
- 00:00 to 24:00 is the whole day; 24:00 to 00:00 is no time at all.
- A clip that shows one moment of its media (a frame hold with Hold Filters) has no time for the
  light to change in: it gets the light of `time`, still, and `keyframes` is 0.
- `pictureChange` says how much the grade shows. A clip on a locked track is refused.

It is a grade: it assumes the shot was photographed in neutral daylight and moves its colour and
exposure. It casts no new shadows and lights no windows.

## Scene detection: cuts between shots that look alike

`clip.sceneEditDetection` finds the cuts in a clip that was edited before it came in. Its
`sensitivity` sets how much the picture must change for a cut. A cut between two shots that look
alike (the same room, the same light, the same sea from another boat) can fall below a
sensitivity that is right for the rest of the film, and is still one sharp step in a quiet
stretch. Those are found too (`lookAlike`, on by default): they are among `cuts`, and
`lookAlikeCuts` lists which they are, with the difference each made. A flash, a lightning stroke
or a dropped frame changes the picture and changes it back; that is two steps, and no cut.
Frames shown twice (slowed footage, or 25 fps media in a 50 fps sequence) are not taken for a
quiet stretch.

The lists come back with `"wait": true`. A job that is not waited for applies its cuts when it
finishes and says how many were between look-alike shots in its message.

With `"lookAlike": false` only the sensitivity decides. The cuts it leaves out are then listed as
`nearMisses`, each with the `sensitivity` that would find it, and `warnings` names the sensitivity
that finds them all.

## What these do not do

Some editing products now replace an object with a generated one, change a material from a
slider, or show a place in another decade. Those results come from a generative video model.
FilmCraft has none, and these commands do not imitate one: they move, mask, re-time and grade the
pixels a camera recorded.
