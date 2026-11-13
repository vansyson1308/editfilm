//! The default catalogue: packs `assets.install` can fetch by id (`{"catalog": "kenney-interface-sounds"}`).
//!
//! An entry goes in only when its licence was read in the archive itself and on the author's page,
//! and its archive is pinned by SHA-256 and size: a changed file is refused, never installed. Nothing
//! is downloaded until someone asks for the pack. Each entry is listed in `ATTRIBUTION.md`
//! (section "Asset pack catalogue"), as AGENTS.md §1 asks of anything the app points to.

use serde::Serialize;

/// One downloadable pack.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// What its items are (the pack's own folder names do not say).
    pub kind: &'static str,
    /// Words that fit every item, given to the installed pack (`assets.search` counts them below
    /// the words of each file's own name). Only words that are true of every file in the archive.
    pub tags: &'static [&'static str],
    pub license: &'static str,
    pub author: &'static str,
    /// The author's page for the pack.
    pub page: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub files: usize,
    pub attribution: &'static str,
}

/// Every entry. Verified 2026-11-13: the archive's `License.txt` reads "License: (Creative Commons
/// Zero, CC0)", kenney.nl lists the pack as CC0, and the archive is Kenney's own upload.
pub const CATALOG: &[Entry] = &[
    Entry {
        id: "kenney-interface-sounds",
        name: "Interface Sounds (Kenney)",
        description: "100 short clicks, selects, confirmations, errors, open/close, toggles and glitches: UI and transition accents",
        kind: "sfx",
        tags: &["ui", "interface", "button", "menu"],
        license: "CC0-1.0",
        author: "Kenney (www.kenney.nl)",
        page: "https://kenney.nl/assets/interface-sounds",
        url: "https://opengameart.org/sites/default/files/kenney_interfaceSounds.zip",
        sha256: "f2193d072726d6758a5f7871b2dcc54dcce0d5c35c6f0a62f92549b327c81232",
        size: 834_536,
        files: 100,
        attribution: "Interface Sounds by Kenney (www.kenney.nl), CC0",
    },
    Entry {
        id: "kenney-sci-fi-sounds",
        name: "Sci-Fi Sounds (Kenney)",
        description: "73 sci-fi effects: metal impacts, explosions, lasers, force fields, thrusters, doors, computer noise",
        kind: "sfx",
        tags: &["sci-fi", "space", "futuristic"],
        license: "CC0-1.0",
        author: "Kenney (www.kenney.nl)",
        page: "https://kenney.nl/assets/sci-fi-sounds",
        url: "https://opengameart.org/sites/default/files/sci-fi_sounds.zip",
        sha256: "119340f351a5098ad814f78719438c0da355a9ce8a4c8a3af6a8d48aa3d49e04",
        size: 5_875_104,
        files: 73,
        attribution: "Sci-Fi Sounds by Kenney (www.kenney.nl), CC0",
    },
];

pub fn find(id: &str) -> Option<&'static Entry> {
    CATALOG.iter().find(|e| e.id == id)
}
