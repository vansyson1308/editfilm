//! Asset packs: manifests and archives as hostile input, atomic install and remove, building a
//! pack from a folder, search, and placement on the timeline. All media is made here.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::manifest::{self, Kind};
use crate::Session;
use crate::media_test_util::{make_movie, session_with, tmp_dir};

fn wav(path: &Path, seconds: f32, amp: f32) {
    let rate = 48_000u32;
    let n = (seconds * rate as f32) as usize;
    let s: Vec<f32> = (0..n).map(|i| amp * (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin()).collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, crate::voiceover::write_wav_f32(&s, rate)).unwrap();
}

/// A 64×36 picture: a red square on `bg` (RGBA).
fn png(path: &Path, bg: [u8; 4]) {
    let img = image::RgbaImage::from_fn(
        64,
        36,
        |x, y| if (24..40).contains(&x) && (10..26).contains(&y) { image::Rgba([220, 30, 30, 255]) } else { image::Rgba(bg) },
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    img.save(path).unwrap();
}

fn session(data: &Path) -> Session {
    let mut s = Session::default();
    s.packs.set_dir(data);
    s
}

/// A folder of one's own files, as the owner keeps memes and sounds.
fn own_folder(root: &Path) -> PathBuf {
    let f = root.join("My Memes");
    wav(&f.join("music/Nhạc Vui Epic.wav"), 50.0, 0.2);
    wav(&f.join("sfx/whoosh_01.wav"), 0.6, 0.5);
    wav(&f.join("sfx/bigBoom.wav"), 1.2, 0.5);
    png(&f.join("memes/green screen/bruh.png"), [0, 200, 40, 255]);
    png(&f.join("memes/cat transparent.png"), [0, 0, 0, 0]);
    png(&f.join("overlays/leak.png"), [250, 200, 120, 255]);
    std::fs::write(f.join("memes/Kenney.url"), "[InternetShortcut]\nURL=https://example.com\n").unwrap();
    std::fs::write(f.join("notes.docx"), b"not media").unwrap();
    f
}

fn item<'a>(ix: &'a super::store::Index, pack: &str, id: &str) -> &'a manifest::Item {
    &ix.item(pack, id).unwrap_or_else(|| panic!("no {pack}/{id} in {:?}", ix.items.iter().map(|i| &i.item.id).collect::<Vec<_>>())).item
}

// ----------------------------------------------------------------------------- manifest

#[test]
fn unsafe_paths_and_ids_are_refused() {
    for p in [
        "",
        "../x.wav",
        "a/../../x.wav",
        "/etc/passwd",
        "C:/x.wav",
        "c:x.wav",
        "a\\b.wav",
        "a//b.wav",
        "./a.wav",
        "a/./b.wav",
        "CON",
        "sfx/nul.wav",
        "com1.ogg",
        "LPT9.txt",
        "a.wav.",
        "a.wav ",
        "a<b.wav",
        "a|b.wav",
        "x?.wav",
        "x*.wav",
        "a\u{0}b",
        "file.wav:stream",
    ] {
        assert!(manifest::safe_rel_path(p).is_err(), "{p:?} must be refused");
    }
    let deep = vec!["d"; 17].join("/");
    assert!(manifest::safe_rel_path(&deep).is_err());
    assert!(manifest::safe_rel_path(&"x".repeat(300)).is_err());
    for p in ["a.wav", "sfx/whoosh 01.wav", "memes/Nhạc vui.mp4", "console.wav", "com10.wav"] {
        assert!(manifest::safe_rel_path(p).is_ok(), "{p:?} is fine");
    }
    for id in ["", ".hidden", "-x", "a/b", "a b", "con", "x".repeat(129).as_str(), "nhạc"] {
        assert!(manifest::check_id(id).is_err(), "{id:?}");
    }
    assert!(manifest::check_id("kenney-interface-sounds").is_ok());
}

fn manifest_json(items: Value) -> Value {
    json!({"format": 1, "id": "p", "name": "P", "license": "CC0-1.0", "source": "own", "items": items})
}

fn validate(v: Value) -> Result<(manifest::Manifest, Vec<String>), String> {
    let mut m = manifest::parse(&serde_json::to_vec(&v).unwrap())?;
    let w = manifest::validate(&mut m)?;
    Ok((m, w))
}

