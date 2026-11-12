//! `cargo xtask icns <out.icns> <in.png>…`: pack PNG images into a macOS `.icns`, so the icon
//! can be built on any OS (macOS `iconutil` is not needed).
//!
//! The format, as Apple documents it for icon families: a big-endian header (`icns`, then the
//! file length) followed by elements, each a four-byte type code, a big-endian length that counts
//! its own eight-byte header, and the payload. Since macOS 10.7 the payload of the types below is
//! a PNG file, stored as is. Each input is matched to its type codes by the size in its IHDR chunk;
//! one PNG can fill two codes (a 32 px image is both the 32 px icon and the 16 px @2x one). A
//! `TOC ` element listing the other elements comes first, as `iconutil` writes it.

use std::path::Path;

use crate::ico::png_size;

/// PNG element types: (type code, pixel size). `ic11`–`ic14` and `ic10` are the @2x
/// (Retina) variants of 16, 32, 128, 256 and 512 pt.
pub const TYPES: &[(&[u8; 4], u32)] = &[
    (b"icp4", 16),
    (b"icp5", 32),
    (b"icp6", 64),
    (b"ic07", 128),
    (b"ic08", 256),
    (b"ic09", 512),
    (b"ic10", 1024),
    (b"ic11", 32),
    (b"ic12", 64),
    (b"ic13", 256),
    (b"ic14", 512),
];

fn be32(n: usize, what: &str) -> Result<[u8; 4], String> {
    u32::try_from(n).map(u32::to_be_bytes).map_err(|_| format!("{what} is too large for an icns file"))
}

/// An icns file holding `pngs`, which must be square, of distinct sizes, each one of
/// 16, 32, 64, 128, 256, 512 or 1024 px.
pub fn pack(pngs: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    if pngs.is_empty() {
        return Err("need at least one PNG".into());
    }
    let mut sized: Vec<(u32, &[u8])> = Vec::with_capacity(pngs.len());
    for png in pngs {
        let (w, h) = png_size(png)?;
        if w != h || !TYPES.iter().any(|&(_, s)| s == w) {
            return Err(format!("icns images must be square and 16, 32, 64, 128, 256, 512 or 1024 px, got {w}x{h}"));
        }
        if sized.iter().any(|&(s, _)| s == w) {
            return Err(format!("two images of {w} px"));
        }
        sized.push((w, png));
    }
    let elements: Vec<(&[u8; 4], &[u8])> =
        TYPES.iter().filter_map(|&(code, size)| sized.iter().find(|&&(s, _)| s == size).map(|&(_, png)| (code, png))).collect();

    let mut body = Vec::new();
    // Table of contents: the type code and length of every element after it.
    body.extend_from_slice(b"TOC ");
    body.extend_from_slice(&be32(8 + 8 * elements.len(), "the table of contents")?);
    for &(code, png) in &elements {
        body.extend_from_slice(code);
        body.extend_from_slice(&be32(png.len().saturating_add(8), "an image")?);
    }
    for &(code, png) in &elements {
        body.extend_from_slice(code);
        body.extend_from_slice(&be32(png.len().saturating_add(8), "an image")?);
        body.extend_from_slice(png);
    }
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&be32(body.len().saturating_add(8), "the icon")?);
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn run(args: &[&str]) -> Result<(), String> {
    let [out, inputs @ ..] = args else {
        return Err("usage: cargo xtask icns <out.icns> <in.png>…".into());
    };
    let pngs = inputs.iter().map(|p| std::fs::read(p).map_err(|e| format!("read {p}: {e}"))).collect::<Result<Vec<_>, _>>()?;
    let icns = pack(&pngs)?;
    std::fs::write(Path::new(out), &icns).map_err(|e| format!("write {out}: {e}"))?;
    println!("{out}: {} images, {} bytes", pngs.len(), icns.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_png(size: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0, 1, 2, 3, 4]);
        v
    }

    fn be(b: &[u8]) -> usize {
        u32::from_be_bytes(b.try_into().unwrap()) as usize
    }

    /// (type code, payload) of every element after the header, checking each length.
    fn elements(icns: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(be(&icns[4..8]), icns.len());
        let mut at = 8;
        let mut out = Vec::new();
        while at < icns.len() {
            let len = be(&icns[at + 4..at + 8]);
            assert!(len >= 8 && at + len <= icns.len());
            out.push((icns[at..at + 4].try_into().unwrap(), icns[at + 8..at + len].to_vec()));
            at += len;
        }
        out
    }

    #[test]
    fn packs_every_size_with_its_retina_codes() {
        let sizes = [16, 32, 64, 128, 256, 512, 1024];
        let pngs: Vec<_> = sizes.iter().map(|&s| fake_png(s)).collect();
        let icns = pack(&pngs).unwrap();
        let els = elements(&icns);
        assert_eq!(&els[0].0, b"TOC ");
        let codes: Vec<_> = els[1..].iter().map(|(c, _)| *c).collect();
        let want: Vec<_> = TYPES.iter().map(|(c, _)| **c).collect();
        assert_eq!(codes, want);
        for (code, payload) in &els[1..] {
            let size = TYPES.iter().find(|(c, _)| **c == *code).unwrap().1;
            assert_eq!(png_size(payload).unwrap(), (size, size), "{}", String::from_utf8_lossy(code));
        }
        // The table of contents lists each element's code and full length.
        let toc = &els[0].1;
        assert_eq!(toc.len(), 8 * TYPES.len());
        for (i, (code, payload)) in els[1..].iter().enumerate() {
            assert_eq!(&toc[8 * i..8 * i + 4], code);
            assert_eq!(be(&toc[8 * i + 4..8 * i + 8]), payload.len() + 8);
        }
    }

    #[test]
    fn a_partial_set_writes_only_its_codes() {
        let icns = pack(&[fake_png(32)]).unwrap();
        let codes: Vec<_> = elements(&icns).into_iter().map(|(c, _)| c).collect();
        assert_eq!(codes, [*b"TOC ", *b"icp5", *b"ic11"]);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(pack(&[]).is_err());
        assert!(pack(&[fake_png(48)]).is_err());
        assert!(pack(&[fake_png(2048)]).is_err());
        assert!(pack(&[fake_png(64), fake_png(64)]).is_err());
        assert!(pack(&[b"GIF89a".to_vec()]).is_err());
        let mut tall = fake_png(16);
        tall[20..24].copy_from_slice(&32u32.to_be_bytes());
        assert!(pack(&[tall]).is_err());
        // Truncated PNGs fail without panicking.
        let png = fake_png(16);
        for n in 0..png.len() {
            let _ = pack(&[png[..n].to_vec()]);
        }
    }

    #[test]
    fn committed_icon_holds_every_size() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/app-icon/filmagic.icns");
        let Ok(icns) = std::fs::read(&path) else { return };
        let codes: Vec<_> = elements(&icns).into_iter().map(|(c, _)| c).collect();
        for (code, _) in TYPES {
            assert!(codes.contains(code), "{} lacks {}", path.display(), String::from_utf8_lossy(*code));
        }
    }
}
