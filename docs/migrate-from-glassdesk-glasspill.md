# 从 glassdesk / glasspill 迁移到 GlassSuite

> 本文件写给两件事：**用户怎么把旧配置带过来**，以及**维护者为什么没有把旧仓的文件整包搬过来**。
> 旧两个仓库是只读参考源，GlassSuite 不复用它们的 git 历史，也不执行 `git mv`。

## 1. 一句话结论

旧版是**两个进程、两套状态、两套光标观测**；GlassSuite 是**一个进程、多个无边框窗口、一份共享状态**。
配置可以平滑迁移，代码是按能力重写/收敛，不是复制粘贴。

## 2. 用户：配置会自动迁移

首次启动时，GlassSuite 会在**创建任何配置文件之前**做一次一次性复制：

| 旧位置（`%APPDATA%`） | 新位置（`%APPDATA%`） | 内容 |
|---|---|---|
| `dev.glassdesk.app\dock.json` | `com.glasssuite.app\dock.json` | 固定图标、顺序、游离图标坐标、自动隐藏开关 |
| `dev.glassdesk.app\menu.json` | `com.glasssuite.app\menu.json` | 开始菜单收藏 |
| `dev.glasspill.app\settings.json` | `com.glasssuite.app\settings.json` | 需要**手工**核对，见下表 |

规则：

- **只复制、不删除、不覆盖**：目标文件已存在就跳过，旧文件永远保留。日志各写一行
  `dock: migrated dock.json count=1` / `dock: migrated menu.json count=1`（只记文件名与数量，
  不记路径里的用户名）。
- 旧 `dock.json` 的 snake_case 字段（`dock_top_layer` / `free_rect`）用 serde alias 兼容，
  **图标顺序与收藏逐项保持**。
- 旧 `dock.json` 把裸 RGBA 图标内联进配置（本机这份 93,080 字节）。迁移后这些内联图标
  会被丢弃，改由 `SHGetFileInfoW` 在**运行时**重新提取，并把配置瘦回新格式（通常是几 KB）。
  图标外观可能变化，但顺序、名称、路径、游离位置不变。
- 旧 `settings.json` **不自动复制**。原因是旧版里的天气坐标、网络探测地址、AI 端点、
  歌词源地址都是本机私有值，直接带过来会破坏「首启默认不联网」这条红线。
  需要的功能请在新 `settings.json` 里显式重填。

`settings.json` 字段对照（新首启模板所有外联字段为 `null`）：

| 旧字段 | 新字段 | 默认 |
|---|---|---|
| 天气坐标 | `weather: { latitude, longitude }` | `null` |
| 网络探测目标 | `networkProbeUrl` | `null` |
| AI 网关 | `aiBaseUrl` | `null`（**仓内没有任何内置/回落端点**） |
| AI 密钥 | `aiApiKey` 或环境变量 `GLASSSUITE_AI_KEY` | `null` |
| 歌词源 | `lyricsSourceUrl` | `null` |
| 通知读取 | `notifications.enabled` | `false` |
| 自动隐藏 | `dock.autoHide`（同时也在 `dock.json`） | `false` |

## 3. 维护者：能力映射

| 旧文件 | 新落点 | 处理 |
|---|---|---|
| `glasspill/.../media/smtc.rs` | `src-tauri/src/island/media.rs` | 事件驱动保留；歌词耦合拆到 `island/lyrics.rs` |
| `glasspill/.../notification/*` | `src-tauri/src/island/notification/*` | WAL 快照只读复制；正文不落盘/不进日志；默认关闭 |
| `glasspill/.../lyrics.rs`, `ai/*` | `src-tauri/src/island/lyrics.rs`, `ai/*` | 源/端点默认 null；私有网关不作为黑名单写进源码 |
| `glasspill/.../window/clickthrough.rs` | `src-tauri/src/window/cursor.rs` | **唯一** `WH_MOUSE_LL` 钩子，同时驱动岛与 dock |
| `glassdesk/.../dock.rs` | `src-tauri/src/dock/mod.rs` | 收敛为指纹比对 + 按需扫描；删掉 200ms `GetCursorPos` 轮询 |
| `glassdesk/.../running.rs` | `src-tauri/src/dock/running.rs` | 2s 硬截止；超时返回「无结果」而不是「空集合」 |
| `glassdesk/.../menu.rs` | `src-tauri/src/dock/menu.rs`, `StartMenu.tsx` | 本地目录枚举，2s 硬截止 |
| `glassdesk/.../preview.rs` | `src-tauri/src/dock/preview.rs`, `PreviewSurface.tsx` | DWM 注册 400ms 硬截止，降级纯色块 |
| `glassdesk/.../free.rs` | `src-tauri/src/dock/free.rs`, `FreeIcon.tsx` | 每个游离图标一个恰好等大的窗口，不引入全屏透明层 |
| 两仓各自的 `SharedState` | `src-tauri/src/state.rs` | 合成一份 |
| 两仓各自的几何/任务栏代码 | `crates/winplat/*` | **几何唯一真相源**，前端永远不算摆位 |

