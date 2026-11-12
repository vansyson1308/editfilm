//! The app identifier is one value everywhere. Wayland and the desktop shells match the window to
//! the `.desktop` file by it, Flatpak and AppStream key the app by it, the macOS bundle and project
//! type carry it, and the updater (`UPDATE_APP.bundle_id()`) uses it to recognise its own Flatpak,
//! the menu entry beside an AppImage and the running `.app`. If any of them drifts, the app loses
//! its icon, its file type or its updates without an error, so these tests fail instead. The same
//! goes for the rest of the app's identity the updater relies on: the Windows publisher and install
//! key, and the Apple Developer team. Nothing outside `vendor/` may name the upstream company
//! except the notes listed in [`UPSTREAM_NOTES`].

use std::path::{Path, PathBuf};

use crate::APP_ID;

const DESKTOP: &str = include_str!("../../../packaging/linux/io.github.vansyson1308.editfilm.desktop");
const METAINFO: &str = include_str!("../../../packaging/linux/io.github.vansyson1308.editfilm.metainfo.xml.in");
const MIME: &str = include_str!("../../../packaging/linux/io.github.vansyson1308.editfilm.mime.xml");
const FLATPAK: &str = include_str!("../../../packaging/linux/flatpak/io.github.vansyson1308.editfilm.yml");
const FLATPAK_BUNDLE: &str = include_str!("../../../packaging/linux/flatpak/io.github.vansyson1308.editfilm.bundle.yml");
const INFO_PLIST: &str = include_str!("../../../packaging/macos/Info.plist.in");
const OMASTORE: &str = include_str!("../../../omastore.toml");
const WXS: &str = include_str!("../../../packaging/windows/filmagic.wxs");
const WXI: &str = include_str!("../../../packaging/windows/install-policy.wxi");
const PACKAGE_SH: &str = include_str!("../../../packaging/linux/package.sh");
/// The scripts that name the id themselves, with the assignment each one makes.
const SCRIPTS: [(&str, &str, &str); 6] = [
    ("packaging/icons.sh", include_str!("../../../packaging/icons.sh"), "APP_ID=\""),
    ("packaging/linux/package.sh", include_str!("../../../packaging/linux/package.sh"), "APP_ID="),
    ("packaging/linux/flatpak-bundle.sh", include_str!("../../../packaging/linux/flatpak-bundle.sh"), "APP_ID="),
    ("packaging/linux/install.sh", include_str!("../../../packaging/linux/install.sh"), "app_id="),
    ("packaging/linux/test_install.py", include_str!("../../../packaging/linux/test_install.py"), "APP_ID = \""),
    ("packaging/freebsd/package.sh", include_str!("../../../packaging/freebsd/package.sh"), "APP_ID="),
];
const HICOLOR_SIZES: [&str; 8] = ["16x16", "24x24", "32x32", "48x48", "64x64", "128x128", "256x256", "512x512"];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The value of `key=` in a desktop entry.
fn desktop_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
}

/// The text of the one element opened by `open` (attributes included), up to the next `<`.
fn element_text<'a>(text: &'a str, open: &str) -> Vec<&'a str> {
    text.match_indices(open).filter_map(|(at, _)| text.get(at + open.len()..)?.split('<').next()).collect()
}

/// The `<string>` that follows `<key>key</key>` in a property list.
fn plist_string<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let after = text.split_once(&format!("<key>{key}</key>"))?.1;
    after.trim_start().strip_prefix("<string>")?.split("</string>").next()
}

/// A Flatpak manifest's top-level `key: value`.
fn yaml_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(": ")).map(str::trim)
}

/// A Flatpak manifest's `finish-args`, comments left out.
fn finish_args(text: &str) -> Vec<&str> {
    text.lines().skip_while(|l| *l != "finish-args:").skip(1).take_while(|l| l.starts_with("  ")).filter_map(|l| l.trim().strip_prefix("- ")).collect()
}

#[test]
fn the_updater_knows_the_app_by_its_app_id() {
    let app = &filmagic_ui_egui::UPDATE_APP;
    assert_eq!(app.bundle_id(), APP_ID);
    assert_eq!(app.id, "filmagic", "the binary and asset stem stay `filmagic`");
    assert_eq!(app.name, "Filmagic");
    assert!(!app.developer.contains("ArtCraft"), "{}", app.developer);
}

