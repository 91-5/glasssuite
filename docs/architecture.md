# Architecture

> Living document. Sections marked **(planned)** are contracts agreed up front so that later
> milestones don't drift; they have no implementation yet.

## 1. Process and window model

One process, two (later three) frameless windows:

```
GlassSuite.exe
├── island     top, centered, always-on-top, no-activate
├── dock       bottom, above taskbar, no-activate, auto-hide
└── startmenu  transient panel opened from dock (later milestone)
```

Why one process:

- One WebView2 runtime and one GPU pipeline for both surfaces; the second window costs far less than
  a second process.
- A single `RunEvent` loop decides full-screen yielding, taskbar changes and z-order for all windows
  at once — the alternative is IPC-driven state that inevitably desyncs.
- Single-instance is one named mutex.

Cost we accept: no crash isolation between dock and island. Mitigation: anything that can hang on a
driver (icon extraction, DWM thumbnails) must be bounded by a timeout and degrade.

## 2. Crate / package boundaries

```
crates/winplat/        Windows platform layer — the only place that talks to shell geometry
  geom.rs              pure math (physical pixels), unit-tested on any OS
crates/shell/ (planned) Tauri shell: window creation, RunEvent loop, IPC commands

apps/dock, apps/island  frontends, built independently into dist/<app>/
packages/ipc-types      command/event names + payload types (shared contract)
packages/shared-ui      glass surface primitives + CSS tokens
```

Rule: a frontend never computes window placement. It reports its content size (`fit_dock`,
`fit_island`) and `winplat` decides position and size atomically.

## 3. Geometry (implemented: `winplat::geom`)

All geometry is **physical pixels**, matching Win32 `RECT` (right/bottom exclusive).

- `Rect::centered_x(width)` — center a window within a given rect; relative to that rect's origin,
  so a secondary monitor with a non-zero origin is handled.
- `Rect::bottom_anchored_y(height, margin)` — sit above the taskbar with a margin.
- `Rect::is_plausible_taskbar(screen_height)` — reject phantom taskbar rects.

Phantom taskbar rects are real: after a full-screen app reveals an auto-hidden taskbar and the app
exits, `GetWindowRect` can return the *visible* position (e.g. `top=1032`) while the taskbar is
actually hidden and docked at `(0, 1078, 1920, 1126)`. Trusting it moves windows up by ~46 px.

Resize and reposition must happen **in one synchronous sequence**: resize keeps the top-left corner
fixed, so resizing before recentering produces a visible two-step shift of existing content.

## 4. DPI (planned)

`SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` exactly once, in `winplat`, before any window
is created. Every window subscribes to DPI-change and display-change events from the same source.

## 5. Cursor and click-through (planned)

Single source of truth: one low-level mouse hook (`WH_MOUSE_LL`) drives both the island's
click-through hot zones and the dock's reveal trigger. The dock's legacy 200 ms `GetCursorPos` poll is
retired — two observers in one process produced two different latencies and two different meanings of
"revealed".

Hit-testing is custom: the windows are `WS_EX_NOACTIVATE` and must never steal focus from the
foreground app.

## 6. Full-screen yielding (planned)

One detection routine, quality-number-system based, with the shell-window exclusion list carried over
from the dock. Reason: a purely geometric check misses exclusive-fullscreen apps, and two different
answers in one process means the island hides while the dock shows.

## 7. Logging and config (planned)

One log file per user, one truncation policy, timestamps in local time plus a monotonic counter.
Config lives under `%APPDATA%\com.glasssuite.app\`. On first run, config from the two legacy apps is
migrated once (`dock.json`, `menu.json`), and the migration is logged.

## 8. Single instance (planned)

One named mutex (`Local\GlassSuite.SingleInstance`). A second launch exits 0 and logs
`already running`; it does not steal focus.