### 带进来的 v0.8.1 五项防跳修复

1. 后端 watcher 与前端 `useDockSnapshot` 各做一次内容指纹比对，指纹不变不 emit / 不 `setState`；
2. `useFlip` 测量前先剥掉 inline `transform`，读自然布局位；
3. 全员同幅平移判为窗口几何变化 → 跳过动画，dx 记 0；
4. `fit_dock` 缩放前按「新窗口中心 = 屏幕中心」同步 `set_position`；
5. CSS 药丸 `justify-content: flex-start` 左锚定。

## 4. 明确**不**迁移的东西（清理清单）

以下都在旧仓，本仓从未引入，因此不需要删除；此处留档以防后续有人"好心"再搬回来：

| 旧仓路径（只读参考） | 字节 | 不迁移原因 |
|---|---|---|
| `glasspill/src/assets/fonts/LXGWWenKai-Regular.ttf` | 19,310,488 | 无 OFL 授权文件、体积过大 |
| `glasspill/src/assets/fonts/LXGWWenKai-Medium.ttf` | 18,757,532 | 同上；且只用得到单一字重 |
| `glassdesk/src-tauri/src/taskbar.rs` | 5,464 | 已被 `winplat::taskbar` 取代 |
| `glasspill/src-tauri/src/window/mouse_track.rs` | 3,228 | 单一 LL 钩子已覆盖 |
| `glasspill/src/components/island/Eyes.tsx` / `Eyes.css` | — | 装饰性彩蛋，无功能价值 |
| `glasspill/src/main.rs` | 0 | 空文件 |
| `glassdesk/src-tauri/src/bin/{overlay,preview,taskbar}_poc.rs` | 11,120 / 29,499 / 37,284 | 一次性探针 |
| `glasspill/src-tauri/src/bin/dwm_poc.rs` | 16,349 | 同上 |

**体积收益**：只算两枚字体就是 `19,310,488 + 18,757,532 = 38,068,020` 字节（约 36.3 MiB），
旧仓 `dist/` 里还各有一份同样大小的副本。GlassSuite 跟踪文件里字体数量为 **0**：文字全部使用
系统已安装的字体族（CSS 里是 `Segoe UI Variable` / `Microsoft YaHei UI` / `system-ui`），
既不分发字体，也不需要为字体申请授权。

`licenses/OFL-1.1.txt` 存放的是 SIL OFL 1.1 全文，用于**将来**若确实要引入子集字体时随附；
当前仓库没有任何采用该许可证的字体，因此它目前不约束任何文件，只是把政策固定在仓库里。

## 5. 隐私与网络口径变化

- 天气、网络探测、歌词源、AI 端点、通知读取：**首启全部关闭**，外联字段为 `null`。
- 通知正文、进程名/路径：**不进日志**；日志只允许「事件类型 + 数量」。
- 图标：运行时由 Windows Shell API 从用户本机取，不随发行包分发任何第三方图标。
- 旧仓的私有 AI 网关既不是默认值，也没有作为域名黑名单写进源码——把私有基础设施
  名称写进公开仓库本身就是卫生问题。

## 6. 验证方式

```powershell
# 用一份隔离的 APPDATA 模拟全新安装，不影响本机真实配置
$tmp = Join-Path $env:TEMP 'gs-migrate-check'
New-Item -ItemType Directory -Force "$tmp\dev.glassdesk.app" | Out-Null
Copy-Item "$env:APPDATA\dev.glassdesk.app\dock.json"  "$tmp\dev.glassdesk.app\"
Copy-Item "$env:APPDATA\dev.glassdesk.app\menu.json"  "$tmp\dev.glassdesk.app\"
$env:APPDATA = $tmp
.\target\debug\GlassSuite.exe
# 看 "$tmp\com.glasssuite.app\glasssuite.log" 里的 migrated 行与顺序
```
