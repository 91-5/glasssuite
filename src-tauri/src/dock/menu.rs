//! 玻璃开始菜单（OC-20261030-003 任务 C）。
//!
//! 面板 = 独立 Tauri 窗口（`decorations:false + transparent:true`，tauri.conf.json
//! 预定义 `startmenu`），`desktop::enable_glass` 复用 dock 药丸/预览窗同款玻璃
//! （卡面钉死，禁止新合成路径）。
//!
//! 应用枚举（卡面钉死）：`SHGetKnownFolderItem(FOLDERID_AppsFolder)` →
//! `BindToHandler(BHID_EnumItems)` → `IEnumShellItems`——系统开始菜单同源数据，
//! Win32 + UWP + Store 一次到位。**不扫文件夹、不拼注册表**。
//! 每条 `IShellItem` 取 `SIGDN_NORMALDISPLAY`（显示名）+ `SIGDN_DESKTOPABSOLUTEPARSING`
//! （可执行解析名：UWP 为 `shell:AppsFolder\<AUMID>` 形态、Win32 为开始菜单 .lnk，
//! 两者 `ShellExecuteW` 均可直接启动——Stage B 已分别实测）。单条解析失败跳过
//! （交接单坑：.lnk 条目上的部分解析会返回 null，逐条容错不致命）。
//!
//! 图标提取（卡面：主要工作量）：`IShellItemImageFactory::GetImage(SIIGBF_ICONONLY)`
//! 48px，后台线程逐条提取（COM 单次初始化），**两级缓存**：
//!   - 磁盘：`%LOCALAPPDATA%\com.glasssuite.app\icon_cache\<hash(key)>.bin`
//!     （缓存键 = 解析名 + 文件 mtime；UWP 条目无稳定文件，mtime 分量取 ""——
//!     包图标变更需清缓存，取舍记交付单）
//!   - 内存：HashMap，进程内一次提取终身命中
//!
//! 面板展开不等图标：立即显示 + 占位块，提取完成后前端 400ms 轮询自然补齐。
//!
//! 交互：dock 左端 ⊞ 点击 toggle；失焦延迟收回（150ms 后复查前台——新前台是
//! dock 本体则不收，交给 ⊞ 的 toggle 语义，避免「点 ⊞ 关闭反而重开」竞态）；
//! Esc / 点击面板外收回。启动复用 `launch_shortcut`（`dock::launch` = ShellExecuteW，
//! UWP 解析名实测可用，Stage B 计算器验证）。

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::ffi::c_void;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::Manager;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{HWND, PROPERTYKEY, RECT};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, IShellItem2,
    SHGetKnownFolderItem, SHGetKnownFolderPath, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
    SIGDN_PARENTRELATIVEPARSING, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};

use crate::dock::IconData;

/// 面板逻辑尺寸（shell.rs 建窗时引用同一对常量，保持唯一真相源）。
pub const PANEL_W: f64 = 360.0;
pub const PANEL_H: f64 = 480.0;
const PANEL_GAP: i32 = 6;
const BLUR_DELAY_MS: u64 = 150;
/// 失焦复查的代数：每次 show/hide 自增，延迟任务过期即弃。
static BLUR_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// 上次面板锚点（dock 没动 → 展开 时跳过 SetWindowPos）。
static LAST_ANCHOR: LazyLock<std::sync::Mutex<Option<(i32, i32)>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

static MENU: RwLock<Option<Vec<MenuApp>>> = RwLock::new(None);
static ICONS: RwLock<Option<HashMap<String, IconData>>> = RwLock::new(None);
static ENUM_DONE: AtomicBool = AtomicBool::new(false);
static EXTRACT_DONE: AtomicBool = AtomicBool::new(false);
static EXTRACT_STARTED: AtomicBool = AtomicBool::new(false);
static LOG_PATH: LazyLock<std::sync::Mutex<Option<PathBuf>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

#[derive(Serialize, Deserialize, Clone)]
pub struct MenuApp {
    pub id: String,
    pub name: String,
    /// 可执行解析名（UWP：`shell:AppsFolder\<AUMID>`；Win32：开始菜单 .lnk 路径）
    pub path: String,
    /// None = 图标未就绪（前端画占位块）
    pub icon: Option<IconData>,
}

/// 初始化（lib.rs setup 调用）：日志路径；枚举与提取在首次展开时懒启动。
pub fn init(log_dir: PathBuf) {
    if let Ok(mut g) = LOG_PATH.lock() {
        *g = Some(log_dir.join("menu.log"));
    }
}

