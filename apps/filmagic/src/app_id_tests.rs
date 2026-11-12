//! The app identifier is one value everywhere. Wayland and the desktop shells match the window to
//! the `.desktop` file by it, Flatpak and AppStream key the app by it, the macOS bundle and project
//! type carry it, and the updater (`UPDATE_APP.bundle_id()`) uses it to recognise its own Flatpak,
//! the menu entry beside an AppImage and the running `.app`. If any of them drifts, the app loses
//! its icon, its file type or its updates without an error, so these tests fail instead.

use std::path::{Path, PathBuf};

use crate::APP_ID;

const DESKTOP: &str = include_str!("../../../packaging/linux/io.github.vansyson1308.editfilm.desktop");
const METAINFO: &str = include_str!("../../../packaging/linux/io.github.vansyson1308.editfilm.metainfo.xml.in");
const MIME: &str = include_str!("../../../packaging/linux/io.github.vansyson1308.editfilm.mime.xml");
const FLATPAK: &str = include_str!("../../../packaging/linux/flatpak/io.github.vansyson1308.editfilm.yml");
const FLATPAK_BUNDLE: &str = include_str!("../../../packaging/linux/flatpak/io.github.vansyson1308.editfilm.bundle.yml");
const INFO_PLIST: &str = include_str!("../../../packaging/macos/Info.plist.in");
const OMASTORE: &str = include_str!("../../../omastore.toml");
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
    let files = [DESKTOP, METAINFO, MIME, FLATPAK, FLATPAK_BUNDLE, INFO_PLIST, OMASTORE].into_iter().chain(SCRIPTS.map(|(_, text, _)| text));
    for text in files {
        assert!(!text.contains("ai.storyteller"), "{text}");
        assert!(!text.contains("ArtCraft"), "{text}");
    }
}
