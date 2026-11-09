# GlassSuite

A glass-styled desktop suite for Windows: a **top island** (media, notifications, system stats) and a
**bottom dock** (launching apps, running-app indicators, window previews) — running as **one process,
two frameless windows**.

> **Status: pre-release / 开发中.** Public versioning starts at `0.1.0`. Nothing here is a stable
> API yet; expect breaking changes between `0.1.x` releases.

## Why another dock?

Most "dock" projects are either a theme for the Windows taskbar or a single-window overlay. GlassSuite
is built around two constraints that come from actually using one every day:

- **One process, two windows.** Both surfaces share one WebView2 runtime, one GPU pipeline and one
  event loop, so full-screen yielding, taskbar geometry and always-on-top ordering are decided once.
- **A single platform layer.** Taskbar geometry, DPI awareness, full-screen detection, window styles,
  single-instance and logging live in `crates/winplat` — the windows don't each reinvent them.

## Layout

```
apps/dock        bottom dock frontend (React + Vite)
apps/island      top island frontend (React + Vite)
packages/ipc-types   IPC contract shared by frontends and Rust
packages/shared-ui   glass surface primitives shared by both windows
crates/winplat   Windows platform layer (geometry, DPI, fullscreen, styles, logging, config)
```

## Build

Requirements: Windows 10/11, Node 20+, pnpm 11+, Rust stable (MSVC toolchain), WebView2 runtime.

```powershell
pnpm install
pnpm -r build          # builds dist/dock and dist/island
cargo test --workspace
```

The Tauri shell crate is added in a later milestone; until then `pnpm -r build` and `cargo test` are
the two gates.

## Privacy

GlassSuite is **local-only**:

- It reads the list of running applications to show indicators, and (for the notification feature)
  reads local notification data. **Nothing is uploaded anywhere.** No telemetry, no analytics, no
  crash reporting endpoint.
- Network access only happens for features you enable yourself (weather with a location you set,
  optional AI summarization with **your own** API endpoint and key).
- Process lists and notification contents are never written to logs.

## Icons and trademarks

Application icons are fetched at **runtime** through Windows shell APIs (`SHGetFileInfo` /
`IShellItemImageFactory`). No third-party icon resources are redistributed with this project.

## License

GPL-3.0. See [LICENSE](LICENSE) and [NOTICE.md](NOTICE.md) for third-party components.

## Install (once 0.1.0 ships)

Releases will be **unsigned**; Windows SmartScreen will warn on first launch. Verify the published
`SHA256SUMS` before running. A portable `.zip` will be published alongside the NSIS installer.