/// 调试注记（拖拽定位临时通道：前端关键事件 → menu.log）。
pub fn debug_note(msg: &str) {
    log(&format!("drag: {msg}"));
}

fn log(line: &str) {
    let Ok(g) = LOG_PATH.lock() else { return };
    let Some(p) = g.as_ref() else { return };
    if let Ok(meta) = std::fs::metadata(p) {
        if meta.len() > 64 * 1024 {
            if let Ok(all) = std::fs::read(p) {
                let tail = &all[all.len().saturating_sub(8 * 1024)..];
                let _ = std::fs::write(p, tail);
            }
        }
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        let st = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let _ = writeln!(
            f,
            "[{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}] {}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds, line
        );
    }
}

// ---------------------------------------------------------------------------
// 枚举（系统开始菜单同源）
// ---------------------------------------------------------------------------

/// COM 枚举主体（STA 线程内执行）：名称 + 解析名 + AppUserModel.ID 属性 +
/// 父相对解析名 + 保留 IShellItem 供同线程提取图标。
struct EnumItem {
    name: String,
    parsing: String,
    aumid: String,
    parent_rel: String,
    item: Option<IShellItem>,
}

unsafe fn com_enum_items() -> Vec<EnumItem> {
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let mut out: Vec<EnumItem> = Vec::new();
    let Ok(folder) = SHGetKnownFolderItem::<IShellItem>(
        &FOLDERID_AppsFolder,
        windows::Win32::UI::Shell::KNOWN_FOLDER_FLAG(0),
        None,
    ) else {
        log("com_enum_items: SHGetKnownFolderItem failed");
        return out;
    };
    let Ok(enum_items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else {
        log("com_enum_items: BindToHandler(BHID_EnumItems) failed");
        return out;
    };
    let mut seen = std::collections::HashSet::new();
    loop {
        let mut buf: [Option<IShellItem>; 64] = std::array::from_fn(|_| None);
        let mut fetched: u32 = 0;
        if enum_items.Next(&mut buf, Some(&mut fetched)).is_err() || fetched == 0 {
            break;
        }
        for item in buf.into_iter().flatten() {
            // 单条容错：.lnk 等条目上解析可能失败（交接单坑），跳过不致命
            let (name, parsing, parent_rel) = match (
                item.GetDisplayName(SIGDN_NORMALDISPLAY),
                item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING),
                item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING),
            ) {
                (Ok(n), Ok(p), Ok(r)) => (n, p, r),
                _ => continue,
            };
            let (Ok(name), Ok(parsing), Ok(parent_rel)) = (
                name.to_string(),
                parsing.to_string(),
                parent_rel.to_string(),
            ) else {
                continue;
            };
            if name.is_empty() || parsing.is_empty() || !seen.insert(parsing.clone()) {
                continue;
            }
            // System.AppUserModel.ID 属性（shell 认定的可启动 AUMID）
            let aumid = item
                .cast::<IShellItem2>()
                .ok()
                .and_then(|it2| it2.GetString(&pkey_app_user_model_id()).ok())
                .and_then(|pw| {
                    let s = pw.to_string().ok();
                    CoTaskMemFree(Some(pw.0 as *const c_void));
                    s
                })
                .unwrap_or_default();
            out.push(EnumItem {
                name,
                parsing,
                aumid,
                parent_rel,
                item: Some(item),
            });
        }
    }
    out
}

/// 枚举 → (显示名, 解析名, AppUserModelID, 父相对解析名)。独立进程（探针）诊断用。
pub struct RawApp {
    pub name: String,
    pub parsing: String,
    pub aumid: String,
    pub parent_rel: String,
}

pub fn enum_apps_raw() -> Vec<RawApp> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(unsafe {
            com_enum_items()
                .into_iter()
                .map(|e| RawApp {
                    name: e.name,
                    parsing: e.parsing,
                    aumid: e.aumid,
                    parent_rel: e.parent_rel,
                })
                .collect()
        });
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(v) => v,
        Err(_) => {
            log("enum_apps_raw: timeout (10s)");
            Vec::new()
        }
    }
}

