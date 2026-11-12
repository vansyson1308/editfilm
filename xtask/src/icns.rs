//! `cargo xtask icns <out.icns> <in.png>…`: pack PNG images into a macOS `.icns`, so the icon
//! can be built on any OS (macOS `iconutil` is not needed).
//!
//! The format, as Apple documents it for icon families: a big-endian header (`icns`, then the
//! file length) followed by elements, each a four-byte type code, a big-endian length that counts
//! its own eight-byte header, and the payload. The elements mirror what `iconutil` writes:
//!
//! - 16 and 32 px as `ic04` / `ic05`: the tag `ARGB`, then the alpha, red, green and blue planes,
//!   each compressed on its own with the icns run-length scheme (the one `is32` / `it32` channel
//!   data use): a control byte below 0x80 copies the next `c + 1` bytes, one of 0x80 or more
//!   repeats the next byte `c - 125` times.
//! - 32 to 1024 px as PNG files stored as is, in `ic07`–`ic14` (`ic10`–`ic14` are the @2x images
//!   of 512, 16, 32, 128 and 256 pt).
//!
//! Each input is matched to its type codes by its size; one PNG can fill two codes (a 32 px image
//! is both `ic05` and `ic11`, the 16 pt @2x one).

use std::path::Path;

use filmagic_testkit::golden::decode_png;

use crate::ico::png_size;

/// How an element stores its image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `ARGB` tag and run-length-compressed channel planes.
    Argb,
    /// A PNG file.
    Png,
}

/// Element types in file order: (type code, pixel size, storage).
pub const TYPES: &[(&[u8; 4], u32, Kind)] = &[
    (b"ic04", 16, Kind::Argb),
    (b"ic05", 32, Kind::Argb),
    (b"ic07", 128, Kind::Png),
    (b"ic08", 256, Kind::Png),
    (b"ic09", 512, Kind::Png),
    (b"ic10", 1024, Kind::Png),
    (b"ic11", 32, Kind::Png),
    (b"ic12", 64, Kind::Png),
    (b"ic13", 256, Kind::Png),
    (b"ic14", 512, Kind::Png),
];

fn be32(n: usize, what: &str) -> Result<[u8; 4], String> {
    u32::try_from(n).map(u32::to_be_bytes).map_err(|_| format!("{what} is too large for an icns file"))
}

/// One channel plane in the icns run-length scheme: runs of 3 to 130 equal bytes become a
/// control byte (`len + 125`) and the byte; everything else goes in literal blocks of up to 128.
pub fn rle(plane: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(plane.len() + plane.len() / 128 + 1);
    let mut literal: Vec<u8> = Vec::with_capacity(128);
    let flush = |literal: &mut Vec<u8>, out: &mut Vec<u8>| {
        for block in literal.chunks(128) {
            // A block holds 1..=128 bytes, so the control byte is 0..=127.
            out.push(u8::try_from(block.len() - 1).unwrap_or(127));
            out.extend_from_slice(block);
        }
        literal.clear();
    };
    let mut i = 0;
    while let Some(&b) = plane.get(i) {
        let run = plane.get(i..).unwrap_or(&[]).iter().take(130).take_while(|&&x| x == b).count();
        if run >= 3 {
            flush(&mut literal, &mut out);
            // 3..=130 maps to 0x80..=0xff.
            out.push(u8::try_from(run + 125).unwrap_or(0xff));
            out.push(b);
            i += run;
        } else {
            literal.push(b);
            i += 1;
        }
    }
    flush(&mut literal, &mut out);
    out
}

