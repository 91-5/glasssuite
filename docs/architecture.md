# Architecture

> Living document. Status markers: **(implemented)** = 有代码与单测；**(contract)** = 已定但尚未落地。
> 最后一次结构性更新：P4（dock 迁移）完成后。

## 1. Process and window model (implemented)

**One process, N frameless windows.** 基线是两窗，P4 之后按需创建辅助窗：

```
GlassSuite.exe                    一个 WebView2 runtime / 一个 tao 事件循环 / 一个 GPU 管线
├── island      常驻  top, centered, always-on-top, no-activate, 点击穿透
├── dock        常驻  bottom, above taskbar, no-activate, auto-hide（可配）
├── startmenu   按需  dock 上的开始按钮唤起，首屏隐藏
├── preview     按需  DWM 缩略图承载层，尺寸 = 预览尺寸，首屏隐藏
└── free-<id>*  按需  每枚游离图标一个恰好等大的小窗（0..n 个）
```

- `island` / `dock` 共享 `Arc<SharedState>`（媒体通道、两窗热区、穿透标志、指标快照）。
- 辅助窗**不是**「固定尺寸大窗 + 常驻热区」：它们的窗口尺寸就是各自内容尺寸。
  唯一允许固定尺寸的是 island 展开面板与预览层，两者都走 `place()` 的 resize 路径。
- 所有窗口统一：无边框、透明、`skip_taskbar`、`WS_EX_TOOLWINDOW` + `WS_EX_NOACTIVATE`、
  不抢焦点、不进 Alt-Tab。`show()` 之后要补一次 ExStyle（tao 会重写）。
- 全屏让位由**一个**检测源决定**所有**窗显隐，并记住让位前哪些窗可见，
  退出全屏时不会把本来关着的开始菜单顺手弹出来。

Why one process:

- 两个窗口共用一套 WebView2 运行时与 GPU 管线；第二个窗口远比第二个进程便宜。
- 一个事件循环决定全屏让位、任务栏几何和 z-order —— 用 IPC 驱动必然出现状态漂移。
- 单实例就是一个命名互斥量。

Cost we accept: no crash isolation between dock and island. Mitigation: anything that can hang on a
driver (icon extraction, DWM thumbnails, WAL reads, process enumeration, ShellExecute) runs on a
**short-lived thread with a hard deadline** and degrades to a placeholder. 单进程下「dock 卡 = 岛也卡」，
这条不是可选项。

## 2. Crate / package boundaries (implemented)

```
crates/winplat/       Windows 平台层：几何 / DPI / 全屏 / ExStyle / 单实例 / 日志 / 配置 / GPU / 电源
  geom.rs             纯数学（物理像素），跨平台可单测
  taskbar.rs          任务栏矩形 + 隐藏态 phantom rect 判定
  screen.rs / dpi.rs / fullscreen.rs / style.rs / instance.rs / log.rs / config.rs
src-tauri/            Tauri 应用层（原计划的 crates/shell 直接落在这里，没有再拆一层）
  shell.rs            建窗、摆位、内容自适应尺寸、辅助窗
  state.rs            进程级共享状态（唯一一份）
  window/             cursor / panel（全屏让位）/ set_hotzone
  island/             metrics / weather / network / media / notification / lyrics / ai
  dock/               config / running / icons / preview / menu / free / mod
apps/dock, apps/island 前端，各自构建到 dist/<app>/
packages/ipc-types     命令名 / 事件名 / payload 类型（前后端唯一契约）
packages/shared-ui     GlassSurface + useFitWindow + CSS tokens
```

**Rule: a frontend never computes window placement.** 前端只上报内容尺寸（`fit_dock` / `fit_island`），
`winplat` 算位置，`shell::place` 用「先 `set_position` 后 `set_size`」的同步序列一次性应用。
前端也不直接 `listen()`，统一用 `ipc-types::subscribe`（处理 unlisten 竞态）。

### `winplat` 边界（不得破坏）