#[test]
fn hostile_manifests_are_refused_or_cleaned() {
    let it = |extra: Value| {
        let mut o = json!({"id": "a", "file": "a.wav", "kind": "sfx"});
        for (k, v) in extra.as_object().unwrap() {
            o[k] = v.clone();
        }
        manifest_json(json!([o]))
    };
    // dropped with a warning: out-of-range numbers and a malformed colour
    let (m, w) = validate(it(json!({"duration": 1e300, "loudness": -1000.0, "keyColor": "green", "tags": ["  Whoosh ", "WHOOSH", "Nhạc"]}))).unwrap();
    assert_eq!((m.items[0].duration, m.items[0].loudness, m.items[0].key_color.clone()), (None, None, None));
    assert_eq!(w.len(), 3, "{w:?}");
    assert_eq!(m.items[0].tags, ["whoosh", "nhac"]);
    // refused
    for (bad, why) in [
        (it(json!({"url": "http://example.com/a.wav", "sha256": "0".repeat(64)})), "https"),
        (it(json!({"url": "https://example.com/a.wav"})), "sha256"),
        (it(json!({"url": "https://example.com/a.wav", "sha256": "xyz"})), "hex"),
        (it(json!({"url": "https://example.com/a.wav", "sha256": "0".repeat(64), "size": u64::MAX})), "MiB"),
        (it(json!({"file": "../../evil.wav"})), "`..`"),
        (it(json!({"id": "../x"})), "item id"),
        (it(json!({"kind": "virus"})), "valid pack manifest"),
        (json!({"format": 99, "id": "p", "name": "P", "license": "x", "source": "y"}), "update Filmagic"),
        (json!({"format": 1, "id": "p", "name": "P", "license": "", "source": "y"}), "license"),
        (json!({"format": 1, "id": "CON", "name": "P", "license": "x", "source": "y"}), "reserved"),
        (manifest_json(json!([{"id": "a", "file": "a.wav", "kind": "sfx"}, {"id": "a", "file": "b.wav", "kind": "sfx"}])), "twice"),
        (manifest_json(json!([{"id": "a", "file": "a.wav", "kind": "sfx"}, {"id": "b", "file": "A.WAV", "kind": "sfx"}])), "twice"),
        (json!("not an object"), "valid pack manifest"),
    ] {
        let e = validate(bad.clone()).expect_err(&bad.to_string());
        assert!(e.contains(why), "{e} (wanted {why}) for {bad}");
    }
    let many: Vec<Value> = (0..manifest::MAX_ITEMS + 1).map(|i| json!({"id": format!("i{i}"), "file": format!("{i}.wav"), "kind": "sfx"})).collect();
    assert!(validate(manifest_json(json!(many))).unwrap_err().contains("at most"));
    assert!(manifest::parse(&vec![b' '; (manifest::MAX_MANIFEST_BYTES + 1) as usize]).unwrap_err().contains("larger"));
    // http to this machine only
    assert!(manifest::check_url("http://127.0.0.1:8080/p.zip").is_ok());
    assert!(manifest::check_url("http://localhost/p.zip").is_ok());
    for u in ["http://localhost.evil.com/p.zip", "http://user@evil.com/p", "ftp://x/y", "file:///etc/passwd", "https://", "https://a b"] {
        assert!(manifest::check_url(u).is_err(), "{u}");
    }
}

#[test]
fn words_fold_accents_and_split_names() {
    assert_eq!(manifest::words("bigBoom_02 Nhạc-Đám"), ["big", "boom", "02", "nhac", "dam"]);
    assert_eq!(manifest::slug("Nhạc nền Vui!", "x"), "nhac-nen-vui");
    assert_eq!(manifest::slug("!!!", "fallback"), "fallback");
    assert_eq!(manifest::slug("con", "fallback"), "fallback", "a Windows device name is no folder name");
}

// ----------------------------------------------------------------------------- build & install