#[test]
fn the_desktop_entry_carries_the_app_id() {
    assert_eq!(desktop_value(DESKTOP, "Icon"), Some(APP_ID));
    assert_eq!(desktop_value(DESKTOP, "StartupWMClass"), Some(APP_ID));
    assert!(repo().join(format!("packaging/linux/{APP_ID}.desktop")).is_file(), "the desktop file is named by the app id");
}

#[test]
fn the_entry_the_updater_writes_beside_an_appimage_matches_the_packaged_one() {
    let app = &filmagic_ui_egui::UPDATE_APP;
    let Some(text) = craft_update::desktop_entry::render(app, Path::new("/home/a/Applications/filmagic.AppImage"), true) else {
        panic!("no desktop entry for a plain path");
    };
    for key in ["Name", "Icon", "StartupWMClass", "Categories"] {
        assert_eq!(desktop_value(&text, key), desktop_value(DESKTOP, key), "{key}:\n{text}");
    }
    assert!(!text.contains("ArtCraft"), "{text}");
}

#[test]
fn appstream_and_mime_name_the_app_id() {
    assert_eq!(element_text(METAINFO, "<id>"), [APP_ID]);
    assert_eq!(element_text(METAINFO, "<launchable type=\"desktop-id\">"), [format!("{APP_ID}.desktop").as_str()]);
    assert_eq!(element_text(METAINFO, "<icon type=\"stock\">"), [APP_ID]);
    assert!(MIME.contains(&format!("<icon name=\"{APP_ID}\"/>")), "{MIME}");
    for file in [format!("{APP_ID}.metainfo.xml.in"), format!("{APP_ID}.mime.xml")] {
        assert!(repo().join("packaging/linux").join(&file).is_file(), "{file}");
    }
}

#[test]
fn both_flatpak_manifests_use_the_app_id_and_agree() {
    for (name, text) in [("from-source", FLATPAK), ("bundle", FLATPAK_BUNDLE)] {
        assert_eq!(yaml_value(text, "id"), Some(APP_ID), "{name} manifest");
        for installed in [
            format!("/app/share/applications/{APP_ID}.desktop"),
            format!("/app/share/mime/packages/{APP_ID}.xml"),
            format!("/app/share/metainfo/{APP_ID}.metainfo.xml"),
        ] {
            assert!(text.contains(&installed), "{name} manifest installs {installed}");
        }
    }
    for key in ["runtime", "runtime-version", "sdk", "command"] {
        assert_eq!(yaml_value(FLATPAK, key), yaml_value(FLATPAK_BUNDLE, key), "{key}");
    }
    assert_eq!(finish_args(FLATPAK), finish_args(FLATPAK_BUNDLE));
    assert!(finish_args(FLATPAK).contains(&"--share=network"), "parsed: {:?}", finish_args(FLATPAK));
    for file in [format!("{APP_ID}.yml"), format!("{APP_ID}.bundle.yml")] {
        assert!(repo().join("packaging/linux/flatpak").join(&file).is_file(), "{file}");
    }
}

#[test]
fn the_macos_bundle_and_project_type_use_the_app_id() {
    assert_eq!(plist_string(INFO_PLIST, "CFBundleIdentifier"), Some(APP_ID));
    let project = format!("{APP_ID}.project");
    assert_eq!(plist_string(INFO_PLIST, "UTTypeIdentifier"), Some(project.as_str()));
    // Once as the exported type, once in the document type that opens it.
    assert_eq!(INFO_PLIST.matches(&format!("<string>{project}</string>")).count(), 2);
}

#[test]
fn the_icon_theme_files_are_named_by_the_app_id() {
    let hicolor = repo().join("assets/app-icon/hicolor");
    let scalable = hicolor.join("scalable/apps").join(format!("{APP_ID}.svg"));
    assert!(scalable.is_file(), "{}", scalable.display());
    for size in HICOLOR_SIZES {
        let png = hicolor.join(size).join("apps").join(format!("{APP_ID}.png"));
        assert!(png.is_file(), "{}", png.display());
    }
    for size in HICOLOR_SIZES.into_iter().chain(["scalable"]) {
        let Ok(entries) = std::fs::read_dir(hicolor.join(size).join("apps")) else { panic!("hicolor/{size}/apps") };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(name.starts_with(&format!("{APP_ID}.")), "hicolor/{size}/apps/{name} isn't named by the app id");
        }
    }
    assert_eq!(OMASTORE.lines().find_map(|l| l.strip_prefix("icon = ")), Some(format!("\"assets/app-icon/hicolor/256x256/apps/{APP_ID}.png\"").as_str()));
}