| 只在这里算 | 消费方 |
|---|---|
| 显示器/任务栏矩形、屏幕中心、多屏原点偏移 | `shell::compute_geometry`, `dock::running` 的 shell 类名表 |
| DPI 感知声明与逻辑↔物理换算依据 | `shell::place`, `place_at`, `window::cursor` |
| 前台是否全屏（QNS + shell 类排除） | `window::panel::spawn_fullscreen_watch` |
| ExStyle 位操作（tool window / no-activate） | `shell::build_window`, `ensure_window`, `dock::free` |
| 单实例互斥量、日志格式与脱敏、配置原子写与迁移 | `lib.rs` setup, 各功能域 |

`src-tauri` 与两个 app **不得**自行调用 `GetWindowRect`/`GetSystemMetrics` 去推摆位；
`window::cursor` 是唯一例外——它只取窗口实时原点做命中判定，不决定摆位。

## 3. Geometry (implemented: `winplat::geom`)

All geometry is **physical pixels**, matching Win32 `RECT` (right/bottom exclusive).

- `Rect::centered_x(width)` — center a window within a given rect; relative to that rect's origin,
  so a secondary monitor with a non-zero origin is handled.
- `Rect::above_anchored_y(height, margin)` — put a window sitting **above** a rect's top edge. This is
  the dock anchor: the dock sits above the taskbar's *top* edge, never above the hidden taskbar rect's
  bottom edge (which is off-screen by design).
- `Rect::is_plausible_taskbar` / `looks_like_hidden_dock` — reject phantom taskbar rects.

Phantom taskbar rects are real: after a full-screen app reveals an auto-hidden taskbar and the app
exits, `GetWindowRect` can return the *visible* position (e.g. `top=1032`) while the taskbar is
actually hidden and docked at `(0, 1078, 1920, 1126)`. Trusting it moves windows up by ~46 px. The
discriminator: a genuinely hidden taskbar has *slid off-screen*, so its `bottom` exceeds the screen's
bottom; a rect ending exactly at the screen bottom while reporting "hidden" is a phantom.

Resize and reposition must happen **in one synchronous sequence**: resize keeps the top-left corner
fixed, so resizing before recentering produces a visible two-step shift of existing content.
`fit_dock` 额外在缩放前按「新窗口中心 = 屏幕中心」做一次同步 `set_position`（v0.8.1 防跳修复 #4）。

Live check: `cargo run -p winplat --example probe` prints screen, taskbar and foreground-fullscreen
values from the real desktop.

## 4. DPI (implemented: `winplat::dpi`)

`SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` exactly once, **before any window is created**
(`lib.rs::run` 第一步)。每个窗口订阅 `ScaleFactorChanged`，按新缩放重摆，避免跨不同缩放的显示器
时窗口留在旧物理坐标上。逻辑→物理换算集中在 `shell::place` / `place_at`。

## 5. Cursor and click-through (implemented: `window::cursor`)

**单一观测源**：一个 `WH_MOUSE_LL` 低级鼠标钩子，装在主线程 setup 内（独立线程消息泵实测会静默停摆）。
它**只读取光标位置**，同时驱动：

- island 点击穿透（动态热区 + 80ms 进入防抖 / 250ms 离开滞回）；
- dock 贴底唤出（4px 触发带 / 800ms 收起延迟）。

钩子回调只做内存计算 + `try_lock`，样式切换派发回主线程；不记录按键/标题/剪贴板/通知内容，
不注入任何进程。安装失败时降级为「两窗都可交互」，不留收不到输入的幽灵窗。
旧 dock 的 200ms `GetCursorPos` 轮询**已删除** —— 一个进程里两套观测必然给出两套「已唤出」语义。

热区是 CSS 逻辑像素、随内容尺寸动态上报（`useFitWindow` → `set_hotzone`），窗口原点由
`GetWindowRect` 实时取，因此窗口移动/DPI 变化后热区自动跟随。

## 6. Thread model (implemented)

