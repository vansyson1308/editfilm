# Agents

Filmagic is built to be driven by AI agents, and largely built by them. Part 1 covers using
Filmagic from an agent. Part 2 covers developing Filmagic as an agent.

# Part 1: driving Filmagic

## 1. MCP server

`filmagic-cli mcp` serves MCP on stdio. It drives one of two things:

| Mode | Command | What it drives |
|---|---|---|
| App | `filmagic-cli mcp` while the desktop app runs with `--control`, or `--bridge 127.0.0.1:9876` | the desktop app the user has open: they see every edit |
| Headless | `filmagic-cli mcp` with no such app, or `--headless`, `--demo`, `--project p.fcproj` | an in-process engine session, no window: the user sees nothing |

With no mode option, the server looks in the app's control folder (see
[below](#driving-the-desktop-app)). When a running app there proves that it holds its credential,
the server drives it. Otherwise the server works headless on an empty project. It writes one line to stderr
saying which mode it chose, and the MCP tool `app_status` says so too. The choice is made once, at start. If
the app opens later, the server does not move the agent's work into it behind the agent's back.
The agent calls `app_open` for that (see [Opening the app mid-session](#opening-the-app-mid-session)).

`--bridge ADDR` drives that address. `--headless` (an empty project), `--demo` and `--project`
always work headless, even while the app runs. `--data-dir DIR` sets the data directory. Headless
mode reads user presets from it, and every mode looks in that data directory's control folder. The
default is the app's own data directory, or `FILMAGIC_DATA_DIR` when that is set. Any other
option is rejected (exit 2). There are no folder-confinement flags such as
`--automation-read-root`, so the server can read and write wherever the user can.

The agent starts the server itself, as a child process speaking MCP on stdin/stdout. A server
started by hand in a terminal has no agent attached to it.

### Build

There are no published releases yet: build from source (Rust 1.95 or newer; on Linux a C toolchain,
`pkg-config` and the ALSA development package, e.g. `sudo apt install build-essential pkg-config
libasound2-dev`; see [contributing.md](contributing.md#1-setup)).

```sh
git clone https://github.com/vansyson1308/editfilm && cd editfilm
cargo build --release -p filmagic -p filmagic-cli
```

The binaries are `target/release/filmagic` (the desktop app) and `target/release/filmagic-cli`
(`filmagic.exe` and `filmagic-cli.exe` on Windows).

### Claude Code

Inside the checkout, the repository's [`.mcp.json`](../.mcp.json) registers two servers:
`filmagic` (`mcp`: the open app when there is one, else headless) and `filmagic-headless`
(`mcp --demo`, always headless). Their `command` is the relative path
`target/release/filmagic-cli`. Start `claude` at the root of the checkout, approve the project
servers when asked, and check them with `/mcp`.

From any other folder, register the binary by its absolute path. `--scope user` makes it available
in every project; it goes before `--`, because everything after `--` is passed to `filmagic-cli`,
which rejects options it does not know (exit 2, "unknown mcp option"):

```sh
claude mcp add --scope user filmagic -- /abs/path/editfilm/target/release/filmagic-cli mcp
claude mcp add --scope user filmagic-headless -- /abs/path/editfilm/target/release/filmagic-cli mcp --demo
```

Use `mcp --project /abs/path/film.fcproj` instead of `--demo` to open your own project.

### Codex

Codex reads [`AGENTS.md`](../AGENTS.md) at the repository root by itself. It does not read
`.mcp.json`; its MCP servers are configured in `~/.codex/config.toml`. `codex mcp add` always
writes that global file, so there is no separate "inside the checkout" setup: one entry, with the
absolute path of the binary, serves the checkout and every other folder. Add the server with
`codex mcp add <name> -- <command> <args…>`:

```sh
codex mcp add filmagic -- /abs/path/editfilm/target/release/filmagic-cli mcp
codex mcp list            # or /mcp inside Codex
```

That writes this table to `~/.codex/config.toml`, which can also be edited by hand:

```toml
[mcp_servers.filmagic]
command = "/abs/path/editfilm/target/release/filmagic-cli"
args = ["mcp"]
```

The server finds the app through its control folder, so no credential goes into this file. Only the
[fixed-token setup](#fixed-token-filmagic_control_token) needs `env_vars = ["FILMAGIC_CONTROL_TOKEN"]`
in the table.

### Claude Desktop

Claude Desktop starts the servers listed in `claude_desktop_config.json`: Settings ▸ Developer ▸
Edit Config opens it. On Windows the file is usually `%APPDATA%\Claude\claude_desktop_config.json`,
and on macOS `~/Library/Application Support/Claude/claude_desktop_config.json`. Use the absolute path
of the binary. JSON needs every backslash doubled:

```json
{
  "mcpServers": {
    "filmagic": {
      "command": "C:\\Users\\you\\editfilm\\target\\release\\filmagic-cli.exe",
      "args": ["mcp"]
    }
  }
}
```

Quit Claude Desktop completely (on Windows, also from the tray icon) and start it again: it reads
the file only at start. No token is needed. Claude Desktop cannot easily pass one through its
environment, and the server finds the app through its control folder instead. If the app runs with
`--data-dir`, give the server the same folder: `"args": ["mcp", "--data-dir", "D:\\Filmagic data"]`.

### Driving the desktop app

Start the app with its control server. `--control <port>` takes a port (`0` lets the system pick
a free one):

```sh
./target/release/filmagic --control 9876 >/dev/null 2>&1 &   # keeps the app's log out of the agent's terminal
```

On Windows, add `--control 9876` to a shortcut's Target, or in PowerShell:

```powershell
Start-Process .\target\release\filmagic.exe -ArgumentList '--control','9876'
```

Then start the agent, or let an agent that is already running call `app_status` or `app_open`. An
agent started before the app came up is headless: it reaches the app through `app_open`, or by being
restarted.

When the control server starts, the app makes a random credential for the session (64 hex
characters), unless `FILMAGIC_CONTROL_TOKEN` is set. It writes its port, process id and version,
plus that session credential, to `app-<pid>.json` in its **control folder**. A credential from
`FILMAGIC_CONTROL_TOKEN` is never written there. Every running app also holds a lock on
`instance-<pid>.lock` in the same folder, with or without `--control`. The operating system
releases the lock when the app ends, however it ends.

The control folder belongs to the data directory, but it is always somewhere only you can read:

- **Windows:** `%LOCALAPPDATA%\Filmagic\control\<key>\`, where `<key>` names the data directory.
  `%LOCALAPPDATA%` is in your user profile, which other users who are not administrators cannot
  open. The data directory itself may be readable by everyone. The portable build keeps it next to
  the executable (`data\` beside `filmagic.exe`), and a folder such as `C:\Tools\Filmagic`
  inherits `C:\`'s permissions, which let every user read it. That is why the control folder is
  not in the data directory on Windows.
- **macOS and Linux:** `control/` inside the data directory: `~/.local/share/filmagic/control/`
  on Linux, `~/Library/Application Support/Filmagic/control/` on macOS, or under `--data-dir` /
  `FILMAGIC_DATA_DIR`. The folder is created with mode `0700` and the files with `0600`. Readers
  refuse a folder or file that belongs to another user or that other users can write, and a file
  other users can read.

The app's log is in `Logs/filmagic.log` in the data directory. The files are protected and checked
as follows:

- **Atomic writes.** Each file is written to a temporary name and renamed into place, so a reader
  never sees half a file.
- **One file per app.** Each app writes only its own `app-<pid>.json` and removes only that file
  when it exits. With two apps running, closing one leaves the other findable. Agents attach to
  the newest app that answers.
- **The server proves itself before the credential is sent.** A port named in a file can belong
  to another program once the app that had it exits. A client therefore never sends the credential
  first. It sends `control.hello` with a random nonce. The server must answer with an HMAC-SHA256,
  under the credential, of that nonce and its own port. Only then does the client send anything
  else. A program on the port cannot produce that answer, not even by relaying the real app's
  answer from another port, so it learns nothing.
- **A crash leaves nothing that misleads.** An app that crashed leaves its files behind, but its
  lock is free. Readers see that, ignore those files and remove them.

The control server listens on 127.0.0.1 only, and refuses every request without the credential
(`control.hello` aside, which reveals nothing).

**Opened without `--control`?** Then there is nothing to attach to. Opening the app from the
Start menu, the Dock or a file starts no control server, so an agent cannot see or drive that
window. `app_status` reports it (`appWithoutControl: true`). `app_open` refuses to start a second
window, because the agent could not drive the first one either. Close the app and start it with
`--control <port>`, or let `app_open` start it.

### Opening the app mid-session

`app_status` says what the session drives: `mode: "app"` (with `answering`, whether the app still
answers) or `mode: "headless"` (with `appRunning`, whether an app is open that it is not driving).

`app_open` hands a headless session to the desktop app, so the user can watch:

1. It saves the headless project, to `path` when given. A project that was never saved needs
   `path`, an absolute `.fcproj` path. Without it, the call fails and says so.
   `path` must be absolute: the MCP server's working folder is whatever the client started it in.
2. If an app is already running, it only attaches to that app. It does not replace the app's open
   project, and it returns a note: run `file.open` with the saved path to continue there.
   Otherwise it starts the app (`filmagic --control 0 --data-dir <the server's> -- <project>`),
   waits up to 90 s for the app's control server, and attaches.
3. From then on, every command and the `ui_*` tools go to the app. The headless undo history does
   not carry over: the app opens the saved file.

It refuses, and changes nothing, in three cases:
- **Unfinished headless work.** An export, proxy or ingest job, mask tracking or scene detection
  that has not finished or not been applied would be lost with the headless session. The error
  names it; wait for `jobs.list` to show it finished, or cancel it with `jobs.cancel`.
- **Filmagic is open without `--control`** (see above).
- **A malformed `FILMAGIC_CONTROL_TOKEN`** in the server's environment, which would keep the
  started app's control server closed.

If the started app shows **Recover Unsaved Changes**, from an earlier session that ended without
saving, the result says so in `notes`. Recovering would replace the project the agent opened. Ask
the user: to keep the agent's project, close the dialog without recovering (the changes stay
available through File ▸ Recover Unsaved Changes).

The app is found next to `filmagic-cli` (`filmagic` or `filmagic.exe`, as in the Windows and Linux
archives and `target/<profile>/`), or at the path in `FILMAGIC_APP` in the server's environment. On
macOS the app is inside its bundle, so set `FILMAGIC_APP` to
`/Applications/Filmagic.app/Contents/MacOS/filmagic` (or wherever the bundle is). An agent never
chooses the executable. The app outlives the MCP server: closing the agent leaves the app and its
project open.

If the user closes the app, the next command fails with "Filmagic is not answering at …: the app
was closed". The session never falls back to headless on its own, because headless would edit a
different copy. `app_open` then starts the app again on the project it had: the server remembers
the app's project file while it drives the app. It reads the file again after `file.*` commands
and on `app_status`, and otherwise on the first call 30 s or more after the last reading. A project
the user opened by hand is picked up that way; call `app_status` before the user closes the app to
be sure. If that project was never saved, `app_open` needs
`path`, and it never starts on the demo project. Edits the closed app had not saved are offered by
its auto-save recovery. In app mode with the app still open, `app_open` changes nothing.

### Fixed token (`FILMAGIC_CONTROL_TOKEN`)

The older setup still works, and it takes precedence over the session credential. Set
`FILMAGIC_CONTROL_TOKEN` to 64 random hex characters, fresh for each session, and start the app
and the agent from that shell so both inherit it:

```sh
export FILMAGIC_CONTROL_TOKEN="$(openssl rand -hex 32)"      # or: python3 -c 'import secrets; print(secrets.token_hex(32))'
./target/release/filmagic --control 9876 >/dev/null 2>&1 &
claude                    # or codex
```

On Windows (PowerShell):

```powershell
$b = [byte[]]::new(32); [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($b)
$env:FILMAGIC_CONTROL_TOKEN = -join ($b | ForEach-Object { $_.ToString('x2') })
Start-Process .\target\release\filmagic.exe -ArgumentList '--control','9876'
claude                    # or codex
```

The app does not write this value to disk: its endpoint file holds only the port, so the clients
must have the variable themselves. A bridge (`mcp --bridge ADDR`, or `filmagic-cli --bridge ADDR
exec …`) sends `FILMAGIC_CONTROL_TOKEN` when it is set. Otherwise it sends the session credential of
the app on that port, after the server has proved itself. An agent started before the variable was
exported does not have it: exporting it in the agent's own shell tool, or reconnecting from `/mcp`,
does not help. Quit the agent and start it again from that shell; `app_status` and the server's
start-up line say when this is what is missing. A value that is set but is not 64 hex characters keeps the control server closed. The app
logs `control server disabled: FILMAGIC_CONTROL_TOKEN must contain 64 random hex characters` and
shows it in the status bar, so a typo is never replaced silently.

An agent can also drive the app without MCP, through the command-line tool. The credential comes
from the app's session credential in the control folder, or from the variable put in front of the
command (which keeps it out of the command's arguments):

```sh
./target/release/filmagic-cli --bridge 127.0.0.1:9876 exec sequence.clips
FILMAGIC_CONTROL_TOKEN=<token> ./target/release/filmagic-cli --bridge 127.0.0.1:9876 exec playhead.set seconds=2
```

A bridge with the wrong credential reaches the app but gets `control authentication failed`. For a
few seconds after launch the app answers `Filmagic is still starting; send the request again`.

### Windows console output

`filmagic-cli` prints UTF-8. Written to a console window, it shows correctly. Piped or redirected,
for example captured by PowerShell or by an agent's shell tool, it is often decoded in the
console's code page, and "…" turned into "�?�". So on Windows, output that does not go to a console
is ASCII:

- JSON escapes non-ASCII characters as `\uXXXX`, which any JSON reader turns back into the same
  text. Nothing is lost.
- Text spells "…" as `...`, "→" as `->`, "▸" as `>`, and shows any other non-ASCII character as
  `?`. Use `--json` for exact text.

This is ASCII, not UTF-8: the program cannot tell which code page the reader decodes with
without calling the Windows console API, which the project keeps out of everything but media code.
Git Bash and other MSYS2 terminals (`MSYSTEM` set) decode UTF-8, so there the output stays UTF-8.
Set `FILMAGIC_CLI_UTF8=1` to keep UTF-8 for another reader that decodes it, such as PowerShell
after `[Console]::OutputEncoding = [Text.Encoding]::UTF8`. MCP output is always UTF-8 JSON-RPC.

### Headless Linux machines

To run the desktop app (and so the bridge) on a Linux machine with no display, such as a cloud
container or a CI box, it needs three things a desktop already has: an X server, the
`libxkbcommon-x11` runtime library and a Vulkan driver (software rendering is enough). On Debian
and Ubuntu:

```sh
sudo apt install xvfb libxkbcommon-x11-0 mesa-vulkan-drivers
Xvfb :99 -screen 0 1920x1080x24 &
export DISPLAY=:99
./target/release/filmagic --control 9876 >/dev/null 2>&1 &
```

Without a Vulkan driver the app exits with "Failed to create surface for any enabled backend". The
headless MCP server (`mcp --demo`) needs none of this.

### Other MCP clients

Any client that starts stdio servers takes the same command and arguments:

```json
{ "mcpServers": { "filmagic": { "command": "/abs/path/filmagic-cli", "args": ["mcp"] } } }
```

The server finds the app through its control folder. With the fixed-token setup the client must pass
`FILMAGIC_CONTROL_TOKEN` through from its own environment (in Codex, `env_vars`). The token's
value never goes into a checked-in file.

### Future releases

Releases are not published yet. When they are, the packages will ship `filmagic-cli` beside the
desktop app, and the path to give the agent will be:

| Install | CLI |
|---|---|
| Windows (MSI) | `%LOCALAPPDATA%\Programs\Filmagic\filmagic-cli.exe` by default (`C:\Program Files\Filmagic\filmagic-cli.exe` for an all-users install, under `Program Files (x86)` instead for the 32-bit build), not on `PATH` |
| Linux (deb, rpm) | `/usr/bin/filmagic-cli` |
| macOS | the separate `filmagic-cli-<version>-macos-<arch>.zip` release asset (the `.app` holds only the desktop app) |

### Effect parameter replies

`effects.setParam` returns `{"keyframes": n, "param": id, "value": value}` on success.
The keyframe count is zero for a static parameter: this is a successful static edit,
not a failure. `param` identifies the edited parameter and `value` reports the accepted
value; a choice supplied by name is returned as its numeric option index. An animated
edit reports the value written at the requested time, rather than the static fallback.
For example, setting opacity `blend` to `"Screen"` returns
`{"keyframes": 0, "param": "blend", "value": 8}`.

With `clips` (a list of clip ids) instead of `clip`, `effects.setParam` sets the same value on every
listed clip as one undo step, and the reply adds `"clips": [{"clip", "keyframes", "value",
"mediaTime", "mediaSeconds"}]` (the media times where a keyframe was written). Each clip must
already have the effect; if one does not, or a value is refused, none of them changes. One timeline
`time` keys every clip at that moment: a clip that is not under it gets its key outside its range,
and `warnings` names each such clip and where its key landed.

`effects.addKeyframe` toggles: it returns `{"added": true, "mediaTime": t}` when it added a keyframe and
`{"added": false, "mediaTime": t}` when it removed the one already there; `mediaTime` is where the keyframe
is, to find it again. An explicit `time` keeps its place, outside the clip too, as in `effects.setParam`;
only the playhead (when no `time` is given) is held to the clip's last tick. `effects.deleteKeyframe`
returns `{"removed": true}` or `{"removed": false}` (no keyframe sat exactly at `mediaTime`).

Keyframes by timeline time: `effects.setKeyframe`, `effects.setInterpolation`, `effects.deleteKeyframe`
and `effects.moveKeyframe` take either `mediaTime` (the keyframe's source time, exact) or `time`
(timeline time, e.g. `{"seconds": 52.5}`; the keyframe nearest it within half a frame). The engine
turns timeline time into the clip's media time through its speed, reverse and frame hold, as
rendering does, and the reply says which keyframe it was (`mediaTime`, `mediaSeconds`). With Hold
Filters on, every timeline time of a held clip maps to the hold's media time, so `time` reaches only
the keyframe there; give `mediaTime` for the others.
`effects.setParam` takes `ease` (`linear`, `bezier`, `autoBezier`, `continuousBezier`, `hold`,
`easeIn`, `easeOut`, the names `effects.setInterpolation` takes) to set the value, its keyframe and
its easing in one call and one undo step:

```json
{"clip": 7, "param": "scale", "value": 130, "keyframe": true, "time": {"seconds": 52.5}, "ease": "easeOut"}
```

An unknown easing name is an error (it used to mean linear).

### Time values

Every parameter whose doc types it as ticks (`ticks`, `mediaTicks`, `timelineTicks`) takes the same
forms. An integer is ticks (254 016 000 000 per second), as it always was; an object gives the time
in another unit. The examples are all 52.5 s, the last two in a 24 fps time base:

| Form | Example | Counts in |
|---|---|---|
| ticks | `"time": 13335840000000` | ticks |
| seconds | `"time": {"seconds": 52.5}` | seconds |
| frame | `"time": {"frame": 1260}` | frames at the rate of the time base |
| timecode | `"time": {"timecode": "00:00:52:12"}` | `HH:MM:SS:FF` at the rate of the time base; `"+10"` / `"-00:00:01:00"` count from the playhead |

Parameters typed in seconds (`seconds`, `durationSeconds`, `fromSeconds`, …) or frames
(`durationFrames`, `deltaFrames`, …) keep their own unit and are not time objects.

The doc says which time base each value is in:

- `ticks`: sequence time, from the start of the sequence (00:00:00:00), at the sequence frame rate.
- `mediaTicks` (and `mediaTime`, `sourceIn`, `time` with `"target": "source"`, and `time` of
  `clip.frameHoldOptions` with `"holdOn": "sourceTimecode"`): source time, the media time of the
  clip or item the command acts on (the `item`, `items`, `clip` or `clips` it names, else what it
  falls back to: the selection, the Source Monitor's item), at that media's frame rate. A timecode
  is the media's own: its start timecode (10:00:00:00 on many cameras) is subtracted. A relative
  timecode counts from the Source Monitor's playhead when the item is open there, else from the
  media's first frame. This includes `timeline.place` `sourceIn`: a `{"timecode": …}` there now
  subtracts the media's start timecode. A frame or timecode for several `clips` at once must mean
  the same in each (same frame rate and start timecode); otherwise give seconds, or one call per
  clip.
- `delta` and `timelineTicks`: a signed offset of sequence time.
- `duration` and the graphics roll and responsive timings (`preroll`, `easeIn`, `intro`, …): a
  length of sequence time.

Commands that already took `seconds`, `frame` or `timecode` as separate keys (`playhead.set`,
`timeline.place`, `markers.markIn`, …) still do. Prefer the object form: a separate `seconds` key
means a duration in some commands (`graphics.newText`). A value the engine cannot use is an
error that names the parameter: a non-number, a number beyond the tick range (about ±420 days), a
negative absolute time or length, a fractional frame, a timecode that does not parse or is longer
than 64 characters, or a frame or timecode where there is no usable frame rate (give seconds
then).

`probe` (CLI) reports `durationSeconds`, `durationFrames` (at the video frame rate) and, per audio
stream, `durationSamples` next to `duration` in ticks; `project.inspect` reports `durationSeconds`,
`durationFrames` and `frameRate` for each item.

A graphic, title, still or caption that a command places or lengthens past the last picture frame
(`graphics.new*`, `graphics.template.apply`, `graphics.resetDuration`, `captions.add`,
`timeline.place`) is still placed, and the result says so. The picture is the sequence's footage:
other titles, stills and captions do not count. `warnings` gives the overrun in seconds and frames
and the call that fixes it: a `timeline.trim` (or `captions.trim`) that ends it with the picture,
or, for one that starts at or after the picture's end, a `timeline.delete` (or `captions.delete`).
`pastSequenceEnd` gives the same as data (`start`, `end`, `pictureEnd`, `over`, `overSeconds`,
`overFrames`, `whollyPast`, `fix`). These commands answer an object; the warning is added to it.
There is no trim option: an end card may run on past the last shot on purpose.

### Clip ids, linked clips and tracks

- **Clip ids are the `clip` numbers** of the items in `sequence_inspect` (or `sequence.clips`).
  Clips, links, groups, tracks, transitions, markers, captions and project items share one id
  counter, so the `link` value of a clip looks like a clip id but is not one. A command whose
  params doc declares `"clips":[id]` or `"clip":id` refuses an id that is no clip
  of the sequence it works on, before it changes anything, and says what the id is and what to pass
  instead: `53 is a link id, not a clip id; its linked clips are 52 (video, V1) and 54 (audio, A1).
  Pass those clip ids`. The same goes for group, track, caption-track, caption, transition, marker
  and project-item ids, clips of another sequence, and values that are not ids (`29.0` is refused
  with "pass 29": the commands read ids as whole numbers).
  - `clip` holds one id. A list is refused ("`clip` takes one id here"), except where a doc says
    `"clip":id|[id]`; use `clips:[...]` for several.
  - The sequence is the command's `sequence` where it documents one (`edit.label`), or the `item`
    of `sequence.inspect`, else the active one.
  - When a command reads `item` instead of `clip` (`metadata.*`, `clip.rename`, `tts.inspect`) and
    you pass `item`, `clip` is not read and not checked. Where `clip` is read first
    (`source.open`, `clip.makeSubclip`, `audio.speechSegments`), it is checked.
  - Only those declared keys are checked. Ids inside other values (`timeline.move`'s
    `moves[].clip`, `captions`, `transitions`) are checked by the command itself.
- **Linked clips.** While Linked Selection is on (the default; `sequence.linkedSelection`), a video
  clip brings its linked audio along: edits such as `edit.clear`, `edit.rippleDelete`,
  `clip.speedDuration`, `clip.enable` and `timeline.move` change both, and audio commands
  (`essentialSound.*`, `clip.audioGain`) take a video clip as its linked audio. Effect setters
  (`effects.setParam`) change exactly the clips you name.
- **One setting on many clips.** `effects.setParam` takes `clips:[...]` (above). To give several
  clips everything one clip has (Color Grading, Motion, Opacity, Volume…), select the source clip
  and run `edit.copy`, then `edit.pasteAttributes {"clips":[...]}`; `edit.removeAttributes` takes
  them off again. For one grade over a stretch of the film, `file.newAdjustmentLayer` makes an item
  to place on a track above the clips. `command_list` with a `filter` matches each word against the
  id, label, menu, params and `keywords` of every command, so `"many clips"` or `"colour grade"`
  finds these.
- **Graphic and template tracks.** `graphics.new*` and `graphics.template.apply` take `track` as
  `"V2"` (that track, which must be free over the new clip's range and unlocked), `"auto"` (the
  first free track above every clip in the new clip's range, else a new track), or a 0-based
  index as before. Without `track` the clip goes above the clips at the playhead.
- **New tracks follow the media.** `timeline.place` with `"track":"new"` adds a video track for the
  picture; for a file with sound only it adds an audio track, unless `audioTrack` says where the
  sound goes (`{"track":"new","audioTrack":"A1"}` puts the sound on A1 and adds no track).
- **A sequence that matches a clip.** `file.newSequence {"fromItem": id, "place": false}` takes the
  frame size, rate and audio settings of a media file (or of a subclip's file) and leaves the
  sequence empty; the default, `place: true`, also places the clip. `place:false` takes one item:
  with `fromItems` it is refused.
- **Finding commands.** `command_list` (and `filmagic-cli commands many clips`) ranks what matches:
  commands with every word in their id first, then in their label, then in their keywords.

### Tools

| Tool | Modes | Purpose |
|---|---|---|
| `command_list` | both | every command: id, label, menu, shortcut, params, keywords, enabled now (`filter`: every word must appear in one of those; `enabled_only`) |
| `command_run` | both | run a command `{id, params}`; edits are undoable |
| `command_batch` | both | run several commands in order `{steps: [{id, params}], stop_on_error}` → `{completed, failed, results}` |
| `doc_inspect` | both | the project tree and the active sequence in one call (`project_inspect` + `sequence_inspect`); takes the same filters as `sequence_inspect` |
| `render_preview` | both | a still: same as `render_frame` |
| `render_motion` | both | a few seconds of the sequence moving, for people: an animated GIF returned as an image, or (`format: "mp4"`) an MP4 file whose path is returned and which you delete (`from_seconds`, `to_seconds`, `fps`, `width` = the longest side; at most 10 s, 640 px, 60 GIF frames, 4 MB). To see motion yourself, use `story_board` with `fps`. Read-only |
| `project_inspect` | both | bins and items with ids, types, durations; active sequence |
| `sequence_inspect` | both | the active sequence: tracks, clips (`start` / `end` and `sourceIn` / `sourceOut` in ticks, frames, `speed`, `reverse`: the clip plays its `sourceIn` to `sourceOut` stretch backward, `gainDb`), effects, transitions, markers (name and `comment`), playhead, selection. Filters: `clip` (an id or a list), `track` (`"V1"`, `"A2"` or an id), `effect` (id or name: only clips with it, and only it); `compact: true` gives one short entry per clip as `sequence.clips` does, with `changed` listing only parameters that differ from their defaults |
| `media_import` | both | import files by absolute path (`paths`, an array; optional `bin`, `image_sequence`) |
| `render_frame` | both | PNG of the program frame at `seconds` (headless renders; bridge screenshots the Program monitor). Read-only: the playhead and selection are left as they were. Parallel bridge calls take turns, so each returns its own frame |
| `story_board` | both | PNG of one representative frame per shot of the active sequence, or of `count` frames across a project `item`'s rushes, with what each frame is as JSON (`item`, `count`, `max`, `from_seconds`, `to_seconds`, `columns`, `width`, `labels`). `fps` or `step` samples `from_seconds`..`to_seconds` densely instead (up to 96 frames: 12 fps for 8 s). An `item` board marks the cells after a cut inside its source (`\| CUT`, `cutBefore`, `sourceShot`). Read-only |
| `ui_inspect` | bridge | UI state: tool, workspace, panels, zoom, playback, fps |
| `ui_elements` | bridge | on-screen interactive elements with id, label and rect (`prefix` filter) |
| `ui_click` | bridge | click by `id` or `x`,`y`; `button`, `count`, `modifiers` |
| `ui_drag` | bridge | press–move–release between points or element ids |
| `ui_key` | bridge | key or shortcut: `Space`, `Cmd+K`, `Shift+Delete` |
| `ui_type` | bridge | type text into the focused field |
| `ui_screenshot` | bridge | PNG of the window or one `panel` |
| `ui_control` | bridge | call any control-channel method directly (`ui.set`, `ui.scroll`, `ui.timeline.locate`, …) |

Typical loop: `project_inspect` / `sequence_inspect` → get ids → `command_run` → `render_frame` or
`ui_screenshot` → look at the result → `edit.undo` if it's wrong.

To judge an edit and not only make one, and to work on part of a picture, see
[story-and-layers.md](story-and-layers.md): `story.rhythm`, `story.cuts`, `story.blinkPoints`,
`story.board`, `story.splitEdit`, `story.shadowCut`, `story.compare`, the `layers.*` commands and
`color.relight`. A loop that uses them: `story_board` on the rushes → `story.blinkPoints` for
where each clip could be cut → build the sequence (`sequence.clips` lists its clips in brief) →
`story.rhythm` and `story.cuts` → look at the cuts they call out → `story.shadowCut` before trying
another version, and `story.compare` to see shot by shot what the new one changed.

For sports and event footage, see [highlights-and-speed-ramps.md](highlights-and-speed-ramps.md).
`story.highlights` finds rallies, cheering, bursts of motion and scoreboard changes in long
single-camera footage, where scene detection finds nothing. It runs as a job, and lists every
impact it heard. `clip.speedRamp` makes a smooth speed change (100 % easing to 40 %) in one
command, as Time Remapping keyframes that the renderer plays.

Two habits make these reliable. Read `warnings` and `notes` in every result: they say what a
command could not do. And read `pictureChange` after a command that alters the picture: `subtle`
means a viewer will not notice it. A region read off a rendered frame takes `"space":"frame"`
when the clip is moved, zoomed or turned.

### Checking what you made

A still frame cannot show motion, and a rendered frame does not say what covers what. These close
the gaps:

- **Motion.** To see motion yourself, lay a moment out frame by frame: `story_board` with `fps`
  (or `step`), e.g. `{"fps": 12, "from_seconds": 31.0, "to_seconds": 33.0}` shows a door closing
  at 12 frames per second (up to 96 frames). `render_motion` makes a short GIF (or MP4) of a range
  for a person to watch: many vision models see only the first frame of an animated GIF. It is
  capped at 10 s, 640 px on the longest side, 60 GIF frames (the frame rate is lowered to fit) and
  4 MB; an MP4 stays where it was written until you delete it. It renders before it answers
  (seconds; up to about 30 s for 10 s at 640 px, longer in bridge mode), so keep the range short.
- **Cuts inside a source file.** Long recordings and compilations hold shot changes of their own.
  Finding them decodes every frame, which on long-GOP H.264 costs tens of milliseconds a frame, so
  it runs in the background. `media.sourceCuts {item|clip, fromSeconds?, toSeconds?}` lists what is
  known (media seconds, and timeline seconds for a `clip`) and queues the rest for the session's one
  background checker (`job`; follow it with `jobs.list`, stop it with `jobs.cancel`). Every edit and
  call adds to the same queue and the same job, and the job leaves the list once it has finished:
  call `media.sourceCuts` again then. `wait: true` checks before answering, which can take minutes
  (the web build has no background checker and needs it). A call checks at most 7200 frames; `next`
  says where to continue. The status bar shows the checker only when no export or other job runs. After `timeline.razor`, `sequence.addEdit`,
  `sequence.addEditAllTracks`, `clip.speedDuration`, `timeline.rateStretch`, `clip.frameHold`,
  `clip.frameHoldOptions`, `clip.insertFrameHoldSegment`, `clip.speedRamp` or a Time Remapping change, the result's
  `warnings` say when the edit lands within 3 frames of a known cut, or the retimed clip straddles
  one, with the cut's time (`sourceCuts` has the details; on a ramped clip the timeline time is
  where the ramp plays the cut). An edit never waits for a decode: where the cuts are not known
  yet, `notes` says a background check started, and the next edit, or `media.sourceCuts`, reports
  them. A file that is relinked or changes on disk is looked at afresh. The detection is Scene Edit
  Detection's measure at its default sensitivity. A good habit: run `media.sourceCuts {item}` on
  long rushes when you import them.
- **Layout.** `layout.check {fromSeconds?, toSeconds?, clip?}` measures every graphic, title and
  caption in the range from the picture (the frame rendered with it alone) and reports overlaps,
  boxes outside the title-safe or action-safe area or off the frame, and overlays over busy or
  text-like picture (`under.reads`: `calm`, `busy`, `textLike`). The safe areas are Project
  Settings ▸ General's: by default title safe 20 % and action safe 10 %, the per cent of the
  frame's width and height left as a margin, half on each side (on a 1920×1080 frame title safe is
  192–1728 × 108–972). The busy-picture test is a **heuristic**: the share of strong luma edges
  under the overlay (`edgeDensity`; busy from 0.06, text-like from 0.10 with a luma spread
  `contrast` of 0.12 or more). It finds sharp, high-contrast detail such as a burned-in scoreboard,
  not meaning: a striped shirt reads busy too, and soft text can be missed, so look at the frame.
  An overlay that spans the frame's width or height (a bar, a border) gets a note, not a warning;
  captions outside a safe area are reported once for all of them. Transitions are left out of the
  measurement, so a dip to black does not make a title fill the frame.
- **Placing graphics.** `graphics.anchor {clip, anchor, safe="title", margin=0}` moves a graphic's
  layers together to `topLeft`, `top`, `topRight`, `left`, `center`, `right`, `bottomLeft`, `bottom`
  or `bottomRight` of the title-safe area, the action-safe area or the frame (`safe: "none"` is the
  frame too). It moves the layers where they are at the playhead: an animated Position gets a
  keyframe there. It refuses a clip whose Motion moves, scales or turns it (reset Motion first),
  and warns when the graphic's canvas is not the sequence's size. `graphics.template.apply` takes
  the same `anchor`, `safe` and `margin`.
- **Does it show?** `graphics.set` returns what it stored (`accepted`, values as you write them),
  `pictureChange` (how much this call changed the picture) and `layerShows` (the frame with the
  layer against the frame without it). A black band at 50 % opacity over dark footage reads
  `subtle` or `none` in `layerShows`: raise its opacity or change its colour. The measurement
  renders a few frames; `pictureChange: false` skips it (the app's own panels do).
- **Reading a sequence.** `sequence_inspect` with `compact: true`, or a `clip`, `track` or `effect`
  filter, is a fraction of the full answer. In compact form `changed` lists, per effect in the
  clip's order (`effect`, `index`, `params`), only the parameters that differ from their defaults. `effects.list {"effect": "Gaussian Blur"}` gives one
  effect's parameters with their ids, types, ranges, units and defaults.

### Conventions

The server follows a few conventions that make it predictable for agents. They are not
Filmagic-specific: the sibling craft apps' MCP servers can follow the same ones, so an agent that has
driven one app can drive the others.

- **Core tools.** `command_list`, `command_run`, `command_batch`, `doc_inspect` and `render_preview`
  work in both modes. The Filmagic tools above stay as they are.
- **Titles and annotations.** Every tool has a `title` and the four MCP hints (`readOnlyHint`,
  `destructiveHint`, `idempotentHint`, `openWorldHint`), so a client can let the read-only tools
  (lists, inspects, renders, screenshots) run without asking and ask before edits. When you add a
  tool, set them in its `#[tool(...)]` attribute; a test checks every listed tool.
- **Strict arguments.** An unknown tool argument is a JSON-RPC `-32602` error that names it and the
  accepted ones (`unknown argument "filtr" for command_list; expected: enabled_only, filter`), not a
  silently ignored key. `command_run.params` goes to the command unchanged.
- **Errors.** A failing command is an `isError` result with the engine's message. A line that is not
  JSON gets a `-32700` parse error (id `null`) and the server keeps serving. A command that panics is
  reported as `internal error: …`, and the session stays usable.
- **Resources.** `filmagic://document` (the project tree and active sequence, as `doc_inspect`) and
  `filmagic://commands` (the command catalog, as `command_list`), both `application/json`. For MCP
  2026-07-28 clients such as current Claude Code, list and read results carry the `ttlMs` and
  `cacheScope` hints that revision requires.

### Long exports: progress and cancellation

`command_run {"id": "file.exportMedia", "params": {…, "wait": true}}` in headless mode blocks until
the file is written. It uses the MCP progress and cancellation utilities, so a client can show
progress and stop it:

- The export runs as the same background job `jobs.list` shows. The session is locked only for short
  polls, so other requests (`ping`, `doc_inspect`, `jobs.list`, …) are answered while it encodes.
- `file.exportMedia` with `wait: true`, an export job in `jobs.list` and each `export.queue.list`
  item carry `offlineMedia`: the offline items the export drew the placeholder for (see §2b), `[]`
  when none. The export itself still succeeds; deciding what to do about it is up to the caller.
- Every job in `jobs.list`, and every encoding item of `export.queue.list`, carries `etaSeconds`: the
  time left at the job's speed over the last 15 seconds, measured between the batches of frames it
  finishes and counting down in between, `null` until a second of progress has been measured (a
  loudness pass, a seek) and once it is done. The app shows it as `37% · 2:05 left` in the status
  bar, the Export queue and the Progress panel, each refreshed once a second.
- With `"_meta": {"progressToken": T}` on the `tools/call`, the server sends
  `notifications/progress` `{progressToken: T, progress: <frames done>, total: <frames>, message:
  <job status>}` at most every 100 ms while frames advance. Without a token it sends none.
- `notifications/cancelled` for the request stops the encode at the next batch (`jobs.cancel`),
  deletes the partial output (the movie, or the frames and caption sidecar written so far, and no
  other export's files beside them) and sends no response, as the MCP cancellation utility asks.
- Without `wait` the command returns `{job, path, poll: "jobs.list", cancel: "jobs.cancel"}` at
  once: poll `jobs.list` for the job's `progress`, `etaSeconds`, `finished` and `result`, and stop it
  with `jobs.cancel` (`export.queue.list` lists the Export Queue only, not direct exports, #1544). Other exports (`file.exportFrame`, interchange formats, …) finish quickly and
  ignore the token.
- **Client timeouts.** An MCP client gives up on a tool call after its own timeout and cancels the
  request, which stops a `wait: true` export and deletes the partial file. Codex waits 300 seconds by
  default (`tool_timeout_sec` under `[mcp_servers.<name>]` in `~/.codex/config.toml`), and Claude
  Code waits `MCP_TOOL_TIMEOUT` milliseconds (an environment variable set when `claude` starts). A
  whole film can take longer than that to encode, so for anything but a short range call
  `file.exportMedia` without `wait` and poll `jobs.list` until the job is `finished`, or raise the
  client's timeout.
- An image sequence (PNG, TIFF, DPX, ... stills) never writes the path it is given: `a.png` becomes
  `a000.png`, `a001.png` and so on. `file.exportMedia`, `export.quick` and the job's `result` then
  name the first still as `path`, the name `file.import` takes for the sequence, and the two commands
  add `lastPath`, the last still, and `basePath`, the name the stills are numbered from (a cancelled
  long export's clean-up removes only the stills numbered from `basePath` with the digits of
  `lastPath`, up to it). `filmagic-cli export` prints the first and the last.
- `file.exportMedia`, `export.quick`, `export.resolve` and `export.queue.add` also return `warnings` (an array of
  strings, empty when none). If the default or explicitly selected In/Out range covers one
  sequence frame and at least one mark is set, the warning suggests `range: "entire"` (#3364).
  The export still succeeds, including with `wait: true` over MCP or the bridge. Explicit custom,
  work-area and entire-sequence ranges are unchanged and do not warn. Queue additions collect
  warnings from the newly added ranges. `filmagic-cli export` (including `--queue`)
  also prints the warning on stderr and suggests `--range entire`; the warning does not change
  the exit status. Frame counts here use the sequence rate, even when the export rate differs.
- Bridge mode works the same way: the export runs as an app job (the app stays responsive and shows
  "Exporting… NN%" in its status bar) and the call returns when it is written, however long it
  takes. `filmagic-cli --bridge … exec file.exportMedia … wait=true` blocks the same way.

### Vietnamese narration, transcription languages and the models directory

**Where models live.** Speech models, natural voices and VieNeu-TTS all go in one directory:
`FILMAGIC_MODELS_DIR` if set, else the setting `mediaAnalysis.modelsLocation` (an absolute folder),
else `<data dir>/models` (`%APPDATA%\Filmagic\models` on Windows). `models.info` says which one is
used and what it holds. To move them (a full disk, a second drive), call `models.setLocation
{"dir": "D:\\FilmagicModels"}`. It moves only the folders Filmagic put there (Whisper models,
`kokoro-82m`, `vieneu`), refuses if the folder holds anything else, and never overwrites. If a step
fails before the copy is complete, it puts everything back; once the copy is complete, it switches
and reports any original it could not delete. It needs a session that saves its settings (the
desktop app): `filmagic-cli` and headless MCP sessions set `FILMAGIC_MODELS_DIR` instead.
`"move": false` only switches. Headless sessions do not read the app's settings either. Unless
`FILMAGIC_MODELS_DIR` is set, they use the default folder, so set it the same way for the app and
for `filmagic-cli mcp` to share one set of models. Setting `mediaAnalysis.modelsLocation` with
`prefs.set` does not move anything either. In both cases `models.info`, `tts.voices` and
`transcript.models` warn, in `warnings`, about models left at the old place.

**Transcription languages.** `transcript.generate` and `sequence.transcribe` take `language`:
`"auto"`, or any Whisper code (`"vi"`, `"en"`, `"ja"`, …; `vi-VN` means `vi`). `transcript.models`
lists them in `languages`. An unknown code is an error, and nothing is transcribed. For Vietnamese,
download `whisper-large-v3-turbo` (1.6 GB, about 5 GB of memory) with `transcript.downloadModel
{"model": "whisper-large-v3-turbo"}` and pass `"model": "whisper-large-v3-turbo"`. The smaller
models also know Vietnamese, but they make more mistakes.

**Natural voices one by one.** `tts.downloadVoices {"voices": ["kokoro-heart"]}` downloads the shared
Kokoro model (331 MB, needed by every natural voice) and that voice (0.5 MB). `tts.voices` lists
`package.perVoice` with what each voice still needs.

**Vietnamese voices (VieNeu-TTS).** VieNeu-TTS is an optional external engine: Filmagic never links,
bundles or ships it. VieNeu exists only as a Python package, so Filmagic runs a Python interpreter that
has `vieneu` installed, as a subprocess, through a small bridge script. The owner approved this bridge
for VieNeu (#20). Python is found from, in order:
1. `FILMAGIC_VIENEU_PYTHON`, set by a person when starting Filmagic;
2. Filmagic's own environment, `venv/` in its VieNeu engine directory: `%LOCALAPPDATA%\Filmagic\vieneu`
   on Windows, `<data dir>/vieneu` elsewhere;
3. `py -3` or `python` (Windows), `python3` or `python` (elsewhere).

No command parameter or setting can name a program to run: that would let a prompt-injected agent
run anything. The environment and the bridge, the code that runs, are kept out of the models folder,
which an agent can move. The models folder holds VieNeu's data only: `vieneu/cache` (the downloaded
model) and `vieneu/voices.json`. The child process gets none of Filmagic's `FILMAGIC_*` variables
(the control token among them) and no `PYTHON*` variables. Every Python runs isolated (`-I`), so
nothing beside the bridge or in the working directory is ever imported, in an empty working directory.
The models folder may not overlap the engine directory (neither inside the other). Symbolic links
are resolved as far as the folders exist, and on Windows and macOS letter case is ignored, as those
file systems do, so the check also holds before the engine directory exists. This is checked by `prefs.set`, by `models.setLocation` and
again before every run; `prefs.set` refuses the media cache folders (`mediaCache.location`,
`mediaCache.databaseLocation`) there too. Nothing an agent names is written inside the engine
directory either. These are refused there: narrations (`tts.create` and `tts.edit` with `dir`),
`file.exportMedia` and the other exports (the output file and its folder, which also holds OP-Atom's
audio files), AAF and OMF media, project saves, `file.projectManager`'s destination, Scratch Disks
folders, `project.ingestSettings` and ingest copies, `media.createProxies` and transcodes, `assets.buildPack`'s
folder, captions, markers, presets and `lut.export`. Filmagic runs its environment only if it created it: an existing
`venv` folder without Filmagic's marker is reported by `tts.engines` and refused by
`tts.setupVieneu`, with the advice to delete it. To install by hand, make an environment in a folder
of your own and name its interpreter with `FILMAGIC_VIENEU_PYTHON` (`installSteps` shows how).

1. `tts.engines` reports whether a Python with `vieneu` was found, what was tried, and
   `installSteps` with the exact commands for this machine.
2. `tts.setupVieneu {"install": true}` (a job: follow it with `jobs.list`) does the setup. It creates
   the environment, runs `pip install vieneu==3.8.4` there (about 0.9 GB; it runs every time, so an
   install that was stopped gets finished), downloads the voice model (about 0.6 GB) into the models
   folder, and caches the voice list. After that, VieNeu runs offline. Without `install`, it only
   checks the existing installation and downloads the model.
3. `tts.voices` then lists `vieneu:<name>` voices (`"language": "vi-VN"`, `"engine": "vieneu"`), for
   example `vieneu:Hải Đăng`. Listing them never starts Python. Names typed in decomposed Unicode
   (NFD) match too.
4. VieNeu loads its model on each call and speaks at about real time on a CPU, so the speaking runs
   in a job. First run `tts.render {"text": "Xin chào…", "voice": "vieneu:Hải Đăng"}` and follow it
   with `jobs.list`. Then `tts.create` (or `tts.edit`, `tts.preview`) with the same settings returns
   at once. Called before the render, they refuse and say so. Pause markers, `pace` and `pitch` work
   as with the other voices. VieNeu is generative, so a take whose length does not fit the text is
   generated again, up to three takes. Output is 48 kHz (24 kHz with `VIENEU_MODE=v3nano`, the
   smaller model for weak CPUs).
5. Cloning: `"voice": "vieneu-clone", "referenceAudio": "<3–30 s clip>", "confirmAuthorizedVoice":
   true`. That flag is set by the agent: it is the agent's statement that the person whose voice is in
   the clip agreed, not a check by a person. Pass it only when that is true. Without it, the call is
   refused. The clip's absolute path is stored with the narration, so `tts.edit` regenerates in the
   same voice. That path is also in the project file, so sharing the project shares where the clip is
   on this computer.

Errors name the next step: VieNeu missing, an unknown `vieneu:` name (with the list of voices), a
stuck engine (`jobs.cancel` stops it). VieNeu returns audio only, with no word timings. For captions,
run `transcript.generate {"language": "vi"}` on the narration's media, then
`transcript.createCaptions`.

### Assets: music, sound effects, ambience and memes

Filmagic ships no media. Music beds, sound effects, ambience, meme pictures, GIFs and videos, and
overlays come as **asset packs**: folders installed under `<data dir>/Packs/<id>/` (headless: the
`--data-dir` given to `filmagic-cli`, else the app's data directory), each with a `pack.json`
manifest, and one `Packs/index.json` across them. A pack is removed with one command and nothing
of it is left in a project or in this repository. Use packs rather than synthesising sound.

| Command | Does |
|---|---|
| `assets.catalog` | the packs Filmagic can download (licence checked, pinned by SHA-256), and which are installed |
| `assets.install` | `{"catalog": id}`, `{"path": folder \| file.zip \| pack.json}`, or `{"url": …, "sha256": …}`; `replace: true` updates an installed pack. A download (`catalog`, `url`) runs as a job: poll `jobs.list` until it is finished. With `wait: true`, the MCP server and the bridge poll the job for you and reply when it is done (the app keeps responding); the CLI always waits; the app's control channel itself never waits |
| `assets.fetch` | `{"asset": "pack/item"}` or `{"pack": id}`: download items a manifest lists by URL but that are not on disk yet, as a job |
| `assets.list` | installed packs (`{"pack": id}`: that pack's items) |
| `assets.search` | `query` (words; accents optional: `nhac` finds `Nhạc`), `kind`, `mood`, `pack`, `alpha`, `minSeconds`, `maxSeconds`, `limit` |
| `assets.place` | `{"asset": …, "time": {"seconds": 12}}` puts it on the timeline (below); copy `asset` from a search result |
| `assets.buildPack` | `{"from": folder}` makes a pack from a folder of your own files |
| `assets.remove` | `{"pack": id}` |

Kinds: `music`, `sfx`, `ambience`, `meme-image`, `meme-gif`, `meme-video`, `overlay`. Each item
carries `tags`, `mood`, `duration` (s), `loudness` (LUFS), `alpha`, `keyColor` (the colour of a
green or blue screen), and its pack's `license`, `source` and `attribution`.

`assets.place` imports the file (once, however often it is placed) and, as one undo step:

- puts it at `time` (ticks or a time object such as `{"seconds": 12}`, see
  [Time values](#time-values); `seconds`, `frame` and `timecode` work too; default the playhead) on a free track of
  its kind: pictures above V1 so they land over the footage, sound on the first audio track with
  nothing there, a new track when none is free (or name one: `track`, `audioTrack`, `"new"`);
- `duration` (ticks or a time object) cuts it to length; `untilEnd: true` runs music to the
  end of the sequence;
- keys out the green screen of an item with a `keyColor` (Ultra Key; `key: false` skips it,
  `keyColor` overrides it); a transparent picture is left as it is;
- sets the Essential Sound type (Music, SFX, Ambience) and ducks music and ambience under the
  speech of Dialogue clips (`essentialSound.generateDucking`, see
  [Audio](#audio-speech-loudness-and-the-mix); `duck: false` skips it). Mark speech with
  `essentialSound.setType {"type": "dialogue"}` first; when nothing ducked, the result's `notes`
  say why and what to run.

An item's id is made from its whole path in the pack (`memes/green screen/bruh.png` is
`memes-green-screen-bruh`), so copy `asset` from a search result rather than guessing it.

A meme at 12 s: `assets.search {"query": "bruh", "kind": ["meme-video", "meme-image"]}` →
`assets.place {"asset": "my-memes/memes-green-screen-bruh", "time": {"seconds": 12}, "duration":
{"seconds": 2}}`. A hit on a cut, from the default catalogue: `assets.install {"catalog":
"kenney-sci-fi-sounds"}` → `assets.search {"query": "impact"}` → `assets.place` a few frames before
the cut. The default packs hold UI sounds (clicks, toggles, errors) and sci-fi effects (impacts,
explosions, lasers, engines), but no whooshes or music beds: those come from packs you build or
install.

**Your own memes and sounds.** Put the files in a folder; subfolders and file names become kinds
and tags (`music/`, `sfx/`, `ambience/`, `overlays/`, `memes/green screen/bruh.mp4`), and words like
`epic`, `funny`, `sad` become moods. Then:

1. `assets.buildPack {"from": "D:/Memes"}`: probes every file (duration, loudness, transparency, a
   green or blue screen), tags it, and installs a copy as the pack `memes` (`id` to name it).
2. `assets.search` and `assets.place` as above.

To review the tags first, pass `"install": false`: the manifest is written to the folder as
`pack.json`. Edit kinds, tags or moods there, then `assets.install {"path": "D:/Memes"}`. Building
the folder again keeps what you edited. The licence is recorded as `LicenseRef-Own-Files` unless
you pass `license`; publish only what you may. An archive downloaded from a `url` without a
`pack.json` is recorded as `LicenseRef-Unknown`, with a warning, unless you pass `license`. When a
pack has its own `pack.json`, its name and licence are the manifest's: `name` and `license`
parameters are not used (a note says so).

**Light installs.** A `pack.json` may list items with their own `url`, `sha256` and `size`.
Installing such a manifest (`{"path": "pack.json"}`, or a `url` that serves one) downloads
nothing. `assets.fetch` downloads the items you need, each checked against its SHA-256;
`assets.place` of an item not downloaded yet says to fetch it first (a download never runs on the
app's UI thread).

**Untrusted input.** Archives, manifests and downloads are checked before anything is
installed: entry names must stay inside the pack on every operating system (no `..`, absolute
paths, drive letters, Windows device names), only media, the manifest and licence text are kept,
sizes and counts are capped (20 000 entries, 2 GiB a file, 4 GiB a pack, counted on the bytes
really unpacked), and a pack is staged and renamed into place, so it is either installed whole or
not at all; a pack an interrupted update had moved aside comes back. Downloads must match their
SHA-256, use HTTPS all the way to a public host, and are given up when the server sends nothing
for 60 seconds; `jobs.cancel` stops one. A URL, and every redirect it leads through, is refused
when its host is written as an address on this machine or a private network (in any spelling:
`127.1`, `0x7f000001`, `[::1]`, …); a name that resolves to such an address is not detected. Builds without the `pack-download` feature cannot download; the error says how to
install the file by `path` instead.

### Export speed: review with drafts, export the final once (#18)

A 1080p H.264 export runs at about 18 fps for a plain clip and about 10 fps for a cut sequence with
dissolves, grades and a title on a 4-core machine (`cargo run --release -p filmagic-engine --example
bench_export -- --scenario edit`). Most of the time goes to Filmagic's own H.264 encoder. To spend
less of it:

- **Review with a draft.** `file.exportMedia {"path": "review.mp4", "draft": true}` (or the built-in
  preset `Draft – Half Size Review`) writes H.264 at half the sequence's size with the encoder's
  `speed` preset, constant quality CRF 30 and AAC 128 kbps: 2.6–4× faster than the default export.
  With `hardwareEncoding: "auto"` the draft uses a low VBR instead (hardware encoders take no CRF),
  so it can be encoded by NVENC or VideoToolbox. What the call names itself (`scale`,
  `width`/`height`, `crf`, `bitrateMode`, `bitrateKbps`, `h264Preset`) wins over the draft; with a
  sized preset (`YouTube 1080p Full HD`) the draft halves that size. The result says `"draft": true`
  and has a note. Check frames, cuts, titles and sound in the draft (or with `render_preview` /
  `file.exportFrame` for single frames), and export the final **once**, without `draft`. `draft`
  with a `format` other than `h264` is refused.
- **Re-export after a small fix with `smartRender: true`.** The H.264 stream is then written in
  independent segments of one keyframe interval (2 seconds at the default keyframe distance; fewer
  frames at 4K, where a segment is capped at 192 MB of pictures), and a segment whose pictures are
  all the same as in an earlier smart export is copied from the cache instead of encoded. Every
  frame is still rendered (comparing the pictures is how a change is found, so nothing can be
  missed), so a re-export saves the encoding of the unchanged segments: 2.7× faster for plain
  footage, 1.6× for a sequence with a title and grades on every frame. Know what it changes:
  - The file is byte for byte the one a smart export of the same edit with an empty cache writes,
    but **not** the one a normal export writes. Every segment starts its rate control again, so
    file size and quality per segment differ (on the demo project a smart file was 24 % smaller
    than a normal one, its busiest segments with up to 40 % fewer bits), and there is a keyframe
    at every segment. With `bitrateMode` `cbr` or `vbr1Pass` the rate is held per segment, not
    across the file, and the result's `notes` say so; `crf` suits smart render best.
  - A change that moves pictures in time (a ripple delete, a trim, an inserted clip) moves every
    later picture into another segment, so everything after it is encoded again. A change that
    keeps the timing (a caption, a grade, a title) re-encodes only the segments it touches.
  - It needs Filmagic's own encoder: `format: "h264"` in MP4 or MOV, one pass, 8-bit,
    `hardwareEncoding` off; otherwise the export runs as usual and its `notes` say so.
  - The cache is the user's own cache directory (`%LOCALAPPDATA%\Filmagic\smart-render` on
    Windows, `~/Library/Caches/Filmagic/smart-render` on macOS, `$XDG_CACHE_HOME` or
    `~/.cache/filmagic/smart-render` elsewhere; `settings.smartRenderDir` for another), at most
    4 GB, least recently used segments removed first. On Unix a directory that belongs to another
    user or that others can write to is not used (the result says so).
- **Hardware.** `gpuRendering: "auto"` composites frames on the GPU (2.4× faster on an effects-heavy
  1080p edit where a GPU is present; off by default because its pixels differ slightly by GPU).
  `hardwareEncoding: "auto"` uses NVENC (Windows and Linux, NVIDIA GPUs) or VideoToolbox (macOS) for
  H.264 when it takes the settings. Windows has no other hardware H.264 encoder in Filmagic: Media
  Foundation is used for decoding only, so without an NVIDIA GPU `auto` falls back to the software
  encoder. Both settings fall back cleanly, and the result says what happened.
- **Read the result.** A `wait: true` export (and the job's `result` in `jobs.list`) carries
  `encoder`: `name` (`Filmagic H.264 (software)`, `NVENC H.264`, …), `hardware`,
  `hardwareRequested`, `hardwareDeclined` (each hardware encoder's reason for not taking the
  export), `gpuRequested`, `gpuFrames` / `cpuFrames`, and with smart render `segmentsReused` /
  `segmentsEncoded`; `stages`, the seconds spent in each stage (`render`, `encode`, `convert`,
  `audio`, `mux`, `wait`: the time the encoder waited for frames), summed over the threads that ran
  it and over the exports running at the time, so they show where the time went rather than adding
  up to `seconds`; and `notes`, which say what held the export back and what to try.

### Audio: speech, loudness and the mix

These need no transcript model and no ffmpeg: measure inside Filmagic instead of on an exported file.
Times are sequence seconds (source seconds for an `item`), rounded to 1 ms.

- **`audio.speechSegments`** finds where someone speaks: `{"clip": id}` (a video clip is read
  through its linked audio clip), `{"item": id, "stream": n?}` (source seconds) or a range
  `{"startSeconds", "endSeconds", "track": "A1"?}` of the mix (at most 600 s per call). It returns
  `segments` (`start`, `end`, `confidence` 0–1), the `pauses` between them and `speechSeconds`.
  The detector is light and model-free: voicing of a 70–400 Hz pitch, a level threshold from the
  clip's own noise floor (or, for a clip with no pause, from its speech level), and the shape of
  the level: syllables rise and fall 1–12 times a second. Car doors (bursts and low thunks),
  footsteps, plucked or struck notes, steady tones, hum and a steady pad under the words are not
  speech. Limits: whispering is never found (it is not voiced), and a sung melody or a soft-attack
  synth bass at about 4 notes a second can read as speech (Music-typed clips are never checked at a
  cut).
  With music under the words, a segment's edges can stretch by up to about a second, so prefer a
  Dialogue `clip` or `track` over the full mix. `bridgeSeconds` (default 0.25) sets how long a
  pause stays inside one segment; `minConfidence` filters.
- **Cuts inside a sentence warn.** `timeline.razor`, `sequence.addEdit` and `timeline.trim` add
  `warnings` and `speechCuts` when a cut or a trimmed edge lands inside speech on a Dialogue or
  untyped audio clip (a linked pair warns once). Each entry has the `segment`, `pauseBefore` and
  `pauseAfter` (where to cut instead: the middle of the pause, or 0.1 s before or after the speech
  when the pause runs past the 3 s looked at; `null` when the speech runs on beyond that) and
  `pauseBeforeRange` / `pauseAfterRange`. The edit is still made: `edit.undo` and cut there. The
  check reads the source media as recorded (before clip effects), at its own sample rate, and is
  cached, so it stays quick. It is skipped where the clip is not heard: disabled, on a muted track
  (or one another track's solo silences), inside a transition, or with Volume at or under -40 dB at
  the cut. It uses the channels the clip plays. On a clip sped past 1×, the 8 s of source it reads
  can cover less than ±3 s of the timeline, so a pause further away reads `null`.
  `speechCheck: false` skips it. The desktop app skips it for its own clicks and keys, and runs it
  for commands that come over the control channel.
- **`essentialSound.autoMatch`** matches each clip to the target of its own type. Dialogue clips
  are measured only where they speak (`measuredOver: "speech"`), and a Dialogue clip with no speech
  is skipped with a warning (it is likely an effect: set its type). `target` applies to Dialogue
  only (a note says when other clips used their own target); `targets: {"music": -28, …}` sets the
  others (else Preferences ▸ Audio). The gain change is capped at `maxGainDb` (default ±12 dB, up
  to 48); a capped clip has `capped: true`, `wantedGainDb` and a warning. The Essential Sound
  panel shows the first warning in the status bar.
- **`mix.check`** (`startSeconds`, `endSeconds`, at most 600 s per call; `targetDb: [min, max]`,
  default `[15, 20]`) reports `integratedLufs`, `maxShortTermLufs`, `truePeakDbtp` and `lraLu` for
  the mix, for each audio track (through its fader and the master) and for each Essential Sound
  role. Dialogue is also measured over its speech only (`roles.Dialogue.speech`), so room tone does
  not skew it, and music where the dialogue speaks (`roles.Music.underSpeech`).
  `musicUnderDialogue` gives `differenceDb`, the `verdict` (`ok`, `musicTooLoud`, `musicTooQuiet`,
  `unknown`) and `musicGainChangeDb`, the change that brings the music to the edge of the range.
  Solo is ignored (a note says so); a 5.1 mix is measured as its BS.775 stereo downmix. Roles need
  types (`essentialSound.setType`).
- **`essentialSound.generateDucking`** takes `depthDb` (0–40, stored as the clip's ducking
  amount) and ducks under detected speech when every trigger is a Dialogue clip (`by: "speech"`;
  the union of every Dialogue clip's speech), so noise in the dialogue track does not hold the
  music down; `by: "level"` uses the level detector. A loop: `mix.check` → for `musicTooLoud`,
  `essentialSound.generateDucking {"depthDb": D}` with D the depth the warning suggests (the middle
  of the range minus `differenceDb`, plus any ducking the music already has) → `mix.check`. For
  `musicTooQuiet`, raise the music (or duck it less), never duck it more.
- **Export peaks.** Without a loudness target the export runs a True-Peak Limiter at -1 dBTP. It
  is on by default except where 32-bit float PCM is written (WAV, AIFF, PCM in a movie), which
  keeps peaks above full scale on purpose (a float setting with AAC, MP3 or FLAC keeps the limiter);
  `truePeakLimiter: true|false` on `file.exportMedia` (or `effects.peak_limiter.enabled` in the
  settings) sets it either way. Loudness Normalization has its own limiter and the two never stack.
  An export that never comes within 0.1 dB of the ceiling (where the limiter aims) is written bit
  for bit; after an over, the limiter
  returns to exactly unity. Internal renders (proxies, transcodes and Project Manager copies,
  AAF/OMF clip audio) never limit. Every export with audio reports `result.audio`:
  `truePeakDbtp` and `integratedLufs` of what was written (for AAC and MP3, of the encoder's input),
  `limiter` (`truePeak`, `loudness`, `off`), `ceilingDbtp` and `limitedDb` (the deepest gain
  reduction, including a 0.1 dB margin under the ceiling). `result.warnings` flags a true peak above
  -1 dBTP (whichever limiter, or none, ran) and more than 6 dB of True-Peak limiting; with
  `wait: true` they are merged into `warnings`.

## 2. Control channel

`filmagic --control 9876` (or `FILMAGIC_CONTROL_PORT=9876`) listens on `127.0.0.1` only. Send one
JSON request per line and get one JSON reply per line. Set a fresh `FILMAGIC_CONTROL_TOKEN` for the
desktop session as above; the CLI / MCP bridge must inherit the same variable. Raw requests must
include it as `token` on every line (replace the placeholders below). Full setup and method table:
[control-protocol.md](control-protocol.md).

```jsonc
{"id":1,"token":"<FILMAGIC_CONTROL_TOKEN>","method":"engine.commands"}
{"id":2,"token":"<FILMAGIC_CONTROL_TOKEN>","method":"engine.execute","params":{"command":"file.openDemoProject"}}
{"id":3,"token":"<FILMAGIC_CONTROL_TOKEN>","method":"engine.execute","params":{"command":"playhead.set","params":{"seconds":2}}}
{"id":4,"token":"<FILMAGIC_CONTROL_TOKEN>","method":"engine.execute","params":{"command":"sequence.addEdit"}}
{"id":5,"token":"<FILMAGIC_CONTROL_TOKEN>","method":"engine.execute","params":{"command":"effects.apply","params":{"effect":"Gaussian Blur"}}}
{"id":6,"token":"<FILMAGIC_CONTROL_TOKEN>","method":"ui.elements","params":{"prefix":"tools."}}
{"id":7,"token":"<FILMAGIC_CONTROL_TOKEN>","method":"ui.click","params":{"id":"tools.Razor"}}
{"id":8,"token":"<FILMAGIC_CONTROL_TOKEN>","method":"ui.key","params":{"key":"Cmd+Z"}}
{"id":9,"token":"<FILMAGIC_CONTROL_TOKEN>","method":"ui.screenshot","params":{"path":"/tmp/timeline.png","panel":"Timeline"}}
```

Replies look like `{"id":4,"ok":true,"result":{"cuts":2}}` or `{"id":7,"ok":false,"error":"…"}`.
Minimal client:

```python
import json, os, socket
s = socket.create_connection(("127.0.0.1", 9876)); f = s.makefile("rw")
def call(method, **params):
    f.write(json.dumps({"id": 1, "token": os.environ["FILMAGIC_CONTROL_TOKEN"], "method": method, "params": params}) + "\n"); f.flush()
    return json.loads(f.readline())
print(call("engine.execute", command="sequence.inspect"))
```

Notes:

- Time values: see [Time values](#time-values) above.
- A track parameter (`track`, `audioTrack`) is a track id or a name such as `"V1"` / `"A2"`.
  Leaving it out picks the command's default track; naming a track the sequence does not have is
  an "invalid parameters" error, never a different track.
  In `timeline.place`, `track` is where the picture goes and `audioTrack` where the sound goes;
  an audio track given as `track` places the sound alone, on that track.
  The clip lands on the frame grid: its start and its end (`time + duration`) each snap to the
  nearest frame.
- `sequence.inspect` reports `durationFrames` as the number of frames export renders: a sequence
  that ends partway through a frame counts that frame.
- `engine.execute` also runs UI-only commands (`tool.razor`, `playback.toggle`,
  `window.workspace.color`, `window.panel.<name>`).
- A command that works on the selection is disabled ("no clips selected", "select a clip in the
  Project panel") until something is selected, unless the call names its targets under a key the
  command documents (`clips` / `clip` for timeline clips, `items` / `item` for project items):
  `clip.replaceFromBin {"clips":[12],"item":4}` runs with nothing selected and leaves the
  selection alone. Only the selection condition is answered by the named targets; every other
  one (an open sequence, a filled clipboard, the right kind of clip) still applies, and ids of
  nothing do not count. `command_list` / `engine.commands` keep reporting the selection's state.
- Element ids come from the previous frame. If an element is missing right after a layout change,
  the app retries on later frames before giving up.
- `timeline.move` takes linked partners along while Linked Selection is on: moving a picture
  clip moves its sound by the same offset (pass `"linked": false` to move only the listed clips).
  It returns `{"moved": [ids]}`, every clip that moved.
- Without a window, `filmagic-cli run script.jsonl` (lines of `{"id":"…","params":{…}}`) runs the
  same commands headlessly. See §2b for the rest of the CLI.

## 2b. Command-line interface

Every command is also one shell call away. Options go anywhere; output is JSON; exit status is 0 on
success, 1 when a command fails and 2 on a usage error. `filmagic-cli help`, `filmagic-cli --help`
and `filmagic-cli -h` print the reference and exit successfully.
If the reader of stdout closes the pipe early (`filmagic-cli commands | head`), the CLI drops the rest
of its output but still finishes the work, saves included, and the exit status still reports
failures; any other stdout write error is reported and makes the status 1.

```sh
filmagic-cli commands razor                     # find ids (add --json for machine output)
filmagic-cli describe timeline.razor            # one command: menu, shortcut, params, enabled now
filmagic-cli --demo inspect sequence            # project / sequence as JSON
filmagic-cli --project p.fcproj --save exec timeline.razor seconds=3.5
filmagic-cli --project p.fcproj exec effects.apply '{"effect":"Gaussian Blur"}'
filmagic-cli --project p.fcproj exec markers.exportCsv path=review.csv
filmagic-cli --project p.fcproj --save import a.mov b.wav
filmagic-cli --project p.fcproj export out.mp4  # format from the extension; waits for the job
filmagic-cli --project p.fcproj export out --preset "YouTube 1080p Full HD" --start 0 --end 10
filmagic-cli export --list-presets prores       # built-in + user presets (--data-dir for another library)
filmagic-cli --project p.fcproj exec export.queue.add preset="Apple ProRes 422 HQ" path=renders/ start=true wait=true
echo '{"id":"file.newBin","params":{"name":"Selects"}}' | filmagic-cli --project p.fcproj --save run -
filmagic-cli --bridge 127.0.0.1:9876 exec window.workspace.color   # the running app
```

`key=value` values are parsed as JSON when they can be (`3.5`, `true`, `[1,2]`), otherwise taken as
strings; dotted keys nest (`color.r=1`). `run` prints one JSON line per command
(`{"line","id","ok","result"|"error"}`) and stops at the first failure unless `--keep-going`.
`--save` writes back to `--project`; `--save-as path` writes elsewhere.

Runs that save over the project they opened take turns, so parallel scripts don't lose edits. Each
holds an OS lock on `.<name>.lock` next to the project from before it reads the file until it exits;
the next one says so on stderr, waits, and then edits what the first one saved. A crash releases the
lock, and the empty file left behind is harmless. Only these CLI runs take the lock: the desktop app,
`mcp`, and a `file.save` line in a script run without `--save` don't. OS locks can also be unreliable
over network shares and synced folders. If the lock can't be taken, the run warns on stderr and
saves anyway.

Media that is offline when it is needed (the file is missing, was made offline, or can't be read)
renders as the "media not found" placeholder. `export` and `render` then name each such item on
stderr (`item 8 a.mp4 (missing): /path/a.mp4`) and exit 1 after writing the output, so a moved or
unmounted media folder doesn't pass as a good render. `--allow-offline` accepts the placeholder
frames: the warning stays and the exit status is 0. The export result lists the same items as
`offlineMedia` (`[{item, name, path, status, error}]`, `status` as in `media.status`: `missing`,
`offline` or `unreadable`), and `[]` when every clip it drew was online; check before exporting with
`exec media.status` or `exec media.findMissing`.

`markers.exportCsv {path}` exports all markers in the active sequence, including colours hidden
in the UI and markers outside In/Out, sorted by start then id. The UTF-8 CSV has a header and CRLF
records; quotes, commas and embedded newlines are escaped. Columns are Sequence, Marker ID, Name,
Comment, Kind, Color, Start Timecode, End Timecode, Duration Frames, Start Ticks, Duration Ticks,
Frame Rate (exact numerator/denominator), Author, and URL. Custom colors use canonical `#RRGGBB`;
preset Color uses the user's display label from Settings ▸ Labels. Timecodes include the sequence's start timecode and
drop-frame setting; End is exclusive, and a point marker's End equals Start. Duration Frames is
rounded down; tick columns preserve sub-frame timing (254016000000 ticks per second).
An empty sequence writes just the header. User text starting with a spreadsheet formula character
(`=`, `+`, `-`, `@`, including after whitespace) gets a leading apostrophe in the exported cell.
The export leaves the project, dirty flag and undo history untouched, works with locked tracks,
and uses the host's file service (atomic
on desktop, a download on web). It exports sequence markers only, not clip/source markers;
reports are limited to 100000 markers and a conservative 16 MiB output budget. Invalid timing or a
failed write reports an error. The menu opens the normal save dialog; cancelling writes nothing.

## 3. Verifying UI work

For any UI change, look at the result:

1. Start the app with a fresh token (the control server stays closed without one), and the agent
   or `filmagic-cli --bridge` from the same shell:

   ```sh
   export FILMAGIC_CONTROL_TOKEN="$(openssl rand -hex 32)"
   cargo run --release -p filmagic -- --control 9876 >/dev/null 2>&1 &
   ```

2. Drive the feature through the control channel or the MCP bridge: commands, then clicks and drags
   by automation id.
3. Assert on `ui.inspect`, `ui.elements` and `sequence.inspect`.
4. Take `ui.screenshot` (the whole window and the panel you changed) and open the PNG.

Screenshots you commit must follow [AGENTS.md](../AGENTS.md) §1: Filmagic only, openly licensed
media, sidecar and `ATTRIBUTION.md` entry.

# Part 2: developing Filmagic

## 4. Orientation, in this order

1. [AGENTS.md](../AGENTS.md): absolute rules (assets, clean-room, licences).
2. [CLAUDE.md](../CLAUDE.md) only imports AGENTS.md, so Claude Code reads the same rules; Codex reads
   AGENTS.md directly. There is no second set of instructions.
3. [ROADMAP.md](../ROADMAP.md) for the summary, then [gaps.md](gaps.md) (the ranked shortfalls) first,
   [target-app-parity.md](target-app-parity.md) and [roadmap.md](roadmap.md) (milestones, current focus).
4. [architecture.md](architecture.md), then the README and tests of the crate you'll touch.
5. [contributing.md](contributing.md) (how to add things, gates) and [testing.md](testing.md).

Maintainers also keep a local planning folder, `plan/`. It is gitignored and not in the repository,
and holds the task-level plan, the status file and behaviour reference notes. If you don't have it,
everything you need to contribute is in the public docs above. Ask a maintainer for a task id.

## 5. Autonomous work loop

1. **Orient.** Pick the next task: the next unchecked task in the maintainer status file, or an open
   [gaps.md](gaps.md) entry. Prefer those gaps (stability, `.prproj`, camera media, measurement,
   hardware, Windows/Linux) over adding more checklist items. Read the relevant
   architecture section and crate README.
2. **Plan tests first.** Write down the acceptance test before writing code.
3. **Implement and test.**
4. **Verify.** Run all gates (`cargo xtask ci`). For UI work, run the app with `--control`, drive it
   and look at the screenshots (§3).
5. **Record.** Update the crate README (behaviour decisions, test results, limitations) and
   the progress docs when a milestone moves (ROADMAP.md, docs/roadmap.md, the relevant parity doc
   and gaps.md, with their timestamps). Commit as `M<n>.<k>: …`, then move on to the next task.

For parallel agents, use one git worktree and one `CARGO_TARGET_DIR` per agent, and keep each crate
with one owner ([contributing.md §5](contributing.md#5-parallel-work-several-agents-or-worktrees)).

## 6. Decision rules

Don't block on choices you can make yourself:

- **Behaviour.** Match the publicly documented and observable behaviour of professional editors
  (help pages, published shortcut lists, using the app). Never inspect application internals
  ([AGENTS.md](../AGENTS.md) §2). If behaviour is undocumented, choose what editors most commonly
  expect and record the choice in the crate README.
- **Ask a human only about:**
  - licences outside the allowlist;
  - spending money;
  - publishing or pushing;
  - personal data;
  - destructive git operations;
  - anything AGENTS.md leaves unclear ("when in doubt, leave it out").

## 7. Definition of Done

A feature is done when:

- [ ] its command(s) are registered with label, menu path, shortcut (if any) and a parameter doc;
- [ ] engine tests cover it, including undo/redo and the disabled case;
- [ ] the UI is wired: menu, panel control and shortcut;
- [ ] every new interactive widget has an automation id;
- [ ] you have driven it through the control channel and reviewed a screenshot;
- [ ] all gates pass (`cargo xtask ci`), and new assets have sidecars;
- [ ] the crate README and the progress docs (ROADMAP.md, docs/gaps.md, the parity docs) are updated where relevant;
- [ ] it is committed with its task id.

### Reporting progress honestly

ROADMAP.md and [target-app-parity.md](target-app-parity.md) keep two numbers: **feature breadth**
(does it exist?) and **ready for real work** (does it hold up?). When you update either:

- say how a number was obtained: *measured* (a test, a diff, a benchmark you ran) or *estimated*;
- count an approximation, a stub or a partly wired setting as such, never as done;
- record known bugs and limitations in the crate README and [gaps.md](gaps.md), not only
  what works;
- benchmarks state the machine, the load average and before/after on the same build;
- don't raise a percentage without evidence a reviewer can rerun.

### Marker authors and custom colors

`markers.add`, the range/chapter/Flash Cue marker commands, and `markers.edit` accept
`author` (text, at most 1024 UTF-8 bytes without control characters) and `color` (an
existing label name or six hex digits, optionally prefixed with `#`). Commands and CSV
report custom colors as uppercase `#RRGGBB`. Omitting either property while editing
preserves it; `author: null` also preserves the author, `author: ""` clears attribution,
and choosing a preset clears custom color.
The source-target variants of Add/Edit accept the same properties. Inspect reports
include `author` and the effective `color`.

`markers.edit` also accepts `kind:"comment|chapter|segmentation|webLink|flashCue"`.
The editor's visible Type radio group (`markerProperties.kind`, with choices
`markerProperties.kind.<kind>`) applies type changes with the other fields in one Undo
step. The compact form follows Name, Author, Time/Duration, Notes, then grouped Color
and vertical Type choices. The editor offers Comment, Chapter, Segmentation and Web Link;
existing legacy marker types are retained unless explicitly changed. A narrow action
column uses centered buttons; small windows move actions below the scrolling form.
It retains the same sequence/source owner.
Notes scroll within a bounded field. Time and Duration accept only digits and colons
in `HH:MM:SS:FF` form (three frame digits at 100 fps or higher); Done remains disabled for incomplete or invalid timecodes.
Unchecked Duration ignores its draft value and saves a point marker with zero duration.
Text fields use normal text editing shortcuts and their own Undo/Redo history.

Previous/Next (`markerProperties.previous`, `markerProperties.next`) save valid changes
to the current marker as one Undo step, then open its neighbour in chronological order
with marker ID breaking time ties. Navigation stays in the original sequence/source view,
even if another is opened. Unchanged navigation adds no Undo step; invalid drafts remain
open without moving or changing the project. Ends disable the corresponding button.
Cancel discards only the current draft; edits saved by earlier navigation remain undoable.
These controls change the editor, without redirecting another sequence/source playhead.

Web Link markers show a separate URL field (`markerProperties.url`) and an explicit
Open Link button (`markerProperties.openLink`). Notes remain independent. `markers.edit`
accepts `url` (an absolute HTTP(S) address, up to 8192 UTF-8 bytes, no control characters
or embedded credentials); an empty string clears it and omission/null retains it.
The URL is retained across type changes. Opening uses the existing browser service
and never occurs during playback, seeking, project load or Undo/Redo. URLs survive
project/OTIO round trips and appear in a separate CSV column; other interchange formats
report URL loss. Imported addresses are checked again before opening.

`colors.addCustom {color:"#RRGGBB"}` saves a reusable project-local marker/clip swatch (maximum 64). Duplicate colors are no-ops. Palette additions are independently undoable from marker edits.
Interchange imports fill missing names on existing swatches while preserving target names,
even when the palette is full. Formats that cannot retain the saved palette warn about its
loss even if none of its colors are assigned; Filmagic projects and OTIO retain it.
An OTIO collection unions its timelines' saved palettes in encounter order, preserving the
first valid name and filling missing names for retained colors. Empty later palettes do
not clear earlier colors. Colors beyond the project's 64-entry limit produce a warning.

`edit.label` also accepts a hex `label` and `saveColor:true` to save and apply a custom clip color atomically. Markers and clips share the unchanged sixteen clip presets; Red, Orange, Cyan and Pink are aliases for Rose, Mango, Teal and Magenta.

Adding a marker stays immediate and does not open an editor. The dialogs described here are
marker editing controls, opened explicitly for an existing marker.
If a command changes or removes that marker while its editor is open, the editor and its
native color session close with a status message; reopen it to edit the current values.
Changes to other markers do not discard the draft.

Range markers split their flag vertically, placing the left half at the start and the right
half at the end in the Timeline, Source, and Program rulers. Both ends retain the marker's
identity for editing; `timeline.markerEnd.<id>`, `source.markerEnd.<id>`, and
`program.markerEnd.<id>` expose the end flags to automation. Point markers keep their whole flag.

On macOS, custom marker/clip colors and project-palette edits open the system Color panel.
Its live selection updates only the draft; Filmagic's Add/Save/Apply and Cancel actions keep
ownership of project changes and Undo. Closing the Color panel retains the draft without applying
it. Colors are converted to opaque sRGB `#RRGGBB`; typed hex values update the open panel.
Preset swatches and named project colors remain in Filmagic. Other hosts, headless automation,
and native-picker failures reuse egui's established opaque color picker with Filmagic's hex field.
The `*.native` automation control reopens the system panel; `*.hex` remains available on either path. Switching projects or leaving
the draft closes its native panel, and an older draft cannot redirect or close a newer picker.
While the native Color panel has keyboard focus, its Edit menu uses AppKit's normal text actions
for Copy/Paste, Select All, and Undo/Redo. Filmagic's Edit menu returns when its main window
regains focus. Temporary panel hiding during app deactivation does not discard the color draft.

This host picker is scoped to marker colors, clip labels, and project palette editing. Existing
text, graphics, effect, transition, and settings color controls retain their established behavior.
The label editor stores opaque sRGB bytes; it does not reinterpret floating-point effect colors,
HDR values, or alpha. The fallback shares egui's picker implementation with those controls,
without replacing their command or color-format semantics.