/// 解析名 → 可启动路径。
///
/// 实测 DESKTOPABSOLUTEPARSING 的形态五花八门（sir 手验「点图标没反应」的根因——
/// 旧规则只有带 `!` 的 AUMID 能救，其余全 launch 失败）：
///   - UWP：`Microsoft.WindowsNotepad_8wekyb3d8bbwe!App`（裸 AUMID，带 `!`）
///   - 已注册 Win32：`ai.opencode.desktop` / `Chrome.45TMI32NM5CFN6J2M4MLTAB6J4`
///     （无 `!` 的 AUMID）/ `Microsoft.AutoGenerated.{GUID}`
///   - 已知文件夹相对：`{1AC14E77-…}\cmd.exe`（GUID = 系统文件夹，需展开）
///   - 畸形：`c:.users.15812.…exe`（反斜杠被换成了点）
///
/// 解析顺序（launchers 通用做法）：**条目属性 System.AppUserModel.ID 优先**——
/// 它是 shell 自己认定可启动的 AUMID，`shell:AppsFolder\<aumid>` 全形态通吃；
/// 无属性再按解析名形态兜底。缓存键仍用原始解析名（改形态不废缓存）。
fn resolve_launch_path(parsing: &str, aumid_prop: &str, parent_rel: &str) -> String {
    // 1) 条目的 AppUserModel.ID 属性：shell 认定的 AUMID（UWP + 注册过的 Win32 全覆盖）
    if !aumid_prop.trim().is_empty() {
        return format!("shell:AppsFolder\\{}", aumid_prop.trim());
    }
    let lower = parsing.to_lowercase();
    // 2) 已知文件夹相对形 `{FOLDERID-GUID}\rest` → 展开成真实路径
    if parsing.starts_with('{') {
        if let Some(end) = parsing.find('}') {
            let guid_str = &parsing[1..end];
            let rest = parsing[end + 1..].strip_prefix('\\').unwrap_or("");
            if let Ok(guid) = guid_from_str(guid_str) {
                if let Some(base) = known_folder_path(&guid) {
                    let p = if rest.is_empty() {
                        base
                    } else {
                        format!("{base}\\{rest}")
                    };
                    if std::path::Path::new(&p).exists() {
                        return p;
                    }
                }
            }
        }
    }
    // 3) 真实路径（盘符或 UNC）
    if (parsing.len() >= 3 && parsing.as_bytes()[1] == b':' && parsing.as_bytes()[2] == b'\\')
        || parsing.starts_with("\\\\")
    {
        return parsing.to_string();
    }
    // 4) 裸 AUMID 带 `!` → shell:AppsFolder 前缀
    if lower.starts_with("shell:") {
        return parsing.to_string();
    }
    if parsing.contains('!') {
        return format!("shell:AppsFolder\\{parsing}");
    }
    // 5) 兜底：父相对解析名进 AppsFolder（AUMID 形态无 `!` 也常在此可启动）
    if !parent_rel.trim().is_empty() {
        return format!("shell:AppsFolder\\{}", parent_rel.trim());
    }
    parsing.to_string()
}

/// SHGetKnownFolderPath 展开（失败 None）。
fn known_folder_path(guid: &windows::core::GUID) -> Option<String> {
    unsafe {
        let pw = SHGetKnownFolderPath(guid, windows::Win32::UI::Shell::KNOWN_FOLDER_FLAG(0), None)
            .ok()?;
        let s = pw.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(pw.0 as *const c_void));
        s
    }
}

