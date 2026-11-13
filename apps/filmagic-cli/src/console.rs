//! Text that survives the Windows console's code pages (#17).
//!
//! Rust's standard library writes UTF-16 to a real Windows console window, so text printed
//! straight to a console shows correctly. Output that goes through a pipe or a file is written as
//! UTF-8 bytes, and many Windows readers decode those bytes in the console's OEM or ANSI code
//! page: PowerShell decodes a native program's output with `[Console]::OutputEncoding`, which is
//! the OEM code page (437, 850, 1258…) unless changed, and so do tools that run commands through
//! it. `filmagic-cli commands | more` then printed "…" and "→" as "�?�".
//!
//! This program cannot ask Windows which code page the reader uses without calling the console
//! API, which needs `unsafe` (AGENTS.md §0.3 keeps that to `crates/platform` and its media APIs).
//! So on Windows, when standard output (or standard error) is not a console, the output is made
//! ASCII, which reads the same in every code page:
//! - JSON keeps every character: non-ASCII ones become `\uXXXX` escapes, which every JSON reader
//!   decodes back to the same text;
//! - plain text (the `commands` table, messages) spells common punctuation in ASCII ("..." for
//!   "…", "->" for "→") and shows any other non-ASCII character as `?`.
//!
//! `FILMAGIC_CLI_UTF8=1` turns this off, for a reader that decodes UTF-8 (PowerShell after
//! `[Console]::OutputEncoding = [Text.Encoding]::UTF8`, Git Bash, a file read as UTF-8).
//! Elsewhere (macOS, Linux) output is always UTF-8. `filmagic-cli mcp` is not affected: MCP is
//! JSON-RPC over UTF-8 pipes by definition.

use std::borrow::Cow;
use std::io::IsTerminal as _;
use std::sync::OnceLock;

/// The environment variable that keeps output UTF-8 on Windows.
pub const UTF8_ENV: &str = "FILMAGIC_CLI_UTF8";

/// Whether output to a stream must be ASCII: on Windows, when the stream is not a console and
/// [`UTF8_ENV`] is not `1`.
pub fn needs_ascii(windows: bool, terminal: bool, utf8_env: Option<&str>) -> bool {
    windows && !terminal && utf8_env.map(str::trim) != Some("1")
}

fn decide(terminal: bool) -> bool {
    needs_ascii(cfg!(windows), terminal, std::env::var(UTF8_ENV).ok().as_deref())
}

/// [`needs_ascii`] for standard output, decided once.
pub fn stdout_ascii() -> bool {
    static ASCII: OnceLock<bool> = OnceLock::new();
    *ASCII.get_or_init(|| decide(std::io::stdout().is_terminal()))
}

/// [`needs_ascii`] for standard error, decided once.
pub fn stderr_ascii() -> bool {
    static ASCII: OnceLock<bool> = OnceLock::new();
    *ASCII.get_or_init(|| decide(std::io::stderr().is_terminal()))
}

/// ASCII spellings of the punctuation this program prints.
fn ascii_for(c: char) -> Option<&'static str> {
    Some(match c {
        '…' => "...",
        '→' => "->",
        '←' => "<-",
        '↔' => "<->",
        '▸' | '›' => ">",
        '‹' => "<",
        '·' | '•' => "-",
        '—' => "--",
        '–' | '‐' | '−' => "-",
        '“' | '”' | '„' => "\"",
        '‘' | '’' | '‚' => "'",
        '×' => "x",
        '≤' => "<=",
        '≥' => ">=",
        '≈' => "~",
        '½' => "1/2",
        '°' => " deg",
        '\u{a0}' | '\u{2009}' | '\u{202f}' => " ",
        _ => return None,
    })
}

/// `s` as ASCII text: known punctuation spelled out, any other non-ASCII character as `?`.
pub fn ascii_text(s: &str) -> Cow<'_, str> {
    if s.is_ascii() {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match (c.is_ascii(), ascii_for(c)) {
            (true, _) => out.push(c),
            (false, Some(a)) => out.push_str(a),
            (false, None) => out.push('?'),
        }
    }
    Cow::Owned(out)
}

/// Serialized JSON as ASCII, losing nothing: every non-ASCII character becomes a `\uXXXX` escape
/// (two, a surrogate pair, above U+FFFF). Correct for any serializer output, since JSON allows
/// non-ASCII characters only inside strings, where the escapes mean the same characters.
pub fn ascii_json(s: &str) -> Cow<'_, str> {
    if s.is_ascii() {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + s.len() / 2);
    let mut units = [0u16; 2];
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            for u in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    Cow::Owned(out)
}

/// Text for standard output.
pub fn stdout_text(s: &str) -> Cow<'_, str> {
    if stdout_ascii() { ascii_text(s) } else { Cow::Borrowed(s) }
}

/// JSON for standard output.
pub fn stdout_json(s: &str) -> Cow<'_, str> {
    if stdout_ascii() { ascii_json(s) } else { Cow::Borrowed(s) }
}

/// Text for standard error.
pub fn stderr_text(s: &str) -> Cow<'_, str> {
    if stderr_ascii() { ascii_text(s) } else { Cow::Borrowed(s) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn only_windows_pipes_need_ascii() {
        assert!(needs_ascii(true, false, None));
        assert!(!needs_ascii(true, true, None), "a real console is written in UTF-16 by std");
        assert!(!needs_ascii(false, false, None), "macOS and Linux readers decode UTF-8");
        assert!(!needs_ascii(true, false, Some("1")));
        assert!(!needs_ascii(true, false, Some(" 1 ")));
        for other in ["", "0", "true", "yes", "2"] {
            assert!(needs_ascii(true, false, Some(other)), "{other}");
        }
    }

    /// The characters of the report (#17) come out as ASCII, and other text is kept.
    #[test]
    fn text_spells_punctuation_in_ascii() {
        assert_eq!(ascii_text("Save As…"), "Save As...");
        assert_eq!(ascii_text("10 frames → 4.0 fps"), "10 frames -> 4.0 fps");
        assert_eq!(ascii_text("File ▸ Export · Media — “quoted” ½-res"), "File > Export - Media -- \"quoted\" 1/2-res");
        assert_eq!(ascii_text("Tiếng Việt 🎬"), "Ti?ng Vi?t ?");
        assert!(matches!(ascii_text("plain"), Cow::Borrowed("plain")));
        assert!(ascii_text("…→▸·—–“”‘’×≤≥≈½°\u{a0}é漢\u{10ffff}").is_ascii());
    }

    /// Escaped JSON is ASCII and parses back to exactly the same value, whatever the characters.
    #[test]
    fn json_keeps_every_character() {
        let v = json!({"label": "Save As…", "arrow": "→", "vi": "Tiếng Việt", "emoji": "🎬", "cjk": "漢字", "edge": "\u{ffff}\u{10000}\u{10ffff}", "quote": "\"\\…"});
        for s in [serde_json::to_string(&v).unwrap(), serde_json::to_string_pretty(&v).unwrap()] {
            let a = ascii_json(&s);
            assert!(a.is_ascii(), "{a}");
            assert_eq!(serde_json::from_str::<Value>(&a).unwrap(), v);
        }
        assert!(ascii_json("{\"a\":1}").is_ascii());
    }
}
