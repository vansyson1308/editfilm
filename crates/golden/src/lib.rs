//! Golden-image tests for Filmagic's renderer (test-only crate; the library is empty).
//!
//! `tests/golden.rs` builds small procedural projects (no media files), renders one frame of each
//! through the CPU reference compositor (`filmagic-render`) and compares it with a committed PNG
//! in `goldens/` using `filmagic_testkit::golden`. The same scenes are composited on the GPU
//! (`filmagic-gpu`) when an adapter is available and compared with the CPU result.
//!
//! Regenerate references with `FILMAGIC_BLESS=1 cargo test -p filmagic-golden`.
