# craft-update, vendored in Filmagic

This is `craft-update` (release checking, downloading, installing and self-updating) from
[storytold/craft-libs](https://github.com/storytold/craft-libs), directory `crates/update`, at
revision `1b34eba41e62138fc2438d2734523b302840b45d`. It is MIT OR Apache-2.0 (`LICENSE-MIT`,
`LICENSE-APACHE`, copied from the craft-libs root at that revision).

## Why it is here

Upstream, the crate serves the Craft App Suite and the ArtCraft Launcher: it builds every app's
bundle id as `ai.storyteller.<app>`, links to the suite's website, reads the launcher's version pins
and looks for installs in the suite's folders. Filmagic is a fork with its own identifier
(`io.github.vansyson1308.editfilm`), no launcher and no suite, and the library computes the bundle id
itself (Flatpak detection, the desktop entry beside an AppImage, the macOS bundle check), so the
library and Filmagic's packaging must agree on the id. Filmagic therefore keeps its own copy.

## How it is wired

- `Cargo.toml` (workspace root) names it as a path dependency,
  `craft-update = { path = "vendor/craft-update", ... }`, rather than a `[patch]` of craft-libs:
  only Filmagic's own crates depend on `craft-update`, so a patch would add an indirection and would
  read as a temporary override of upstream, which this is not.
- The workspace `exclude`s it, so `cargo xtask ci` does not build its tests or run clippy on it.
  Run its tests from this directory: `cargo test` (see "Tests" below).
- Its `Cargo.toml` no longer inherits from the craft-libs workspace: the package fields, the lints
  and the four workspace dependencies (`egui`, `serde`, `serde_json`, `chrono`) are written out with
  the requirements craft-libs used, so `Cargo.lock` resolves the same versions as before. The
  upstream manifest is kept unchanged as `Cargo.toml.orig`.
- `SOURCE.json` lists every file with the sha256 and git blob of its upstream original and the
  sha256 of the copy here; `changed_files` names the files that differ.

## Local changes

1. `Cargo.toml`: made standalone, as described above. No dependency or feature changed.