#[test]
fn the_packaging_scripts_name_the_app_id() {
    for (path, text, assignment) in SCRIPTS {
        let value = text.lines().find_map(|l| l.trim().strip_prefix(assignment)).map(|v| v.trim_end_matches('"'));
        assert_eq!(value, Some(APP_ID), "{path}");
    }
}

#[test]
fn no_packaging_file_names_the_upstream_identifier() {
    let files = [DESKTOP, METAINFO, MIME, FLATPAK, FLATPAK_BUNDLE, INFO_PLIST, OMASTORE, WXS, WXI].into_iter().chain(SCRIPTS.map(|(_, text, _)| text));
    for text in files {
        let lower = text.to_ascii_lowercase();
        for term in UPSTREAM_TERMS {
            assert!(!lower.contains(term), "{term}:\n{text}");
        }
    }
}

/// The value of attribute `name` on every element opened by `open`.
fn attributes<'a>(text: &'a str, open: &str, name: &str) -> Vec<&'a str> {
    let needle = format!(" {name}=\"");
    text.match_indices(open)
        .filter_map(|(at, _)| {
            let tag = text.get(at..)?.split('>').next()?;
            tag.split_once(&needle)?.1.split('"').next()
        })
        .collect()
}

#[test]
fn the_updater_knows_the_windows_installer_by_its_publisher_and_key() {
    let app = &filmagic_ui_egui::UPDATE_APP;
    // The uninstall entry's Publisher is the package's Manufacturer, set in two places.
    let manufacturers = attributes(WXS, "<Package ", "Manufacturer").into_iter().chain(attributes(WXS, "<SummaryInformation ", "Manufacturer"));
    let manufacturers: Vec<&str> = manufacturers.collect();
    assert_eq!(manufacturers, [app.publisher, app.publisher]);
    // The key the installer records InstallDir under, in the context it was installed for, and the
    // key the installer's own searches read back for both contexts.
    let install_dir: Vec<&str> = WXS
        .lines()
        .filter(|l| l.contains("<RegistryValue ") && l.contains(" Name=\"InstallDir\""))
        .flat_map(|l| attributes(l, "<RegistryValue ", "Key"))
        .collect();
    assert_eq!(install_dir, [app.install_key().as_str()]);
    let searched: Vec<&str> = WXI
        .lines()
        .filter(|l| l.contains("<RegistrySearch ") && (l.contains(" Name=\"InstallDir\"") || l.contains(" Name=\"InstallFolder\"")))
        .flat_map(|l| attributes(l, "<RegistrySearch ", "Key"))
        .collect();
    assert_eq!(searched.len(), 4, "InstallDir and InstallFolder, for one account and for everyone: {searched:?}");
    assert!(searched.iter().all(|k| *k == app.install_key()), "{searched:?}");
}

#[test]
fn without_an_apple_team_a_macos_update_is_refused_with_a_download_link() {
    let app = &filmagic_ui_egui::UPDATE_APP;
    // Filmagic's releases aren't signed with an Apple Developer ID yet (docs/releasing.md).
    assert_eq!(app.apple_team, None, "set the Team ID only together with signed releases");
    let Err(why) = app.apple_team() else { panic!("an app without a team must not verify a macOS update") };
    assert!(why.contains(&app.releases_url()), "{why}");
}

#[test]
fn linux_packages_name_a_maintainer_of_this_repository() {
    let line = PACKAGE_SH.lines().find(|l| l.contains("FILMAGIC_MAINTAINER=")).unwrap_or_default();
    assert!(line.contains("${FILMAGIC_MAINTAINER:-"), "the environment can still override it: {line}");
    assert!(line.contains("Filmagic contributors <188127122+vansyson1308@users.noreply.github.com>"), "{line}");
}

/// Names of the upstream company, its products and its domains, lowercased.
const UPSTREAM_TERMS: [&str; 5] = ["ai.storyteller", "storyteller.ai", "learning machines", "artcraft", "getartcraft"];