/// The payload of an `ic04` / `ic05` element for a `size`×`size` RGBA image.
fn argb(rgba: &[u8], size: u32) -> Result<Vec<u8>, String> {
    let n = (size as usize).saturating_mul(size as usize);
    if rgba.len() != n.saturating_mul(4) {
        return Err(format!("expected {n} RGBA pixels, got {} bytes", rgba.len()));
    }
    let mut out = b"ARGB".to_vec();
    for channel in [3, 0, 1, 2] {
        let plane: Vec<u8> = rgba.iter().skip(channel).step_by(4).copied().collect();
        out.extend_from_slice(&rle(&plane));
    }
    Ok(out)
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
        if w != h || !TYPES.iter().any(|&(_, s, _)| s == w) {
            return Err(format!("icns images must be square and 16, 32, 64, 128, 256, 512 or 1024 px, got {w}x{h}"));
        }
        if sized.iter().any(|&(s, _)| s == w) {
            return Err(format!("two images of {w} px"));
        }
        sized.push((w, png));
    }

    let mut body = Vec::new();
    for &(code, size, kind) in TYPES {
        let Some(&(_, png)) = sized.iter().find(|&&(s, _)| s == size) else { continue };
        let payload = match kind {
            Kind::Png => png.to_vec(),
            Kind::Argb => {
                let img = decode_png(png)?;
                if (img.w, img.h) != (size, size) {
                    return Err(format!("PNG header says {size} px, image is {}x{}", img.w, img.h));
                }
                argb(&img.px, size)?
            }
        };
        body.extend_from_slice(code);
        body.extend_from_slice(&be32(payload.len().saturating_add(8), "an image")?);
        body.extend_from_slice(&payload);
    }
    let mut out = Vec::with_capacity(body.len().saturating_add(8));
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
    use filmagic_testkit::golden::{Rgba8, encode_png};

    fn fake_png(size: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&size.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0, 1, 2, 3, 4]);
        v
    }

    /// A real PNG with varied pixels: flat areas (runs), gradients (literals), soft alpha.
    fn real_png(size: u32) -> (Vec<u8>, Vec<u8>) {
        let mut px = Vec::new();
        for y in 0..size {
            for x in 0..size {
                let a = if x < 2 {
                    0
                } else if y == 0 {
                    128
                } else {
                    255
                };
                px.extend_from_slice(&[(x * 7) as u8, if y < size / 2 { 200 } else { (x ^ y) as u8 }, 0x5c, a]);
            }
        }
        (encode_png(&Rgba8::new(size, size, px.clone())).unwrap(), px)
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

    /// Independent decoder for one RLE plane of `n` bytes; returns it and the bytes consumed.
    fn unrle(data: &[u8], n: usize) -> (Vec<u8>, usize) {
        let (mut out, mut at) = (Vec::new(), 0);
        while out.len() < n {
            let c = data[at] as usize;
            if c >= 0x80 {
                out.extend(std::iter::repeat_n(data[at + 1], c - 125));
                at += 2;
            } else {
                out.extend_from_slice(&data[at + 1..at + 2 + c]);
                at += c + 2;
            }
        }
        assert_eq!(out.len(), n, "a block ran past the plane");
        (out, at)
    }

    /// Decode an ARGB payload back to RGBA.
    fn unargb(payload: &[u8], size: u32) -> Vec<u8> {
        assert_eq!(&payload[..4], b"ARGB");
        let n = (size * size) as usize;
        let mut at = 4;
        let mut planes = Vec::new();
        for _ in 0..4 {
            let (p, used) = unrle(&payload[at..], n);
            planes.push(p);
            at += used;
        }
        assert_eq!(at, payload.len(), "trailing bytes after the planes");
        (0..n).flat_map(|i| [planes[1][i], planes[2][i], planes[3][i], planes[0][i]]).collect()
    }

    #[test]
    fn rle_round_trips_runs_literals_and_long_blocks() {
        let mut cases: Vec<Vec<u8>> = vec![vec![], vec![7], vec![1, 1], vec![1, 1, 1], vec![9; 130], vec![9; 131], vec![9; 400]];
        cases.push((0..=255).cycle().take(1000).collect());
        cases.push([vec![1, 2], vec![3; 5], vec![4, 5, 6], vec![0; 260], vec![8]].concat());
        for plane in cases {
            let enc = rle(&plane);
            let (dec, used) = unrle(&enc, plane.len());
            assert_eq!(dec, plane);
            assert_eq!(used, enc.len());
        }
        // Runs compress: 400 equal bytes take 4 two-byte runs (130 + 130 + 130 + 10).
        assert_eq!(rle(&[9; 400]).len(), 8);
    }

    #[test]
    fn packs_iconutils_layout_and_argb_decodes_exactly() {
        let (p16, px16) = real_png(16);
        let (p32, px32) = real_png(32);
        let mut pngs = vec![p16, p32.clone()];
        pngs.extend([64, 128, 256, 512, 1024].iter().map(|&s| fake_png(s)));
        let icns = pack(&pngs).unwrap();
        let els = elements(&icns);
        let codes: Vec<_> = els.iter().map(|(c, _)| *c).collect();
        let want: Vec<_> = TYPES.iter().map(|(c, _, _)| **c).collect();
        assert_eq!(codes, want);
        for ((code, payload), &(_, size, kind)) in els.iter().zip(TYPES) {
            match kind {
                Kind::Argb => {
                    let src = if size == 16 { &px16 } else { &px32 };
                    assert_eq!(&unargb(payload, size), src, "{}", String::from_utf8_lossy(code));
                }
                Kind::Png => assert_eq!(png_size(payload).unwrap(), (size, size), "{}", String::from_utf8_lossy(code)),
            }
        }
        // The 32 px PNG is stored as is for ic11.
        assert_eq!(els[6].1, p32);
    }

    #[test]
    fn a_partial_set_writes_only_its_codes() {
        let icns = pack(&[real_png(32).0]).unwrap();
        let codes: Vec<_> = elements(&icns).into_iter().map(|(c, _)| c).collect();
        assert_eq!(codes, [*b"ic05", *b"ic11"]);
        let icns = pack(&[fake_png(64)]).unwrap();
        let codes: Vec<_> = elements(&icns).into_iter().map(|(c, _)| c).collect();
        assert_eq!(codes, [*b"ic12"]);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(pack(&[]).is_err());
        assert!(pack(&[fake_png(48)]).is_err());
        assert!(pack(&[fake_png(2048)]).is_err());
        assert!(pack(&[fake_png(64), fake_png(64)]).is_err());
        assert!(pack(&[b"GIF89a".to_vec()]).is_err());
        // A 16 px header on a body that does not decode.
        assert!(pack(&[fake_png(16)]).is_err());
        let mut tall = fake_png(16);
        tall[20..24].copy_from_slice(&32u32.to_be_bytes());
        assert!(pack(&[tall]).is_err());
        // Truncated and bit-flipped PNGs fail without panicking.
        let png = real_png(16).0;
        for n in 0..png.len() {
            let _ = pack(&[png[..n].to_vec()]);
            let mut flipped = png.clone();
            flipped[n] ^= 0x41;
            let _ = pack(&[flipped]);
        }
    }

    #[test]
    fn committed_icon_holds_every_size() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/app-icon/filmagic.icns");
        let icns = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let els = elements(&icns);
        for &(code, size, kind) in TYPES {
            let Some((_, payload)) = els.iter().find(|(c, _)| c == code) else {
                panic!("{} lacks {}", path.display(), String::from_utf8_lossy(code));
            };
            match kind {
                Kind::Argb => assert_eq!(unargb(payload, size).len(), (size * size * 4) as usize),
                Kind::Png => assert_eq!(png_size(payload).unwrap(), (size, size)),
            }
        }
    }
}
