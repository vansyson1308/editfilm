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
the app's project file while it drives the app. If that project was never saved, `app_open` needs
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

### Tools

| Tool | Modes | Purpose |
|---|---|---|
| `command_list` | both | every command: id, label, menu, shortcut, params, enabled now (`filter`, `enabled_only`) |
| `command_run` | both | run a command `{id, params}`; edits are undoable |
| `command_batch` | both | run several commands in order `{steps: [{id, params}], stop_on_error}` → `{completed, failed, results}` |
| `doc_inspect` | both | the project tree and the active sequence in one call (`project_inspect` + `sequence_inspect`) |
| `render_preview` | both | same as `render_frame` |
| `project_inspect` | both | bins and items with ids, types, durations; active sequence |
| `sequence_inspect` | both | the active sequence: tracks, clips (`start` / `end` and `sourceIn` / `sourceOut` in ticks, frames, `speed`, `reverse`: the clip plays its `sourceIn` to `sourceOut` stretch backward, `gainDb`), effects, transitions, markers (name and `comment`), playhead, selection |
| `media_import` | both | import files by absolute path (`paths`, an array; optional `bin`, `image_sequence`) |
| `render_frame` | both | PNG of the program frame at `seconds` (headless renders; bridge screenshots the Program monitor). Read-only: the playhead and selection are left as they were. Parallel bridge calls take turns, so each returns its own frame |
| `story_board` | both | PNG of one representative frame per shot of the active sequence, or of `count` frames across a project `item`'s rushes, with what each frame is as JSON (`item`, `count`, `max`, `from_seconds`, `to_seconds`, `columns`, `width`, `labels`). Read-only |
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

Two habits make these reliable. Read `warnings` and `notes` in every result: they say what a
command could not do. And read `pictureChange` after a command that alters the picture: `subtle`
means a viewer will not notice it. A region read off a rendered frame takes `"space":"frame"`
when the clip is moved, zoomed or turned.

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
