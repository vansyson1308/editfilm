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

Every change is marked "Filmagic change" in a comment or doc comment where it is not obvious, and
`SOURCE.json` names every changed file. Nothing has been released under the old identifier, so no
change migrates anything a user already has.

1. `Cargo.toml`: made standalone, as described above. No dependency or feature changed.
   `rustfmt.toml` is craft-libs' own, copied so `cargo fmt` here formats as upstream does.
2. `app.rs`: `App::bundle_id_override`, an optional reverse-DNS id the app names for itself;
   `bundle_id()` returns it when set and `ai.storyteller.<id>` otherwise. Every use of the bundle id
   (Flatpak detection and restart, the desktop entry and icon beside an AppImage, the macOS
   `CFBundleIdentifier` check, package names) goes through `bundle_id()`, so Filmagic sets it once.
   Its tests cover the override.
3. `app.rs`: `App::website()` removed. It linked to getartcraft.com, which has no page for Filmagic,
   and nothing called it.
4. `desktop_entry.rs`: the marker on the menu entry written beside an AppImage is
   `X-Craft-Update=true` instead of `X-ArtCraft-Launcher=true` (it is written to the user's disk).
   `meta` gives `filmagic` the categories of its packaged menu entry
   (`AudioVideo;Video;AudioVideoEditing;`) instead of the fallback `Graphics;`. Tests updated, and
   one added for an app with its own bundle id.
5. `install.rs`: the probe, staging and backup files it creates are named `.craft-update-…`
   instead of `.artcraft-launcher-…`, and "already exists and wasn't installed by ArtCraft Launcher"
   reads "… by this updater".
6. `pins.rs`: no default version-pin file. Upstream reads the ArtCraft Launcher's
   `<per-user data>/ArtCraft/versions/versions.json`; Filmagic has no launcher, so only a file named
   by `CRAFT_VERSIONS_FILE` is read and every check otherwise follows stable releases.
   `FLATPAK_GRANT` (`--filesystem=xdg-data/ArtCraft/versions:ro`) is removed, and so is that grant
   in Filmagic's two Flatpak manifests. The test of the default path now checks there is none.
7. `msi_migrate.rs`, `msi_context.rs`, `msi_copy.rs`: Windows installs have no `ArtCraft` suite
   folder. The per-user default is `%LOCALAPPDATA%\Programs\<App>` (upstream also listed
   `Programs\ArtCraft\<App>` first), and a per-user copy moving to everyone goes to
   `Program Files\<App>` instead of `Program Files\ArtCraft\<App>`, matching
   `packaging/windows/install-policy.wxi`. Tests updated to the new folders.
8. `self_update.rs`: the shared pretend-version variable is `CRAFT_UPDATE_PRETEND_VERSION` (beside
   the existing `CRAFT_UPDATE_DEV`) instead of `ARTCRAFT_LAUNCHER_PRETEND_VERSION`; the per-app
   `FILMAGIC_PRETEND_VERSION` is unchanged.
9. `protocol.rs`, `status.rs`: the labels of `Source::Feed` and `InstallSource::Launcher` read
   "release feed" and "Launcher" instead of naming ArtCraft. Filmagic never produces either, but
   both are display strings.

## Left as upstream has it, and why

- `suite.rs` (the catalog of the upstream apps and the ArtCraft Launcher), `app::DEVELOPER`
  ("ArtCraft", the default developer of those catalog entries) and the `ai.storyteller.<id>`
  default of `bundle_id()`: they describe upstream's apps. Filmagic's `UPDATE_APP` names its own
  developer and bundle id, is not in the catalog, and nothing Filmagic runs passes a catalog entry.
  The many tests that use catalog entries as sample apps keep working unchanged.
- `detect::linux::packages()` asks Flatpak for the version of `ai.storyteller.*` bundles. It belongs
  to `detect::Snapshot::take`, the launcher's scan of every suite app, which Filmagic never calls.
- Doc comments and test data that mention the launcher or ArtCraft paths: not shown and not
  written by Filmagic.
- Upstream identity that is not a name and is not changed here: `detect.rs` recognises Windows
  Installer entries by the publisher `Learning Machines LLC`, `msi_copy.rs` reads the launcher's
  `HKCU\Software\Learning Machines LLC\<App>` folder, and `install::macos::TEAM_ID` is upstream's
  Apple Developer team. Filmagic's MSI names "Filmagic contributors" as its publisher, so a
  self-update of an MSI copy stops with "Windows lists this app's installation more than once or not
  at all" until these become settings of `App` as well; see the pull request that added this file.

## Tests

The workspace does not build this crate's tests. Run them from this directory with craft-libs'
lockfile and egui patches (the egui fork `storytold/egui` at `6d518fc7`), for example:

```sh
cp <craft-libs checkout at 1b34eba>/Cargo.lock .
cargo test --features egui --no-fail-fast \
  --config 'patch.crates-io.egui.git="https://github.com/storytold/egui.git"' \
  --config 'patch.crates-io.egui.rev="6d518fc799681571963842f95b55a6b68431b15a"'   # and likewise ecolor, emath, epaint, epaint_default_fonts, egui_kittest
rm Cargo.lock
```

`Cargo.lock` and `target/` here are ignored by git.