#[test]
fn a_folder_of_own_files_becomes_a_tagged_keyed_pack() {
    let root = tmp_dir("assets-build");
    let data = root.join("data");
    let mut s = session(&data);
    let from = own_folder(&root);
    let r = s.execute("assets.buildPack", json!({"from": from.to_string_lossy()})).unwrap();
    assert_eq!(r["pack"]["id"], "my-memes", "{r}");
    assert_eq!(r["built"]["items"], 6, "{r}");
    let ix = s.packs.index().unwrap();
    let music = item(&ix, "my-memes", "music-nhac-vui-epic");
    assert_eq!(music.kind, Kind::Music);
    assert!(music.mood.contains(&"epic".to_string()) && music.mood.contains(&"vui".to_string()), "{:?}", music.mood);
    assert!((music.duration.unwrap() - 50.0).abs() < 0.01);
    let l = music.loudness.unwrap();
    assert!((-25.0..-10.0).contains(&l), "a 0.2 sine is about -17 LUFS, got {l}");
    let whoosh = item(&ix, "my-memes", "sfx-whoosh-01");
    assert_eq!(whoosh.kind, Kind::Sfx);
    assert!(whoosh.tags.contains(&"whoosh".to_string()) && !whoosh.tags.contains(&"01".to_string()), "{:?}", whoosh.tags);
    assert!(item(&ix, "my-memes", "sfx-big-boom").tags.contains(&"boom".to_string()));
    let bruh = item(&ix, "my-memes", "memes-green-screen-bruh");
    assert_eq!(bruh.kind, Kind::MemeImage);
    let key = manifest::parse_hex_color(bruh.key_color.as_deref().unwrap()).unwrap();
    assert!(key[1] > 150 && key[0] < 40 && key[2] < 80, "{key:?}");
    assert!(!bruh.alpha);
    let cat = item(&ix, "my-memes", "memes-cat-transparent");
    assert!(cat.alpha && cat.key_color.is_none());
    assert_eq!(item(&ix, "my-memes", "overlays-leak").kind, Kind::Overlay);
    // a copy is installed; the shortcut and the document stay behind
    let pack = data.join("Packs/my-memes");
    assert!(pack.join("sfx/whoosh_01.wav").is_file() && pack.join("pack.json").is_file());
    assert!(!pack.join("memes/Kenney.url").exists() && !pack.join("notes.docx").exists());
    assert!(r["notes"].to_string().contains("own files"), "the own-files licence is pointed out: {r}");
    // the source folder is left as it was
    assert!(!from.join("pack.json").exists());
    // the index is one file across packs
    let index: Value = serde_json::from_slice(&std::fs::read(data.join("Packs/index.json")).unwrap()).unwrap();
    assert_eq!(index["items"].as_array().unwrap().len(), 6);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_manifest_written_into_the_folder_keeps_edits_on_the_next_build() {
    let root = tmp_dir("assets-manifest-only");
    let mut s = session(&root.join("data"));
    let from = own_folder(&root);
    let r = s.execute("assets.buildPack", json!({"from": from.to_string_lossy(), "install": false, "id": "memes", "license": "CC-BY-4.0"})).unwrap();
    assert!(from.join("pack.json").is_file(), "{r}");
    assert!(s.packs.index().unwrap().packs.is_empty(), "install:false installs nothing");
    // edit a tag and a kind by hand, then build again: the edits stay
    let mut m: Value = serde_json::from_slice(&std::fs::read(from.join("pack.json")).unwrap()).unwrap();
    let items = m["items"].as_array_mut().unwrap();
    let boom = items.iter_mut().find(|i| i["id"] == "sfx-big-boom").unwrap();
    boom["tags"] = json!(["explosion"]);
    boom["kind"] = json!("ambience");
    std::fs::write(from.join("pack.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    s.execute("assets.buildPack", json!({"from": from.to_string_lossy()})).unwrap();
    let ix = s.packs.index().unwrap();
    let boom = item(&ix, "memes", "sfx-big-boom");
    assert_eq!((boom.tags.clone(), boom.kind), (vec!["explosion".to_string()], Kind::Ambience));
    assert_eq!(ix.pack("memes").unwrap().license, "CC-BY-4.0");
    // installing the folder with its manifest gives the same pack
    let r = s.execute("assets.install", json!({"path": from.to_string_lossy(), "replace": true})).unwrap();
    assert_eq!((r["pack"]["id"].as_str(), r["pack"]["items"].as_u64()), (Some("memes"), Some(6)), "the manifest's id, not the folder's name: {r}");
    assert!(s.packs.index().unwrap().pack("my-memes").is_none());
    let _ = std::fs::remove_dir_all(root);
}

/// Write a zip of `(name, bytes)` entries.
fn zip(path: &Path, entries: &[(&str, &[u8])]) {
    let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (n, b) in entries {
        w.start_file(*n, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
        w.write_all(b).unwrap();
    }
    w.finish().unwrap();
}

#[test]
fn zips_cannot_write_outside_the_pack() {
    let root = tmp_dir("assets-zipslip");
    let data = root.join("data");
    let mut s = session(&data);
    wav(&root.join("good.wav"), 0.3, 0.3);
    let good = std::fs::read(root.join("good.wav")).unwrap();
    let z = root.join("evil.zip");
    zip(
        &z,
        &[
            ("../../evil.wav", &good),
            ("/abs.wav", &good),
            ("C:/win.wav", &good),
            ("a\\..\\..\\back.wav", &good),
            ("sfx/CON.wav", &good),
            ("run.exe", b"MZ"),
            ("sfx/good.wav", &good),
        ],
    );
    let r = s.execute("assets.install", json!({"path": z.to_string_lossy(), "license": "CC0-1.0"})).unwrap();
    assert_eq!(r["pack"]["items"], 1, "{r}");
    assert!(r["notes"].to_string().contains("left out 6 files"), "{r}");
    for escaped in ["evil.wav", "data/evil.wav", "data/Packs/evil.wav", "abs.wav", "back.wav", "win.wav"] {
        assert!(!root.join(escaped).exists(), "{escaped} was written outside the pack");
    }
    assert!(!data.join("Packs/evil/run.exe").exists());
    assert!(data.join("Packs/evil/sfx/good.wav").is_file());
    // a zip holding nothing usable installs nothing and leaves no staging behind
    let z2 = root.join("nothing.zip");
    zip(&z2, &[("../x.wav", &good), ("readme.url", b"x")]);
    let e = s.execute("assets.install", json!({"path": z2.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("no media"), "{e}");
    assert_eq!(std::fs::read_dir(data.join("Packs/.staging")).unwrap().count(), 0);
    // a duplicated entry is refused; garbage is refused
    let z3 = root.join("dup.zip");
    zip(&z3, &[("a.wav", &good), ("A.WAV", &good)]);
    assert!(s.execute("assets.install", json!({"path": z3.to_string_lossy()})).unwrap_err().to_string().contains("twice"));
    std::fs::write(root.join("junk.zip"), b"PK\x03\x04 this is not a zip").unwrap();
    assert!(s.execute("assets.install", json!({"path": root.join("junk.zip").to_string_lossy()})).unwrap_err().to_string().contains("zip"));
    let _ = std::fs::remove_dir_all(root);
}

/// Mutated archives (cut short, bytes flipped) are errors, never a panic or a half-installed pack.
#[test]
fn damaged_zips_fail_cleanly() {
    let root = tmp_dir("assets-zipfuzz");
    let data = root.join("data");
    let mut s = session(&data);
    wav(&root.join("x.wav"), 0.2, 0.3);
    let good = std::fs::read(root.join("x.wav")).unwrap();
    let z = root.join("ok.zip");
    zip(&z, &[("pack/sfx/a.wav", &good), ("pack/sfx/b.wav", &good)]);
    let bytes = std::fs::read(&z).unwrap();
    let mut seed = 0x2545_f491u32;
    for round in 0..60 {
        let mut b = bytes.clone();
        if round % 3 == 0 {
            b.truncate(round * bytes.len() / 60);
        } else {
            for _ in 0..4 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let i = seed as usize % b.len();
                b[i] ^= 1 << (seed % 8);
            }
        }
        let p = root.join(format!("m{round}.zip"));
        std::fs::write(&p, &b).unwrap();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute("assets.install", json!({"path": p.to_string_lossy(), "replace": true}))));
        assert!(r.is_ok(), "round {round} panicked");
        if r.unwrap().is_ok() {
            // whatever installed is whole and indexed
            let ix = s.packs.index().unwrap();
            assert!(ix.problems.is_empty(), "{:?}", ix.problems);
        }
    }
    assert_eq!(std::fs::read_dir(data.join("Packs/.staging")).map(|r| r.count()).unwrap_or(0), 0);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn install_and_remove_are_atomic() {
    let root = tmp_dir("assets-atomic");
    let data = root.join("data");
    let mut s = session(&data);
    let from = own_folder(&root);
    s.execute("assets.buildPack", json!({"from": from.to_string_lossy(), "id": "a"})).unwrap();
    // the same id again needs `replace`, and the installed pack is untouched
    let e = s.execute("assets.buildPack", json!({"from": from.to_string_lossy(), "id": "a"})).unwrap_err().to_string();
    assert!(e.contains("replace"), "{e}");
    assert!(data.join("Packs/a/pack.json").is_file());
    // a failing replacement leaves the old pack in place
    let broken = root.join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("pack.json"), serde_json::to_vec(&manifest_json(json!([{"id": "x", "file": "missing.wav", "kind": "sfx"}]))).unwrap()).unwrap();
    let e = s.execute("assets.install", json!({"path": broken.to_string_lossy(), "id": "a", "replace": true})).unwrap_err().to_string();
    assert!(e.contains("does not hold"), "{e}");
    assert_eq!(s.packs.index().unwrap().pack("a").unwrap().items, 6);
    // a hand-made folder without a manifest shows as a problem, not a crash
    std::fs::create_dir_all(data.join("Packs/stray")).unwrap();
    let r = s.execute("assets.list", json!({})).unwrap();
    assert_eq!(r["problems"][0]["folder"], "stray", "{r}");
    // remove
    let r = s.execute("assets.remove", json!({"pack": "a"})).unwrap();
    assert_eq!(r["removed"], "a");
    assert!(!data.join("Packs/a").exists());
    assert!(s.packs.index().unwrap().pack("a").is_none());
    for (bad, why) in [
        (json!({"pack": "a"}), "no installed pack"),
        (json!({"pack": "../data"}), "pack id"),
        (json!({"pack": "CON"}), "reserved"),
        (json!({"pack": 7}), "string"),
        (json!({}), "need `pack`"),
    ] {
        let e = s.execute("assets.remove", bad.clone()).unwrap_err().to_string();
        assert!(e.contains(why), "{e} for {bad}");
    }
    assert!(data.exists() && root.join("My Memes").exists(), "remove deletes only the pack");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn install_parameters_are_checked() {
    let root = tmp_dir("assets-params");
    let mut s = session(&root.join("data"));
    for (p, why) in [
        (json!({}), "exactly one"),
        (json!({"path": "/x", "url": "https://x/y.zip"}), "exactly one"),
        (json!({"path": 5}), "string"),
        (json!({"path": "/no/such/folder"}), "not a folder or file"),
        (json!({"catalog": "nope"}), "no catalogue entry"),
        (json!({"url": "https://example.com/p.zip"}), "sha256"),
        (json!({"url": "https://example.com/p.zip", "sha256": "abc"}), "64 hex"),
        (json!({"url": "ftp://example.com/p.zip", "sha256": "0".repeat(64)}), "https"),
        (json!({"url": "https://example.com/p.zip", "sha256": "0".repeat(64), "size": -1}), "bytes"),
        (json!({"path": "/x", "id": "../up"}), "`id`"),
        (json!({"path": "/x", "replace": "yes"}), "true or false"),
    ] {
        let e = s.execute("assets.install", p.clone()).unwrap_err().to_string();
        assert!(e.contains(why), "{e} (wanted {why}) for {p}");
    }
    // without a data directory every command says how to get one
    let mut bare = Session::default();
    let e = bare.execute("assets.list", json!({})).unwrap_err().to_string();
    assert!(e.contains("--data-dir"), "{e}");
    let _ = std::fs::remove_dir_all(root);
}

// ----------------------------------------------------------------------------- search

#[test]
fn search_finds_by_word_kind_mood_and_length() {
    let root = tmp_dir("assets-search");
    let mut s = session(&root.join("data"));
    s.execute("assets.buildPack", json!({"from": own_folder(&root).to_string_lossy()})).unwrap();
    let ids = |r: &Value| r["items"].as_array().unwrap().iter().map(|i| i["asset"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    let r = s.execute("assets.search", json!({"query": "whoosh"})).unwrap();
    assert_eq!(ids(&r), ["my-memes/sfx-whoosh-01"]);
    assert!(Path::new(r["items"][0]["path"].as_str().unwrap()).is_file());
    // typed without accents, the Vietnamese name is found
    assert_eq!(ids(&s.execute("assets.search", json!({"query": "nhac vui"})).unwrap()), ["my-memes/music-nhac-vui-epic"]);
    assert_eq!(ids(&s.execute("assets.search", json!({"query": "Nhạc"})).unwrap()), ["my-memes/music-nhac-vui-epic"]);
    assert_eq!(ids(&s.execute("assets.search", json!({"kind": "music"})).unwrap()), ["my-memes/music-nhac-vui-epic"]);
    assert_eq!(ids(&s.execute("assets.search", json!({"mood": "epic"})).unwrap()), ["my-memes/music-nhac-vui-epic"]);
    assert_eq!(s.execute("assets.search", json!({"kind": ["sfx", "music"]})).unwrap()["total"], 3);
    assert_eq!(s.execute("assets.search", json!({"kind": "sfx", "maxSeconds": 1.0})).unwrap()["total"], 1);
    assert_eq!(ids(&s.execute("assets.search", json!({"alpha": true, "kind": ["meme-image", "meme-gif"]})).unwrap()).len(), 2, "transparent or keyable");
    let r = s.execute("assets.search", json!({"query": "zebra"})).unwrap();
    assert_eq!(r["total"], 0);
    assert!(r["notes"][0].as_str().unwrap().contains("fewer words"));
    assert_eq!(s.execute("assets.search", json!({"limit": 1})).unwrap()["items"].as_array().unwrap().len(), 1);
    for (p, why) in [
        (json!({"kind": "virus"}), "unknown kind"),
        (json!({"kind": 3}), "kind"),
        (json!({"kind": ["sfx", 1]}), "kind"),
        (json!({"limit": 0}), "1 to 500"),
        (json!({"limit": 1e9}), "1 to 500"),
        (json!({"limit": -3}), "1 to 500"),
        (json!({"minSeconds": -1}), "0 or more"),
        (json!({"maxSeconds": "long"}), "seconds"),
        (json!({"query": 5}), "string"),
        (json!({"pack": "nope"}), "no installed pack"),
        (json!({"alpha": 1}), "true or false"),
    ] {
        let e = s.execute("assets.search", p.clone()).unwrap_err().to_string();
        assert!(e.contains(why), "{e} (wanted {why}) for {p}");
    }
    let _ = std::fs::remove_dir_all(root);
}

// ----------------------------------------------------------------------------- place

/// A session with a 2 s clip on V1/A1 whose sound is typed Dialogue, and a built pack.
fn edit_session(root: &Path) -> (Session, u64) {
    let mov = root.join("talk.mov");
    make_movie(&mov, filmagic_media::DemoScene::Aurora, 64, 36, 48);
    let (mut s, _, _) = session_with(&[&mov]);
    s.packs.set_dir(&root.join("data"));
    let a1 = s.active_sequence().unwrap().audio_tracks[0].items[0].id.0;
    s.execute("essentialSound.setType", json!({"clips": [a1], "type": "dialogue"})).unwrap();
    s.execute("assets.buildPack", json!({"from": own_folder(root).to_string_lossy()})).unwrap();
    (s, a1)
}

#[test]
fn music_goes_on_a_free_track_as_music_and_ducks_under_dialogue() {
    let root = tmp_dir("assets-place-music");
    let (mut s, _) = edit_session(&root);
    let undo_before = s.history.undo.len();
    let r = s.execute("assets.place", json!({"asset": "my-memes/music-nhac-vui-epic", "seconds": 0.0, "untilEnd": true})).unwrap();
    assert_eq!(r["audioType"], "music", "{r}");
    assert_eq!(r["kind"], "music");
    let seq = s.active_sequence().unwrap();
    let a = r["audioClips"][0].as_u64().unwrap();
    let (t, clip) = seq.find_item(filmagic_project::ClipId(a)).unwrap();
    assert_ne!(t, seq.audio_tracks[0].id, "not on the dialogue track");
    assert!(seq.video_tracks.iter().all(|t| t.items.iter().all(|i| i.id.0 != a)), "music has no picture");
    assert_eq!(clip.range().end(), seq.duration(), "untilEnd stops at the sequence end");
    assert_eq!(clip.essential.as_ref().unwrap().kind, filmagic_project::essential::AudioType::Music);
    let lvl = clip.effect("volume").and_then(|e| e.param("level")).unwrap();
    assert!(r["ducking"]["clips"][0]["regions"].as_array().is_some_and(|v| !v.is_empty()), "{r}");
    assert!(lvl.is_animated(), "ducking keyframes on Volume");
    assert_eq!(s.history.undo.len(), undo_before + 1, "one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().find_item(filmagic_project::ClipId(a)).is_none());
    // time objects (#13's convention): `time` and `duration` in seconds, frames or timecode
    let r = s.execute("assets.place", json!({"asset": "my-memes/sfx-whoosh-01", "time": {"seconds": 0.5}, "duration": {"frame": 6}})).unwrap();
    let rate = s.sequence_rate();
    let (_, c) = s.active_sequence().unwrap().find_item(filmagic_project::ClipId(r["audioClips"][0].as_u64().unwrap())).unwrap();
    assert_eq!((c.start, c.range().duration), (rate.snap_nearest(filmagic_time::Tick::from_seconds_f64(0.5)), rate.tick_of(6)));
    s.execute("edit.undo", json!({})).unwrap();
    // placing twice imports the file once
    s.execute("assets.place", json!({"asset": "my-memes/sfx-whoosh-01", "seconds": 0.5})).unwrap();
    let n = s.project.items.len();
    let r2 = s.execute("assets.place", json!({"asset": "my-memes/sfx-whoosh-01", "seconds": 0.5})).unwrap();
    assert_eq!(s.project.items.len(), n);
    assert_eq!(r2["audioType"], "sfx");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn green_screen_memes_are_keyed_above_the_footage() {
    let root = tmp_dir("assets-place-meme");
    let (mut s, _) = edit_session(&root);
    let r = s.execute("assets.place", json!({"asset": "my-memes/memes-green-screen-bruh", "seconds": 0.5, "duration": {"seconds": 1.0}})).unwrap();
    let v = r["videoClip"].as_u64().unwrap();
    let seq = s.active_sequence().unwrap();
    let (t, clip) = seq.find_item(filmagic_project::ClipId(v)).unwrap();
    assert_ne!(t, seq.video_tracks[0].id, "over the footage, not in it");
    let key = clip.effect("ultra_key").expect("keyed");
    let want = r["keyed"].as_str().unwrap();
    assert_eq!(key.param("key_color").unwrap().value, filmagic_project::ParamValue::Color(filmagic_color::parse_hex(want).unwrap()));
    assert_eq!(clip.range().duration, s.sequence_rate().snap_nearest(filmagic_time::Tick::from_seconds_f64(1.0)));
    // a transparent picture is not keyed; `key:false` turns keying off; a forced key needs a colour
    let r = s.execute("assets.place", json!({"asset": "my-memes/memes-cat-transparent", "seconds": 0.0})).unwrap();
    assert!(r["keyed"].is_null(), "{r}");
    let r = s.execute("assets.place", json!({"asset": "my-memes/memes-green-screen-bruh", "seconds": 0.0, "key": false})).unwrap();
    assert!(r["keyed"].is_null());
    let e = s.execute("assets.place", json!({"asset": "my-memes/overlays-leak", "key": true})).unwrap_err().to_string();
    assert!(e.contains("keyColor"), "{e}");
    let r = s.execute("assets.place", json!({"asset": "my-memes/overlays-leak", "key": true, "keyColor": "#F0C080", "track": "new"})).unwrap();
    assert_eq!(r["keyed"], "#f0c080");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn hostile_place_parameters_change_nothing() {
    let root = tmp_dir("assets-place-hostile");
    let (mut s, _) = edit_session(&root);
    let rev = s.revision;
    let undo = s.history.undo.len();
    for (p, why) in [
        (json!({}), "need `asset`"),
        (json!({"asset": 5}), "string"),
        (json!({"asset": "nopack"}), "pack/item"),
        (json!({"asset": "../my-memes/x"}), "pack/item"),
        (json!({"asset": "my-memes/../../etc"}), "pack/item"),
        (json!({"asset": "other/x"}), "no installed pack"),
        (json!({"asset": "my-memes/nothing"}), "has no item"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "seconds": -1.0}), "negative"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "duration": 0}), "longer than 0"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "duration": -5}), "longer than 0"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "duration": "long"}), "must be ticks"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "time": {"seconds": f64::MAX}}), "time"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "time": {"frame": 1.5}}), "frame"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "duration": 10, "untilEnd": true}), "not both"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "untilEnd": 1}), "true or false"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "keyColor": "zz"}), "#rrggbb"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "track": "above"}), "no track above"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "track": [1]}), "track name"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "audioTrack": "A9"}), "no track A9"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "track": 999_999}), "track"),
        (json!({"asset": "my-memes/sfx-whoosh-01", "seconds": 1e300, "untilEnd": true}), "ends before"),
    ] {
        let e = s.execute("assets.place", p.clone()).expect_err(&p.to_string()).to_string();
        assert!(e.contains(why), "{e} (wanted {why}) for {p}");
        assert_eq!((s.revision, s.history.undo.len()), (rev, undo), "{p} changed the project");
    }
    // no sequence open: the command is disabled with a reason
    let mut bare = session(&root.join("data"));
    let e = bare.execute("assets.place", json!({"asset": "my-memes/sfx-whoosh-01"})).unwrap_err().to_string();
    assert!(e.contains("no sequence"), "{e}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn removing_a_pack_reports_offline_project_items() {
    let root = tmp_dir("assets-remove-used");
    let (mut s, _) = edit_session(&root);
    s.execute("assets.place", json!({"asset": "my-memes/sfx-whoosh-01"})).unwrap();
    let r = s.execute("assets.remove", json!({"pack": "my-memes"})).unwrap();
    assert!(r["warnings"][0].as_str().unwrap().contains("offline"), "{r}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_catalogue_is_pinned() {
    let mut s = session(&tmp_dir("assets-catalog"));
    let r = s.execute("assets.catalog", json!({})).unwrap();
    assert!(!r["entries"].as_array().unwrap().is_empty());
    for e in super::catalog::CATALOG {
        assert!(e.url.starts_with("https://") && manifest::check_url(e.url).is_ok(), "{}", e.id);
        assert_eq!(manifest::check_sha256(e.sha256).unwrap(), e.sha256, "{}", e.id);
        assert!(e.size > 0 && e.size < super::store::MAX_DOWNLOAD_BYTES);
        assert!(manifest::check_id(e.id).is_ok() && Kind::parse(e.kind).is_some());
        // only licences of the house allowlist that need no share-alike for use in a video
        assert!(["CC0-1.0", "CC-BY-4.0"].contains(&e.license), "{}", e.id);
        let attribution = include_str!("../../../../ATTRIBUTION.md");
        assert!(attribution.contains(e.url) && attribution.contains(e.sha256), "{} must be in ATTRIBUTION.md", e.id);
    }
}

// ----------------------------------------------------------------------------- downloads

/// Serve `body` once per connection on 127.0.0.1 (plain HTTP to this machine is allowed).
#[cfg(feature = "pack-download")]
fn serve(body: Vec<u8>, hits: usize) -> String {
    use std::io::Read;
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..hits {
            let Ok((mut c, _)) = l.accept() else { return };
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = c.write_all(&body);
        }
    });
    format!("http://127.0.0.1:{}/pack", addr.port())
}

