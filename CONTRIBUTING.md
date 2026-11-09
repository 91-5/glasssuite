# Contributing

Thanks for looking. This is a Windows-only desktop app; the useful contributions are usually
platform-behaviour fixes (DPI, taskbar geometry, multi-monitor, full-screen yield) and UI polish.

## Dev setup

```powershell
pnpm install
pnpm -r build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Both must be green before a PR is reviewable. CI runs the same five commands on `windows-latest`.

## Hard rules

These exist because breaking them has already cost real debugging time:

1. **One geometry source.** Taskbar and screen geometry comes from `crates/winplat`. Do not compute
   it in a window module — mixed sources were what produced 2–46 px drift before.
2. **One DPI declaration.** `SetProcessDpiAwarenessContext` is called exactly once, in `winplat`,
   before any window exists. Never add a second call.
3. **Physical vs logical pixels are explicit.** Win32 rects are physical. CSS sizes are logical.
   Convert at the boundary with the window's `scale_factor`; never mix them in one expression.
4. **No process injection.** `WriteProcessMemory`, `CreateRemoteThread`, `VirtualAllocEx`,
   `SetWindowsHookEx`-style hooks beyond the documented low-level mouse hook are out of scope. The
   single low-level mouse hook used for click-through must stay local, must not record input, and
   must be documented in the PR.
5. **No redistributed icons or fonts.** Icons come from shell APIs at runtime via
   `SHGetFileInfo` / `IShellItemImageFactory`. Any bundled font needs its license text next to it.
6. **No telemetry.** Nothing leaves the machine unless the user turns on a feature that needs the
   network, and that feature must say what it sends.
7. **IPC names are a contract.** `packages/ipc-types` is the single source of truth; a rename is a
   breaking change and must touch the Rust side in the same commit.
8. **Every hot-path call has a timeout path.** Icon extraction and DWM thumbnails can hang on some
   GPU drivers; degrade to a generic icon instead of blocking the UI thread.

## Commit / PR style

- Commit subject: `area: what changed` (imperative, ≤ 72 chars). Body explains **why**, and — for
  platform fixes — what was measured before/after.
- One logical change per PR. Refactors that touch behaviour and structure at once get split.
- Bug fixes are welcome to include a log excerpt, but strip user names, paths and window titles.

## Reporting a bug

Please include: Windows build (`winver`), display scaling per monitor, number of monitors, whether
the taskbar is set to auto-hide, and the relevant lines from the app log (found under
`%APPDATA%\com.glasssuite.app\`). Do not paste full process lists or notification contents.