/// System.AppUserModel.ID 的 PROPERTYKEY（fmtid 9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3, pid 5）。
/// 内联构造，免引 Win32_Storage_EnhancedStorage feature。
fn pkey_app_user_model_id() -> PROPERTYKEY {
    PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    }
}

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` → GUID（windows-core 无 parse，手解 hex）。
fn guid_from_str(s: &str) -> Result<windows::core::GUID, std::num::ParseIntError> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    u128::from_str_radix(&hex, 16).map(windows::core::GUID::from_u128)
}

/// 首次展开时启动枚举 + 图标提取（后台单线程：名称先落 MENU（面板即刻出网格），
/// 图标随后在同一 COM 线程内逐条补——**用枚举手里的 IShellItem 直接取 factory**，
/// 不走「解析名 → SHCreateItemFromParsingName」二次解析（141/180 失败的教训）。
fn ensure_started() {
    if EXTRACT_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("glassdesk-menu-enum".into())
        .spawn(|| unsafe {
            let t0 = std::time::Instant::now();
            let _ = std::fs::create_dir_all(&*ICON_CACHE_DIR);
            if ICONS.read().map(|g| g.is_none()).unwrap_or(true) {
                if let Ok(mut g) = ICONS.write() {
                    *g = Some(HashMap::new());
                }
            }
            // 枚举（保留 IShellItem 用于同线程提取）
            let items = com_enum_items();
            let apps: Vec<MenuApp> = items
                .iter()
                .map(|e| MenuApp {
                    id: format!("m{:016x}", hash_str(&e.parsing)),
                    name: e.name.clone(),
                    path: resolve_launch_path(&e.parsing, &e.aumid, &e.parent_rel),
                    icon: None,
                })
                .collect();
            log(&format!(
                "enum done: {} apps in {}ms",
                apps.len(),
                t0.elapsed().as_millis()
            ));
            if let Ok(mut g) = MENU.write() {
                *g = Some(apps);
            }
            ENUM_DONE.store(true, Ordering::SeqCst);
            seed_pinned(&items);

            // 图标：磁盘缓存优先，缺失的用手里 IShellItem 现场提取
            let mut extracted = 0usize;
            let mut cached = 0usize;
            let mut failed = 0usize;
            for (idx, e) in items.iter().enumerate() {
                let key = cache_key(&e.parsing);
                let id = format!("m{:016x}", hash_str(&e.parsing));
                if let Ok(g) = ICONS.read() {
                    if let Some(map) = g.as_ref() {
                        if map.contains_key(&key) {
                            apply_icon(&id, key);
                            cached += 1;
                            continue;
                        }
                    }
                }
                let disk = ICON_CACHE_DIR.join(format!("i{:016x}.bin", hash_str(&key)));
                let icon = match load_disk_icon(&disk) {
                    Some(d) => {
                        cached += 1;
                        d
                    }
                    None => match e
                        .item
                        .as_ref()
                        .and_then(|it| it.cast::<windows::Win32::UI::Shell::IShellItemImageFactory>().ok())
                        .and_then(|f| {
                            f.GetImage(
                                windows::Win32::Foundation::SIZE { cx: 48, cy: 48 },
                                SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
                            )
                            .ok()
                        })
                        .and_then(|hbmp| {
                            let d = crate::dock::bitmap_to_rgba(hbmp);
                            let _ = windows::Win32::Graphics::Gdi::DeleteObject(hbmp.into());
                            d
                        }) {
                        Some(d) => {
                            extracted += 1;
                            save_disk_icon(&disk, &d);
                            d
                        }
                        None => {
                            failed += 1;
                            if failed <= 5 {
                                log(&format!("icon extract failed: '{}' ({})", e.name, e.parsing));
                            }
                            continue;
                        }
                    },
                };
                if let Ok(mut g) = ICONS.write() {
                    if let Some(map) = g.as_mut() {
                        map.insert(key.clone(), icon);
                    }
                }
                apply_icon(&id, key);
                if idx % 50 == 0 {
                    log(&format!("icon progress {}/{}", idx + 1, items.len()));
                }
            }
            EXTRACT_DONE.store(true, Ordering::SeqCst);
            log(&format!(
                "icon extract done: {} items (fresh_extracted={extracted} cache_hit={cached} failed={failed}) in {}ms",
                items.len(),
                t0.elapsed().as_millis()
            ));
        })
        .expect("spawn menu enum");
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

// ---------------------------------------------------------------------------
// 图标提取与缓存（磁盘 + 内存）
// ---------------------------------------------------------------------------

/// 缓存键 = 解析名 + mtime 分量（Win32 文件取 mtime；UWP 无稳定文件取 ""）。
fn cache_key(path: &str) -> String {
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();
    format!("{}|{}", path.to_lowercase(), mtime)
}

fn load_disk_icon(path: &PathBuf) -> Option<IconData> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 8 {
        return None;
    }
    let w = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let h = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    if w == 0 || h == 0 || w > 512 || h > 512 || bytes.len() < 8 + (w * h * 4) as usize {
        return None;
    }
    Some(IconData {
        w,
        h,
        rgba: crate::dock::b64_encode(&bytes[8..]),
    })
}

fn save_disk_icon(path: &PathBuf, d: &IconData) {
    if let Some(raw) = crate::dock::b64_decode(&d.rgba) {
        let mut out = Vec::with_capacity(8 + raw.len());
        out.extend_from_slice(&d.w.to_le_bytes());
        out.extend_from_slice(&d.h.to_le_bytes());
        out.extend_from_slice(&raw);
        let _ = std::fs::write(path, out);
    }
}

/// %LOCALAPPDATA%\com.glasssuite.app\icon_cache。
static ICON_CACHE_DIR: LazyLock<std::path::PathBuf> = LazyLock::new(|| {
    std::env::var("LOCALAPPDATA")
        .map(|d| {
            PathBuf::from(d)
                .join("com.glasssuite.app")
                .join("icon_cache")
        })
        .unwrap_or_default()
});

/// 把提取好的图标回填进 MENU 条目。
fn apply_icon(id: &str, key: String) {
    let icon = ICONS
        .read()
        .ok()
        .and_then(|g| g.as_ref()?.get(&key).cloned());
    let Some(icon) = icon else { return };
    if let Ok(mut g) = MENU.write() {
        if let Some(apps) = g.as_mut() {
            if let Some(a) = apps.iter_mut().find(|a| a.id == id) {
                a.icon = Some(icon);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 面板生命周期
// ---------------------------------------------------------------------------

fn panel(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window("startmenu")
}

pub fn is_panel_visible(app: &tauri::AppHandle) -> bool {
    panel(app)
        .map(|w| w.is_visible().unwrap_or(false))
        .unwrap_or(false)
}

/// ⊞ 点击：可见 → 收回；隐藏 → 定位 + 展开 + 焦点（搜索框需要）。
pub fn toggle(app: &tauri::AppHandle) {
    let Some(win) = panel(app) else {
        log("toggle: startmenu window missing");
        return;
    };
    if win.is_visible().unwrap_or(false) {
        BLUR_GEN.fetch_add(1, Ordering::SeqCst);
        let _ = win.hide();
        log("panel hide (toggle)");
        return;
    }
    ensure_started();
    // 定位：dock 左端上方（物理坐标，显示器内夹紧）。
    // 热路径优化：dock 没动就跳过 SetWindowPos（二次展开 ≤50ms 判据的敏感项）。
    let t_toggle = std::time::Instant::now();
    let mut positioned = false;
    if let Some((x, y)) = app
        .get_webview_window("dock")
        .and_then(|d| panel_anchor(&d, &win))
    {
        let changed = {
            let last = LAST_ANCHOR.lock().ok();
            match last.as_deref() {
                Some(a) => *a != Some((x, y)),
                None => true,
            }
        };
        if changed {
            let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
            if let Ok(mut l) = LAST_ANCHOR.lock() {
                *l = Some((x, y));
            }
            positioned = true;
        }
    }
    BLUR_GEN.fetch_add(1, Ordering::SeqCst);
    let _ = win.show();
    if let Err(e) = win.set_focus() {
        log(&format!("set_focus failed: {e}"));
    }
    log(&format!(
        "panel show in {}ms (positioned={positioned})",
        t_toggle.elapsed().as_millis()
    ));
    // 焦点保险：show+set_focus 后首个 ~120ms 内前台会被点开的 dock 抢回
    // （首轮实测 GAINED→+120ms LOST）。只补抢**本进程 dock** 的抢回——
    // 前台去了别的进程 = 用户点面板外，绝不抢（否则失焦收回永远不触发）。
    let win2 = win.clone();
    std::thread::spawn(move || {
        let panel_h = win2.hwnd().ok().map(|h| h.0 as isize);
        for delay in [100u64, 300, 600, 1000] {
            std::thread::sleep(Duration::from_millis(delay));
            if !win2.is_visible().unwrap_or(false) {
                return;
            }
            let fg = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            if panel_h == Some(fg.0 as isize) {
                return; // 焦点已稳
            }
            let mut fg_pid = 0u32;
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                    fg,
                    Some(&mut fg_pid),
                );
            }
            if fg_pid != std::process::id() {
                log(&format!(
                    "focus re-assert aborted (t={delay}ms, fg is foreign)"
                ));
                return; // 用户点了别的应用——收回语义接管
            }
            log(&format!(
                "focus re-assert fired (t={delay}ms, fg is own dock)"
            ));
            let _ = win2.set_focus();
        }
    });
    log("panel show (toggle, focused)");
}

/// 面板锚点：dock 窗口左端上方，夹回面板所在显示器。
fn panel_anchor(dock: &tauri::WebviewWindow, win: &tauri::WebviewWindow) -> Option<(i32, i32)> {
    let hwnd = HWND(dock.hwnd().ok()?.0);
    let mut r = RECT::default();
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut r).ok()? };
    let size = win.outer_size().ok()?;
    let hmon = unsafe {
        windows::Win32::Graphics::Gdi::MonitorFromWindow(
            hwnd,
            windows::Win32::Graphics::Gdi::MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut mi = windows::Win32::Graphics::Gdi::MONITORINFO {
        cbSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::GetMonitorInfoW(hmon, &mut mi);
    };
    let m = mi.rcMonitor;
    let x = r.left.clamp(m.left + 4, m.right - size.width as i32 - 4);
    let y = (r.top - size.height as i32 - PANEL_GAP).max(m.top + 4);
    Some((x, y))
}

/// 失焦收回：延迟 150ms 复查前台——前台仍属**本进程**（面板自身/dock，含焦点在
/// 自己窗口间移动的任何情形）一律不收，交给 ⊞ toggle / Esc / 图标点击的显式语义；
/// 前台去了别的进程（点了面板外的应用/桌面）才收回。
pub fn on_blur(app: &tauri::AppHandle) {
    let Some(win) = panel(app) else { return };
    let gen = BLUR_GEN.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        // 双段确认：失焦后 150ms + 再 200ms 两次都看到「前台在别的进程」才收——
        // 中间态（焦点补抢重夺面板）不算失焦意图。
        let fg_foreign = || -> bool {
            let fg = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            let mut fg_pid = 0u32;
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                    fg,
                    Some(&mut fg_pid),
                );
            }
            fg_pid != std::process::id()
        };
        std::thread::sleep(Duration::from_millis(BLUR_DELAY_MS));
        if BLUR_GEN.load(Ordering::SeqCst) != gen {
            return; // 期间有 show/hide，本任务过期
        }
        if !win.is_visible().unwrap_or(false) {
            return;
        }
        if !fg_foreign() {
            log("blur skipped (fg is own window)");
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
        if BLUR_GEN.load(Ordering::SeqCst) != gen || !win.is_visible().unwrap_or(false) {
            return;
        }
        if !fg_foreign() {
            log("blur skipped (fg recovered to own window)");
            return;
        }
        BLUR_GEN.fetch_add(1, Ordering::SeqCst);
        let _ = win.hide();
        log("panel hide (blur)");
    });
}

/// 焦点事件日志（lib.rs 窗口事件回调转接）——诊断 set_focus 链路用。
pub fn log_focus(gained: bool) {
    log(&format!(
        "panel focus {}",
        if gained { "GAINED" } else { "LOST" }
    ));
}

/// 显式收回（Esc / 图标点击启动后）。
pub fn hide(app: &tauri::AppHandle) {
    BLUR_GEN.fetch_add(1, Ordering::SeqCst);
    if let Some(win) = panel(app) {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
            log("panel hide (explicit)");
        }
    }
}

// ---------------------------------------------------------------------------
// 收藏（sir 2026-10-31 反馈「图标太多，要可自由添加减少」）：
// menu.json 持久化 + ⊕ 添加模式（前端）+ 右键移除（dock 同款交互）
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone)]
pub struct MenuEntry {
    pub name: String,
    pub path: String,
}

#[derive(Serialize, Deserialize, Default)]
struct MenuConfig {
    #[serde(default)]
    apps: Vec<MenuEntry>,
}

/// %APPDATA%\dev.glassdesk.app\menu.json（与 dock.json 同目录；UTF-8 无 BOM）。
fn menu_config_path() -> Option<PathBuf> {
    std::env::var("APPDATA").ok().map(|d| {
        PathBuf::from(d)
            .join("com.glasssuite.app")
            .join("menu.json")
    })
}

static PINNED: RwLock<Option<Vec<MenuEntry>>> = RwLock::new(None);
static PINNED_LOADED: AtomicBool = AtomicBool::new(false);

fn load_pinned() {
    if PINNED_LOADED.swap(true, Ordering::SeqCst) {
        return;
    }
    let entries = menu_config_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<MenuConfig>(&b).ok())
        .map(|c| c.apps)
        .unwrap_or_default();
    if let Ok(mut g) = PINNED.write() {
        *g = Some(entries);
    }
}

fn save_pinned(entries: &[MenuEntry]) -> Result<(), String> {
    let path = menu_config_path().ok_or("APPDATA 不可用")?;
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
    let json = serde_json::to_vec_pretty(&MenuConfig {
        apps: entries.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    // v0.6：原子写（同 dock.json——C-ADV-2，面板收藏丢了一样疼）
    crate::dock::atomic_write(&path, &json)
}

fn pinned_paths() -> Vec<String> {
    load_pinned();
    PINNED
        .read()
        .ok()
        .and_then(|g| g.clone())
        .map(|v| v.iter().map(|e| e.path.clone()).collect())
        .unwrap_or_default()
}

/// 首次运行种子：枚举完成后 menu.json 不存在 → 按显示名抓几个常用应用。
fn seed_pinned(items: &[EnumItem]) {
    let path = match menu_config_path() {
        Some(p) => p,
        None => return,
    };
    if path.exists() {
        return; // 已有配置（含空配置）不覆盖
    }
    const SEEDS: [&str; 5] = [
        "文件资源管理器",
        "记事本",
        "计算器",
        "命令提示符",
        "Microsoft Edge",
    ];
    let entries: Vec<MenuEntry> = SEEDS
        .iter()
        .filter_map(|seed| {
            items.iter().find(|e| e.name == *seed).map(|e| MenuEntry {
                name: e.name.clone(),
                path: resolve_launch_path(&e.parsing, &e.aumid, &e.parent_rel),
            })
        })
        .collect();
    log(&format!("seed pinned: {} entries", entries.len()));
    if save_pinned(&entries).is_ok() {
        if let Ok(mut g) = PINNED.write() {
            *g = Some(entries);
        }
    }
}

/// 添加收藏（按 path 去重；名字从 catalog 反查）。
pub fn add_pinned(path: &str) -> Result<Vec<String>, String> {
    load_pinned();
    let name = MENU
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default()
        .into_iter()
        .find(|a| a.path == path)
        .map(|a| a.name)
        .ok_or_else(|| format!("catalog 中无此条目: {path}"))?;
    let mut entries = PINNED
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default();
    if !entries.iter().any(|e| e.path == path) {
        entries.push(MenuEntry {
            name,
            path: path.to_string(),
        });
        save_pinned(&entries)?;
        if let Ok(mut g) = PINNED.write() {
            *g = Some(entries);
        }
        log(&format!("pinned add: {path}"));
    }
    Ok(pinned_paths())
}

/// 移除收藏。
pub fn remove_pinned(path: &str) -> Result<Vec<String>, String> {
    load_pinned();
    let mut entries = PINNED
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default();
    entries.retain(|e| e.path != path);
    save_pinned(&entries)?;
    if let Ok(mut g) = PINNED.write() {
        *g = Some(entries);
    }
    log(&format!("pinned remove: {path}"));
    Ok(pinned_paths())
}

/// 面板内启动：失败要可见（记日志、返回 Err 让前端不关面板）。
pub fn launch_from_menu(path: &str) -> Result<(), String> {
    match crate::dock::launch(path) {
        Ok(()) => {
            log(&format!("launch ok: {path}"));
            Ok(())
        }
        Err(e) => {
            log(&format!("launch FAILED: {path} ({e})"));
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// 命令数据（lib.rs get_menu_apps）
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct MenuSnapshot {
    pub ready: bool,
    pub icons_done: bool,
    pub apps: Vec<MenuApp>,
    pub pinned: Vec<String>,
}

pub fn snapshot() -> MenuSnapshot {
    let apps = MENU.read().ok().and_then(|g| g.clone()).unwrap_or_default();
    MenuSnapshot {
        ready: ENUM_DONE.load(Ordering::SeqCst),
        icons_done: EXTRACT_DONE.load(Ordering::SeqCst),
        pinned: pinned_paths(),
        apps,
    }
}

/// 探针用：独立进程内跑一次枚举，返回条目数与采样。
pub fn probe_enum_stats() -> (usize, Vec<String>) {
    let raw = enum_apps_raw();
    let names: Vec<String> = raw.iter().map(|e| e.name.clone()).collect();
    (raw.len(), names)
}

/// 探针用：按名字找条目并给出**最终解析出的可启动路径**（menupath 诊断）。
pub fn probe_resolved(name: &str) -> Vec<(String, String)> {
    enum_apps_raw()
        .into_iter()
        .filter(|e| e.name.contains(name))
        .map(|e| {
            (
                e.name.clone(),
                resolve_launch_path(&e.parsing, &e.aumid, &e.parent_rel),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// OC-20261104-001：临时运行图标（transient）对目录的只读反查。
// 只新增公共函数供 dock.rs / 探针调用，不改既有函数行为（卡面 §3 owned files 约束）。
// ---------------------------------------------------------------------------

/// 首次调用时懒启动目录枚举 + 图标提取（与面板展开共用同一条后台线程，
/// EXTRACT_STARTED 去重；只为让 transient 反查能拿到 friendly name / 图标）。
pub fn ensure_catalog_started() {
    ensure_started();
}

/// 目录条目（含 icon 字段快照）。内部诊断/反查用。
pub struct CatalogHit {
    pub name: String,
    /// 可启动解析路径（`shell:AppsFolder\<AUMID>` 或 .lnk / 真实路径）
    pub path: String,
    pub icon: Option<crate::dock::IconData>,
}

/// 按 AUMID（不区分大小写）反查目录条目。
pub fn catalog_lookup_aumid(aumid: &str) -> Option<CatalogHit> {
    let want = format!("shell:appsfolder\\{}", aumid.trim().to_lowercase());
    let apps = MENU.read().ok()?.clone()?;
    apps.into_iter()
        .find(|a| a.path.to_lowercase() == want)
        .map(|a| CatalogHit {
            name: a.name,
            path: a.path,
            icon: a.icon,
        })
}

/// 按（可启动解析）路径反查目录条目。
pub fn catalog_lookup_path(path: &str) -> Option<CatalogHit> {
    let want = path.trim().to_lowercase();
    let apps = MENU.read().ok()?.clone()?;
    apps.into_iter()
        .find(|a| a.path.to_lowercase() == want)
        .map(|a| CatalogHit {
            name: a.name,
            path: a.path,
            icon: a.icon,
        })
}

/// menu.json 收藏路径快照（dock.rs 计算「排除收藏 key 集合」用）。
pub fn all_pinned_paths() -> Vec<String> {
    pinned_paths()
}

/// 触发目录枚举（探针诊断用：独立进程内不方便起 watcher，直接跑一次枚举）。
pub fn probe_catalog_stats() -> (usize, usize) {
    ensure_started();
    let apps = MENU.read().ok().and_then(|g| g.clone()).unwrap_or_default();
    (apps.len(), apps.iter().filter(|a| a.icon.is_some()).count())
}

// ---------------------------------------------------------------------------
// 电源菜单（OC-20261101-001 §1.2）：锁定 / 睡眠 / 关机 / 重启。
// 关机/重启必须先启用 SE_SHUTDOWN_NAME 特权；禁 EWX_FORCE（强杀不给保存机会），
// 用 EWX_FORCEIFHUNG（仅挂起的进程强杀）——sir 误触不能丢工作，UI 层另有二次确认。
// ---------------------------------------------------------------------------

/// 启用当前进程 token 的 SeShutdownPrivilege。
fn enable_shutdown_privilege() -> Result<(), String> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .map_err(|e| format!("OpenProcessToken: {e}"))?;
        let mut luid = windows::Win32::Foundation::LUID::default();
        LookupPrivilegeValueW(
            PCWSTR::null(),
            windows::core::w!("SeShutdownPrivilege"),
            &mut luid,
        )
        .map_err(|e| format!("LookupPrivilegeValueW: {e}"))?;
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let r = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        let _ = windows::Win32::Foundation::CloseHandle(token);
        r.map_err(|e| format!("AdjustTokenPrivileges: {e}"))?;
        // AdjustTokenPrivileges 成功 ≠ 特权已启用：ERROR_NOT_ALL_ASSIGNED 时 GetLastError
        let err = windows::Win32::Foundation::GetLastError();
        if err == windows::Win32::Foundation::ERROR_NOT_ALL_ASSIGNED {
            return Err("SeShutdownPrivilege 未分配给当前用户".into());
        }
        Ok(())
    }
}

/// 锁定（user32 LockWorkStation；系统自行切锁屏）。
pub fn power_lock() -> Result<(), String> {
    let r =
        unsafe { windows::Win32::System::Shutdown::LockWorkStation() }.map_err(|e| e.to_string());
    log(&format!(
        "power lock: {}",
        if r.is_ok() { "ok" } else { "failed" }
    ));
    r
}

/// 睡眠（powrprof SetSuspendState(hibernate=false, force=false, wakeupdisabled=false)）。
/// 若系统开启了休眠，实际可能进休眠而非睡眠（卡面已注明，交付单记录行为）。
pub fn power_sleep() -> Result<(), String> {
    let ok = unsafe { windows::Win32::System::Power::SetSuspendState(false, false, false) };
    log(&format!("power sleep: ok={ok}"));
    if ok {
        Ok(())
    } else {
        Err("SetSuspendState 返回 false".into())
    }
}

/// 关机 / 重启（ExitWindowsEx + EWX_FORCEIFHUNG，不用 EWX_FORCE）。
pub fn power_shutdown(reboot: bool) -> Result<(), String> {
    use windows::Win32::System::Shutdown::{
        ExitWindowsEx, EWX_FORCEIFHUNG, EWX_REBOOT, EWX_SHUTDOWN, SHTDN_REASON_FLAG_PLANNED,
    };
    enable_shutdown_privilege()?;
    let flags = if reboot { EWX_REBOOT } else { EWX_SHUTDOWN } | EWX_FORCEIFHUNG;
    let r = unsafe { ExitWindowsEx(flags, SHTDN_REASON_FLAG_PLANNED) }.map_err(|e| e.to_string());
    log(&format!(
        "power {}: {}",
        if reboot { "reboot" } else { "shutdown" },
        if r.is_ok() { "initiated" } else { "failed" }
    ));
    r
}