#[cfg(feature = "pack-download")]
fn sha(b: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(feature = "pack-download")]
#[test]
fn downloads_are_installed_only_when_their_sha256_matches() {
    let root = tmp_dir("assets-download");
    let data = root.join("data");
    let mut s = session(&data);
    wav(&root.join("a.wav"), 0.3, 0.3);
    let z = root.join("p.zip");
    zip(&z, &[("Boom Pack/boom.wav", &std::fs::read(root.join("a.wav")).unwrap())]);
    let body = std::fs::read(&z).unwrap();
    let url = serve(body.clone(), 2);
    let e = s.execute("assets.install", json!({"url": url, "sha256": "0".repeat(64), "license": "CC0-1.0"})).unwrap_err().to_string();
    assert!(e.contains("checksum mismatch"), "{e}");
    assert!(s.packs.index().unwrap().packs.is_empty());
    let r = s.execute("assets.install", json!({"url": url, "sha256": sha(&body), "id": "boom", "license": "CC0-1.0"})).unwrap();
    assert_eq!(r["pack"]["items"], 1, "{r}");
    assert_eq!(r["pack"]["source"], url.as_str());
    assert!(data.join("Packs/boom/Boom Pack/boom.wav").is_file(), "the archive's folders are kept");
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(feature = "pack-download")]
#[test]
fn remote_items_download_when_first_placed() {
    let root = tmp_dir("assets-lazy");
    let (mut s, _) = edit_session(&root);
    wav(&root.join("cheer.wav"), 0.5, 0.4);
    let body = std::fs::read(root.join("cheer.wav")).unwrap();
    let url = serve(body.clone(), 1);
    let m = json!({"format": 1, "id": "remote", "name": "Remote", "license": "CC0-1.0", "source": "test", "items": [
        {"id": "cheer", "file": "crowd/cheer.wav", "kind": "sfx", "tags": ["crowd", "cheer"], "url": url, "sha256": sha(&body), "size": body.len()}
    ]});
    let mdir = root.join("manifest");
    std::fs::create_dir_all(&mdir).unwrap();
    std::fs::write(mdir.join("pack.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    let r = s.execute("assets.install", json!({"path": mdir.join("pack.json").to_string_lossy()})).unwrap();
    assert_eq!(r["pack"]["remote"], 1, "nothing downloaded yet: {r}");
    let file = root.join("data/Packs/remote/crowd/cheer.wav");
    assert!(!file.exists());
    let r = s.execute("assets.place", json!({"asset": "remote/cheer", "seconds": 0.0})).unwrap();
    assert!(file.is_file(), "{r}");
    assert_eq!(s.packs.index().unwrap().pack("remote").unwrap().remote, 0);
    let _ = std::fs::remove_dir_all(root);
}