| 线程 | 生命周期 | 跑什么 | 硬截止 / 降级 |
|---|---|---|---|
| Tauri 主线程 | 进程 | 建窗、resize、DWM 属性、事件分发、命令 | 不做可能卡住的系统调用 |
| `glasssuite-smtc` | 常驻 | `CoInitializeEx(MTA)` + SMTC 事件订阅；回调只投 `MediaMsg::Refresh` | WinRT 异步 `Status()` 轮询 5ms |
| `glasssuite-metrics` / `weather` / `network` | 常驻 | 1s 指标、15min 天气、30s 探测 | 未配置则直接跳过（零外联） |
| `glasssuite-ai` | 常驻（仅在配置齐时创建） | 本地规则 + 可选 LLM | LLM 短命线程 + `recv_timeout` |
| 通知扫描 | 短命 | `wpndatabase.db` + `-wal` 只读快照复制并查询 | **5s**；超时保留上一帧 |
| `glasssuite-fullscreen` | 常驻 | 1s 轮询前台全屏状态 | — |
| `glasssuite-dock-watch` | 常驻 | 1.5s 枚举可见窗口；**内容指纹**不变不 emit | 交给下一行 |
| `glasssuite-running-scan` | 短命 | `EnumWindows` 只收集 (pid, hwnd)，随后查进程路径 | **2s**；`None` ≠ 空集合 |
| `glasssuite-icon` | 短命 / 每 key 一次 | `SHGetFileInfoW` → 32bpp BMP data URL | **800ms**；回落自绘 SVG |
| `glasssuite-dwm-preview` | 短命 | `DwmRegisterThumbnail` + `UpdateThumbnailProperties` | **400ms**；回落纯色块 |
| `glasssuite-menu-scan` | 短命 | 开始菜单目录遍历 | **2s**；回落空列表 |
| `glasssuite-launch` | 短命 | `ShellExecuteW` | **1s**；记一行降级日志 |

超时线程不会被强杀（强杀会泄漏 COM/GDI 状态）；外层丢弃 `Receiver` 后结果自然无人消费，等效降级。

## 7. Full-screen yielding (implemented: `window::panel`)

One detection routine (`winplat::fullscreen`, QNS + shell-window exclusion list) drives **all** windows.
`webview_windows()` 遍历而非硬编码两窗，并用一个 `HashSet` 记住让位前可见的窗口集合。
纯几何判断会漏掉独占全屏应用；两个不同的答案会让岛藏了 dock 没藏。

## 8. Single instance (implemented: `winplat::instance`)

One named mutex (`Local\GlassSuite.SingleInstance`). A second launch exits 0 and logs
`already running`; it does not steal focus.

Implementation note that cost real time: the returned guard must be **bound in `main`'s scope**. Writing
`match acquire() { Ok(Some(_g)) => ... }` drops the guard at the end of the match arm, releasing the
mutex immediately — a second instance then starts happily. The probe binary hit exactly this.

## 9. Logging, config and privacy (implemented: `winplat::{log,config}`)

- Log: one file per user under `%APPDATA%\com.glasssuite.app\glasssuite.log`, 64 KB 原地截断保留尾部。
  **日志只允许「事件类型 + 数量」**：不写用户目录全路径、窗口标题、进程名/路径、通知正文、URL、密钥。
- Config: plain JSON, atomic write (temp + rename), corrupt file → `None` → defaults, never a startup
  failure. Legacy `dev.glassdesk.app` `dock.json` / `menu.json` are copied **once**, never overwritten,
  and the copy happens **before** any default config is written (otherwise a seed file occupies the
  destination and migration silently skips).
- All network/privacy features default off: `weather`, `networkProbeUrl`, `aiBaseUrl`, `aiApiKey`,
  `lyricsSourceUrl` are `null`; `notifications.enabled` is `false`. See
  `docs/migrate-from-glassdesk-glasspill.md` for the full mapping.

## 10. Frontend layout invariants

- `useFitWindow` measures `offsetWidth/offsetHeight` (not `getBoundingClientRect`, which includes CSS
  `transform` and would feed hover scaling back into window size — a self-amplifying loop).
- Resize + hot-zone report happen in the same call, so no stale transparent region can eat clicks.
- The dock pill is **left-anchored** (`justify-content: flex-start`). Content fingerprint comparison
  and FLIP invariants are documented in `apps/dock/src/useFlip.ts`.

## 11. Resource comparison and multi-monitor × DPI

实测数据与冒烟表格在 P6（打包发布）阶段补入，见交付单 `OC-20261109-002.delivery.md` 的 B7 / B12
条目。此处不预填未实测的数字。
