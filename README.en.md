# paintEngine | [中文](README.md)

A cross-platform painting engine core, written in pure Rust. At its heart is an **infinite tiled canvas** — sparse tile storage, copy-on-write undo history, pressure-sensitive stroke rasterization and dirty-region compositing — with the same engine core embedded into desktop, browser, and native Android apps.

## Features

- **Infinite canvas**: 256px sparse tiles, negative coordinates, unbounded pan & zoom; a dot grid conveys the space, and "fit to content" brings you home
- **Pressure-sensitive strokes**: round brush (size/hardness/opacity/flow/spacing), EMA smoothing, pressure→width mapping; eraser (dst-out)
- **Layer system**: add/remove/duplicate/reorder/merge-down/flatten, **12 blend modes** (W3C formulas), per-layer opacity — all undoable
- **Undo**: tile COW snapshots + structural ops (`UndoOp` enum), one group per stroke, history evicted by memory budget
- **Dual rendering backends**: hand-written software renderer (premultiplied alpha, bilinear sampling) + wgpu GPU compositing (feature-gated; CPU stamping + GPU compositing hybrid)
- **PNG import/export**: optional region/scale/transparent background; exports never include the grid
- **Multi-touch gestures**: two-finger pan & zoom, instant rollback of accidental strokes, gesture latch, hover-based palm rejection
- **78 automated tests** plus verified on real hardware, emulator, and browser

## Architecture

```
crates/
├── paint-core/      Engine core (platform-free): tiles/layers/undo/viewport/strokes/PNG/engine skeleton
├── paint-render/    Software Renderer impl: dab stamping, dirty-region compositing, layer merging
├── paint-gpu/       wgpu Renderer impl: GPU layer compositing (feature "gpu")
├── paint-desktop/   Desktop shell: winit + softbuffer (Windows/macOS/Linux)
├── paint-wasm/      Web shell: canvas presentation + Pointer Events (incl. pointerrawupdate)
└── paint-android/   Android shell: JNI + Kotlin PaintEngineView (with demo APK project)
```

Core principle: **the engine only "computes pixels" (tiles); platform shells only "present pixels and feed events."** `paint-core` knows nothing about screens; the `Renderer` / `Surface` traits isolate rendering backends from presentation targets.

## Platform Status

| Platform | Status | Shell tech | Verification |
|---|---|---|---|
| Desktop (Win/macOS/Linux) | ✅ | winit + softbuffer (optional `--features gpu` for wgpu) | Manually tested |
| Web | ✅ | wasm-bindgen + canvas, rAF/interval dual-driven | Automated pixel-level browser tests |
| Android | ✅ | JNI cdylib + Kotlin View | Real device (Lenovo) + emulator |
| iOS | Deferred | — | — |

## Quick Start

```bash
# Desktop (mouse drawing · space/middle-drag pan · wheel zoom · Ctrl+0 fit · G grid · B/E tools)
cargo run -p paint-desktop --release
cargo run -p paint-desktop --release --features gpu   # GPU compositing backend

# Web (open http://localhost:8000 in a browser)
cargo install wasm-pack   # one-time
cd crates/paint-wasm
wasm-pack build . --target web --out-dir www/pkg --release
cd www && python3 -m http.server 8000

# Android (APK ≈ 2.5MB with dual-arch engine; requires NDK cross-compiled .so — see that crate's README)
cd crates/paint-android/android && ./gradlew assembleDebug

# Full test suite (78 tests)
cargo test
```

## Documentation

- **[plan.md](plan.md)** (Chinese) — full development plan: architecture decisions, API blueprint, dependency choices (incl. rejected ones), M1–M4 milestones with per-item acceptance records
- Per-shell crate READMEs — platform-specific build and run details