/// Files outside `vendor/` that may name ArtCraft, and why. The other terms are allowed nowhere.
const UPSTREAM_NOTES: [(&str, &str); 14] = [
    ("AGENTS.md", "the rule against upstream brand marks"),
    ("NOTICE", "the upstream copyright and trademark notice"),
    ("LICENSE-MIT", "the upstream copyright line"),
    ("ATTRIBUTION.md", "the authors of vendored code"),
    ("ROADMAP.md", "history: the upstream naming update"),
    ("docs/gaps.md", "history: the upstream naming update"),
    ("docs/roadmap.md", "history: the upstream naming update"),
    ("docs/hardware-parity.md", "history: the upstream naming update"),
    ("docs/target-app-parity.md", "history: the upstream naming update"),
    ("docs/egui-037-migration-plan.md", "history: the upstream suite's migration"),
    ("crates/ui-egui/src/cjk.rs", "history: an upstream issue number"),
    ("crates/ui-egui/src/theme.rs", "history: an upstream issue number"),
    ("crates/ui-egui/src/panels/dialogs.rs", "About: the fork's credit to its upstream authors"),
    ("crates/ui-egui/src/i18n/", "translations of the About credit"),
];

/// Tests that name the upstream terms to check they appear nowhere else; any term is allowed there.
const UPSTREAM_CHECKS: [&str; 2] = ["crates/ui-egui/tests/it/links_ui.rs", "apps/filmagic/src/app_id_tests.rs"];

/// This repository's files, relative to its root: what `git ls-files` tracks, or, where git or the
/// `.git` folder is missing (a source tarball), a walk of the tree. Either way, `vendor/` and other
/// checkouts placed inside the tree (CI puts craft-fonts there for `CRAFT_FONTS_DIR`) are left out.
fn repository_files(root: &Path) -> Vec<PathBuf> {
    let tracked = std::process::Command::new("git").arg("-C").arg(root).args(["ls-files", "-z"]).output();
    if let Ok(out) = tracked
        && out.status.success()
        && !out.stdout.is_empty()
    {
        return out
            .stdout
            .split(|b| *b == 0)
            .filter_map(|name| std::str::from_utf8(name).ok())
            .filter(|name| !name.is_empty() && !name.starts_with("vendor/"))
            .map(PathBuf::from)
            .collect();
    }
    let mut files = Vec::new();
    files_under(root, Path::new(""), 0, &mut files);
    files
}

/// Every file under `dir` (relative to the repository), skipping build output, the vendored
/// code, the local notebook, version control and any other checkout inside the tree (a folder
/// with its own `.git`). Bounded in depth so a link loop can't run away.
fn files_under(root: &Path, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    const SKIP: [&str; 5] = [".git", "target", "vendor", "plan", "node_modules"];
    if depth > 16 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else { return };
    for entry in entries.flatten() {
        let rel = dir.join(entry.file_name());
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            let skipped = depth == 0 && SKIP.iter().any(|s| entry.file_name() == *s);
            if !skipped && !root.join(&rel).join(".git").exists() {
                files_under(root, &rel, depth + 1, out);
            }
        } else if kind.is_file() {
            out.push(rel);
        }
    }
}

#[test]
fn only_the_listed_notes_name_the_upstream_company() {
    let root = repo();
    let files = repository_files(&root);
    assert!(files.iter().any(|f| f == Path::new("packaging/linux/package.sh")), "the walk found the packaging");
    // The walk used without git leaves out other checkouts in the tree, as `git ls-files` does.
    let mut walked = Vec::new();
    files_under(&root, Path::new(""), 0, &mut walked);
    assert!(walked.iter().any(|f| f == Path::new("packaging/linux/package.sh")), "the fallback walk found the packaging");
    let mut found = Vec::new();
    for file in &files {
        let Ok(bytes) = std::fs::read(root.join(file)) else { continue };
        let Ok(text) = std::str::from_utf8(&bytes) else { continue };
        let lower = text.to_ascii_lowercase();
        let rel = file.to_string_lossy().replace('\\', "/");
        let noted = UPSTREAM_NOTES.iter().any(|(path, _)| if path.ends_with('/') { rel.starts_with(path) } else { rel == *path });
        for term in UPSTREAM_TERMS {
            let allowed = noted && term == "artcraft" || UPSTREAM_CHECKS.contains(&rel.as_str());
            if lower.contains(term) && !allowed {
                found.push(format!("{rel}: {term}"));
            }
        }
    }
    assert!(found.is_empty(), "upstream names outside vendor/ and the notes in UPSTREAM_NOTES:\n{}", found.join("\n"));
}
