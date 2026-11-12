# filmagic-web

Filmagic in the browser (wasm32): the engine and egui UI on eframe's web runner, with browser
implementations of the host services (Blob media reads, downloads, OPFS auto-save and recovery,
WebAudio output, WebCodecs decoding) and the `window.filmagic` agent API.

Build with `cargo xtask web [--serve PORT]`. Toolchain, architecture and the API: [docs/web.md](../../docs/web.md).

`web/` holds the static files copied next to the generated `filmagic_web.js` / `.wasm`;
`tests/smoke.mjs` is a dependency-free headless-Chrome smoke test.
