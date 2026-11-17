//! 底部玻璃图标栏 —— 配置、快捷方式管理、图标提取、启动、自动隐藏策略。
//!
//! 快捷方式管理选型（卡面 Review Q3）：**配置文件 + UI 增删**——固定快捷方式扩展性差，
//! 拖拽添加实现复杂且与玻璃覆盖窗冲突；JSON 配置 + 文件对话框添加 + 右键移除，
//! 实现简单、行为可预期。
//! 自动隐藏策略（Review Q4）：光标靠近屏幕底边或悬停图标栏时显示，离开 800ms 后隐藏；
//! 前台应用全屏时立即隐藏（不挡游戏/视频）。

use std::collections::hash_map::DefaultHasher;
use std::ffi::c_void;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    STGM,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellExecuteW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetCursorPos, GetIconInfo, PrivateExtractIconsW, HICON, ICONINFO, SW_SHOWNORMAL,
};

pub mod desktop;
pub mod menu;
pub mod preview;
pub mod running;
pub mod taskbar;

pub mod api;

// Tauri 用于窗口显示/隐藏与定位
use tauri::{Manager, PhysicalPosition};

const ICON_SIZE: i32 = 32;
const DOCK_MARGIN: i32 = 6; // dock 与任务栏的间距（物理像素）
const REVEAL_EDGE_PX: i64 = 4; // 底边触发带高度
const DOCK_HOVER_MARGIN: f64 = 24.0; // dock 周边容差
const HIDE_DELAY_MS: u64 = 800;
const POLL_MS: u64 = 200;
/// 首次显示前的额外等待（≈一帧）：WebView2 报就绪后再等一拍，避免刚就绪就抢在合成前显示。
const FIRST_SHOW_DELAY_MS: u64 = 50;
/// 兜底：前端一直没报就绪（JS 异常等）时，最多等这么久就强制放行。
const FORCE_READY_AFTER_MS: u64 = 5000;

/// dock WebView2 首帧是否已就绪（前端 `dock_ready` 上报）。
static DOCK_READY: AtomicBool = AtomicBool::new(false);

/// 前端上报首帧已绘制。
pub fn mark_ready() {
    DOCK_READY.store(true, Ordering::SeqCst);
}

/// 首帧是否就绪。
pub fn is_ready() -> bool {
    DOCK_READY.load(Ordering::SeqCst)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IconData {
    pub w: u32,
    pub h: u32,
    /// RGBA，base64
    pub rgba: String,
}

/// 图标所在层（v0.7 Phase 1，OC-20261102-002）。
///
/// - `Dock` = 在 dock 行里（v0.6 及之前只有这一种）
/// - `Free` = 游离图标（独立顶层透明窗口，位置记在 `free_rect`）
///
/// 老配置（v0.6 及之前）没有这个字段，`serde(default)` → `Dock`。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum IconMode {
    #[default]
    Dock,
    Free,
}

impl IconMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            IconMode::Dock => "dock",
            IconMode::Free => "free",
        }
    }
}

/// 游离图标的位置矩形（**物理像素**、**虚拟桌面坐标**——副屏在左/上时坐标为负）。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct FreeRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl FreeRect {
    pub fn center_x(&self) -> i32 {
        self.x + (self.w as i32) / 2
    }
    pub fn center_y(&self) -> i32 {
        self.y + (self.h as i32) / 2
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Shortcut {
    pub id: String,
    pub name: String,
    pub path: String,
    pub icon: IconData,
    /// UWP/打包应用的 AUMID（任务 A：运行指示匹配用）。
    /// Win32 快捷方式为 None；`shell:AppsFolder\<AUMID>` 形式的路径在建条时自动提取。
    /// serde(default)：v0.3 之前的 dock.json 老数据无此字段，加载为 None。
    #[serde(default)]
    pub aumid: Option<String>,
    /// 图标所在层（v0.7 Phase 1）。serde(default) → Dock，老配置零改动可加载。
    #[serde(default)]
    pub mode: IconMode,
    /// 游离模式下的窗口矩形（物理像素虚拟桌面坐标）；dock 模式为 None。
    #[serde(default)]
    pub free_rect: Option<FreeRect>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DockConfig {
    pub auto_hide: bool,
    #[serde(default)]
    pub shortcuts: Vec<Shortcut>,
    /// true = dock 常驻最顶层（旧 D5 行为）；false = 桌面层（默认，全屏/最大化窗口会盖住它）。
    /// sir 2026-10-31 裁定：走桌面层——置顶会干扰全屏/最大化操作。
    #[serde(default)]
    pub dock_top_layer: bool,
}

impl Default for DockConfig {
    fn default() -> Self {
        // v0.2.y（sir 2026-10-30 反馈「dock 一直显示挡住视野」）：默认改回自动隐藏。
        // 全屏让位逻辑不受此值影响（is_foreground_fullscreen 命中时一律隐藏）。
        DockConfig {
            auto_hide: true,
            shortcuts: Vec::new(),
            dock_top_layer: false,
        }
    }
}

pub fn config_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("dock.json"))
}

/// `dock.json` 的备份文件（`<name>.bak`）——「最近一次可解析版本」的快照。
/// 写法：原子写之前先拷贝一份当前文件；解析不了就不备份，不会拿坏配置污染 .bak。
fn bak_path_for(path: &Path) -> std::path::PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    match path.file_name() {
        Some(n) => parent.join(format!("{}.bak", n.to_string_lossy())),
        None => parent.join("dock.json.bak"),
    }
}

/// 保存前刷新 .bak（最近一次好配置）。
fn snapshot_bak(path: &Path) -> bool {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return false,
    };
    if serde_json::from_slice::<DockConfig>(&bytes).is_err() {
        return false; // 坏配置不进 .bak
    }
    std::fs::write(bak_path_for(path), &bytes).is_ok()
}

/// .bak 里救回来的配置。
fn try_load_bak(path: &Path) -> Option<DockConfig> {
    std::fs::read(bak_path_for(path))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

pub fn load_config(path: &Path) -> DockConfig {
    let cfg: DockConfig = match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<DockConfig>(&bytes) {
            Ok(cfg) => cfg,
            Err(e) => {
                // 绝不静默吞数据：原文件改名保留（dock.corrupt-<ts>.json），
                // 然后从 .bak 恢复最近一次好配置；.bak 不可用才回退默认。
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let corrupt = path.with_file_name(format!("dock.corrupt-{stamp}.json"));
                if std::fs::rename(path, &corrupt).is_ok() {
                    winplat::log::log(&format!(
                        "dock: dock.json 解析失败（{e}），原文件已保留为 {}",
                        corrupt.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                    ));
                } else {
                    winplat::log::log(&format!(
                        "dock: dock.json 解析失败（{e}），原文件保留原位（改名失败）"
                    ));
                }
                match try_load_bak(path) {
                    Some(cfg) => {
                        winplat::log::log("dock: 已从 dock.json.bak 恢复配置");
                        // 立刻回写主文件——否则本会话内后续 load_config
                        // 会读到「缺失」而回退默认，把恢复成果打回原形。
                        // （此刻主文件已被改名保留，save_config 不会触发
                        //  「拒绝覆盖无法解析」的护栏）
                        if let Err(e) = save_config(path, &cfg) {
                            winplat::log::log(&format!(
                                "dock: 回写恢复后的 dock.json 失败：{e}（内存中已恢复，本会话仍有效）"
                            ));
                        }
                        cfg
                    }
                    None => {
                        winplat::log::log("dock: dock.json.bak 不可用，回退默认配置");
                        DockConfig::default()
                    }
                }
            }
        },
        Err(_) => {
            // 文件缺失/不可读 → 干净重来（不复活过时的 .bak——
            // 用户主动删 dock.json 重置是常见用法，不该悄悄回来）。
            DockConfig::default()
        }
    };
    // 008 步骤 4（配置兼容）：游离图标功能已退役——旧 dock.json 里的
    // mode:"free" 条目**原位**收敛回 dock 行（free_rect 剥离，相对顺序不变），
    // 读盘即迁移；写回任何配置时落盘的就是收敛后的形态。
    let had_free = cfg.shortcuts.iter().any(|s| s.mode == IconMode::Free);
    if had_free {
        let mut cfg = cfg;
        let n = cfg
            .shortcuts
            .iter()
            .filter(|s| s.mode == IconMode::Free)
            .count();
        for s in &mut cfg.shortcuts {
            if s.mode == IconMode::Free {
                s.mode = IconMode::Dock;
                s.free_rect = None;
            }
        }
        winplat::log::log(&format!(
            "dock: 自由图标已退役，{n} 条 free 条目收敛回 dock 行"
        ));
        return cfg;
    }
    cfg
}

pub fn save_config(path: &Path, cfg: &DockConfig) -> Result<(), String> {
    // 拒绝覆盖一个无法解析的文件——那样就把用户数据永远抹掉了。
    // （正常流程里 load_config 已把它改名保留，这里是最后一道闸门）
    if let Ok(bytes) = std::fs::read(path) {
        if serde_json::from_slice::<DockConfig>(&bytes).is_err() {
            return Err("拒绝覆盖无法解析的 dock.json（已保留原文件）".to_string());
        }
    }
    let _ = snapshot_bak(path);
    let json = serde_json::to_vec_pretty(cfg).map_err(|e| e.to_string())?;
    atomic_write(path, &json)?;
    bump_config_version();
    Ok(())
}

/// 配置版本号（v0.7 Phase 1）：每次写盘 +1。
///
/// 前端用它判断「dock.json 被改过」→ 重新拉列表（拖出后图标离开 dock 行、
/// 拖回后图标回到 dock 行，都靠这个版本号驱动列表刷新）。
static CONFIG_VERSION: AtomicU64 = AtomicU64::new(0);

pub fn config_version() -> u64 {
    CONFIG_VERSION.load(Ordering::SeqCst)
}

pub fn bump_config_version() {
    CONFIG_VERSION.fetch_add(1, Ordering::SeqCst);
    // 006：配置版本变化 → transients/运行标识随之变 → 主动推统一快照
    // （前端据此重拉 list_shortcuts；事件丢失由低频兜底轮询兜住）。
    crate::dock::running::push_snapshot();
}

/// 路径归一化（v0.6 反查兜底，v0.4-a verdict advisory）：小写 + `/`→`\`。
/// 前端传回的 path 若与 dock.json 存的大小写/分隔符不同，逐字节比较会把 UWP
/// 应用当 Win32 启动（AUMID 反查失败 → activate_or_launch 找不到窗口）。
pub fn normalize_path_key(p: &str) -> String {
    p.trim().to_lowercase().replace('/', "\\")
}

/// 按 path 反查快捷方式（归一化后比较）。launch_shortcut 与探针共用。
pub fn find_shortcut_by_path<'a>(cfg: &'a DockConfig, path: &str) -> Option<&'a Shortcut> {
    let key = normalize_path_key(path);
    cfg.shortcuts
        .iter()
        .find(|s| normalize_path_key(&s.path) == key)
}

/// 原子写（v0.6，verdict C-ADV-2）：**tmp 同目录 + rename**。
/// 直接 `fs::write` 覆写是非原子的——写盘瞬间断电/杀进程 = 配置截断、快捷方式全丢。
/// tmp 与目标**同目录**（同卷 rename 才原子；跨卷 rename 会失败，卡面硬约束 4）。
/// Windows 上 std rename = MoveFileEx(MOVEFILE_REPLACE_EXISTING)，可覆盖已存在目标。
/// rename 失败时清掉 tmp 残档，主配置保持上一个完整版本。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| "配置路径无父目录".to_string())?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| "配置路径无文件名".to_string())?;
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, bytes).map_err(|e| format!("tmp 写入失败: {e}"))?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(format!("rename 失败（主配置未动）: {e}"))
        }
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// b64 编码（menu 图标磁盘缓存共用）。
pub(crate) fn b64_encode(data: &[u8]) -> String {
    b64(data)
}

/// base64 解码（标准字母表，容忍缺 padding）。preview/menu 共用。
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' => {
                T.iter().position(|&t| t == c).map(|i| i as u32)
            }
            _ => None,
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| !b" \n\r\t=".contains(b)).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for ch in bytes.chunks(4) {
        if ch.len() < 2 {
            break;
        }
        let n = (val(ch[0])? << 18)
            | (val(ch[1])? << 12)
            | (val(*ch.get(2).unwrap_or(&b'A'))? << 6)
            | val(*ch.get(3).unwrap_or(&b'A'))?;
        out.push((n >> 16) as u8);
        if ch.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if ch.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

fn id_for(path: &str) -> String {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    format!("sc{:016x}", h.finish())
}

/// 生成一个快捷方式条目（图标提取失败时给占位灰色块，仍可用）。
///
/// 支持两种路径形态：
/// - 普通文件路径（.exe / .lnk）—— Win32 应用，`aumid = None`
/// - `shell:AppsFolder\<AUMID>` —— UWP/Store 应用（系统开始菜单同源解析名；
///   ShellExecuteW 可直接启动）。friendly name 与图标走 AppsFolder 的
///   IShellItem（COM 在独立 STA 线程执行，3s 超时）。
pub fn build_shortcut(path: &str) -> Shortcut {
    if is_apps_folder_path(path) {
        let aumid = apps_folder_aumid(path);
        let (name, icon) =
            apps_folder_info(&aumid).unwrap_or_else(|| (fallback_name(&aumid), placeholder_icon()));
        return Shortcut {
            id: id_for(path),
            name,
            path: path.to_string(),
            icon,
            aumid: Some(aumid),
            mode: IconMode::Dock,
            free_rect: None,
        };
    }
    // 应用执行别名（如 system32\notepad.exe，0 字节 reparse point）：stub 文件没有
    // 图标，按解析出的 AUMID 走 AppsFolder 拿 friendly name + 图标，匹配也靠 AUMID。
    if let Some(aumid) = crate::dock::running::alias_aumid(path) {
        let (name, icon) =
            apps_folder_info(&aumid).unwrap_or_else(|| (fallback_name(&aumid), placeholder_icon()));
        return Shortcut {
            id: id_for(path),
            name,
            path: path.to_string(),
            icon,
            aumid: Some(aumid),
            mode: IconMode::Dock,
            free_rect: None,
        };
    }
    let name = Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());
    let icon = extract_icon_rgba(path).unwrap_or_else(placeholder_icon);
    Shortcut {
        id: id_for(path),
        name,
        path: path.to_string(),
        icon,
        aumid: None,
        mode: IconMode::Dock,
        free_rect: None,
    }
}

fn is_apps_folder_path(path: &str) -> bool {
    path.to_ascii_lowercase().starts_with("shell:appsfolder\\")
}

fn apps_folder_aumid(path: &str) -> String {
    path[("shell:AppsFolder\\").len()..].trim().to_string()
}

fn fallback_name(aumid: &str) -> String {
    aumid.split('!').next_back().unwrap_or(aumid).to_string()
}

fn placeholder_icon() -> IconData {
    IconData {
        w: 32,
        h: 32,
        rgba: b64(&vec![0x88u8; 32 * 32 * 4]),
    }
}

/// AppsFolder 条目的 friendly name + 48px 图标。
fn apps_folder_info(aumid: &str) -> Option<(String, IconData)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let aumid = aumid.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(unsafe { com_apps_folder_info(&aumid) });
    });
    rx.recv_timeout(Duration::from_secs(3)).ok().flatten()
}

unsafe fn com_apps_folder_info(aumid: &str) -> Option<(String, IconData)> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{
        IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY,
        SIIGBF_BIGGERSIZEOK,
    };

    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let parsing = format!("shell:AppsFolder\\{aumid}");
    let wide = to_wide(&parsing);
    let item: IShellItem =
        SHCreateItemFromParsingName(PCWSTR::from_raw(wide.as_ptr()), None).ok()?;

    let name = item
        .GetDisplayName(SIGDN_NORMALDISPLAY)
        .ok()
        .and_then(|pw| {
            let s = pw.to_string().ok().filter(|s| !s.is_empty());
            windows::Win32::System::Com::CoTaskMemFree(Some(pw.0 as *const c_void));
            s
        })
        .unwrap_or_else(|| fallback_name(aumid));

    let icon = item
        .cast::<IShellItemImageFactory>()
        .ok()
        .and_then(|factory| {
            factory
                .GetImage(SIZE { cx: 48, cy: 48 }, SIIGBF_BIGGERSIZEOK)
                .ok()
        })
        .and_then(|hbmp| {
            let d = bitmap_to_rgba(hbmp);
            let _ = DeleteObject(hbmp.into());
            d
        });

    Some((name, icon.unwrap_or_else(placeholder_icon)))
}

/// 出厂默认快捷方式（sir 2026-11-12）：控制面板 / 资源管理器 / 设置 三个，
/// 用户自行添加其余。控制面板走 control.exe（任意 Windows 机器都在），
/// 设置走 ms-settings: URI（图标由 shell 命名空间解析，本机验证可拿到 32×32）。
pub fn default_shortcuts() -> Vec<Shortcut> {
    let candidates: [(&str, &str); 3] = [
        ("控制面板", r"C:\Windows\System32\control.exe"),
        ("资源管理器", r"C:\Windows\explorer.exe"),
        ("设置", r"ms-settings:"),
    ];
    candidates
        .iter()
        .filter(|(_, p)| {
            // URI（ms-settings:）无对应文件但必然可启动，始终保留；其余检查存在性
            p.contains(':') && !Path::new(p).is_absolute() || Path::new(p).exists()
        })
        .map(|(name, p)| {
            let mut s = build_shortcut(p);
            s.name = name.to_string();
            s
        })
        .collect()
}

/// 提取图标 → RGBA（32x32）。.lnk 直接抽图标，失败再解析目标；
/// 再失败走 **shell 命名空间兜底**（v0.5.1，sir 反馈：此电脑/控制面板快捷方式
/// 加进来后无图标——它们的 lnk 目标是 CLSID（`::{…}`），PrivateExtractIconsW
/// 对非文件目标拿不到图标，Shell 的 IShellItemImageFactory 才认）。
pub fn extract_icon_rgba(path: &str) -> Option<IconData> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".lnk") {
        if let Some(d) = private_extract(path, 0) {
            return Some(d);
        }
        if let Some(target) = resolve_lnk_target(path) {
            if let Some(d) = private_extract(&target, 0) {
                return Some(d);
            }
        }
    } else if let Some(d) = private_extract(path, 0) {
        return Some(d);
    }
    shell_item_icon(path)
}

/// Shell 命名空间条目图标兜底：SHCreateItemFromParsingName + GetImage(ICONONLY)。
/// 对 lnk（shell 会解析到目标）与 `::{CLSID}` 直写路径都有效。COM 独立线程 + 3s 超时。
fn shell_item_icon(path: &str) -> Option<IconData> {
    let (tx, rx) = std::sync::mpsc::channel();
    let path = path.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(unsafe { com_shell_item_icon(&path) });
    });
    rx.recv_timeout(Duration::from_secs(3)).ok().flatten()
}

unsafe fn com_shell_item_icon(path: &str) -> Option<IconData> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::System::Com::CoInitializeEx;
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
    };
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let wide = to_wide(path);
    let item: windows::Win32::UI::Shell::IShellItem =
        SHCreateItemFromParsingName(PCWSTR::from_raw(wide.as_ptr()), None).ok()?;
    let factory = item.cast::<IShellItemImageFactory>().ok()?;
    let hbmp = factory
        .GetImage(
            SIZE { cx: 48, cy: 48 },
            SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
        )
        .ok()?;
    let d = bitmap_to_rgba(hbmp);
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(hbmp.into());
    d
}

/// PrivateExtractIconsW + GetDIBits：HICON → 32bpp BGRA → RGBA。
fn private_extract(file: &str, index: i32) -> Option<IconData> {
    unsafe {
        let mut wide = [0u16; 260];
        for (i, u) in file.encode_utf16().take(259).enumerate() {
            wide[i] = u;
        }
        let mut hicons: [HICON; 1] = [HICON::default()];
        let got = PrivateExtractIconsW(
            &wide,
            index,
            ICON_SIZE,
            ICON_SIZE,
            Some(&mut hicons),
            None,
            0,
        );
        if got == 0 {
            return None;
        }
        let mut info = ICONINFO::default();
        if GetIconInfo(hicons[0], &mut info).is_err() {
            let _ = DestroyIcon(hicons[0]);
            return None;
        }
        let out = bitmap_to_rgba(info.hbmColor);
        let _ = DeleteObject(info.hbmColor.into());
        let _ = DeleteObject(info.hbmMask.into());
        let _ = DestroyIcon(hicons[0]);
        out
    }
}

pub(crate) unsafe fn bitmap_to_rgba(
    hbm: windows::Win32::Graphics::Gdi::HBITMAP,
) -> Option<IconData> {
    let mut bm = BITMAP::default();
    if GetObjectW(
        hbm.into(),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut _),
    ) == 0
    {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight);
    if w <= 0 || h <= 0 {
        return None;
    }
    let hdc = CreateCompatibleDC(None);
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let ok = GetDIBits(
        hdc,
        hbm,
        0,
        h as u32,
        Some(buf.as_mut_ptr() as *mut _),
        &mut bi,
        DIB_RGB_COLORS,
    ) != 0;
    let _ = DeleteDC(hdc);
    if !ok {
        return None;
    }
    // BGRA → RGBA
    for px in buf.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Some(IconData {
        w: w as u32,
        h: h as u32,
        rgba: b64(&buf),
    })
}

/// 解析 .lnk 目标路径（COM 在独立 STA 线程上执行，3s 超时）。
pub(crate) fn resolve_lnk_target(lnk: &str) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let path = lnk.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(unsafe { com_resolve_target(&path) });
    });
    rx.recv_timeout(Duration::from_secs(3)).ok().flatten()
}

unsafe fn com_resolve_target(path: &str) -> Option<String> {
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let link: IShellLinkW = match CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) {
        Ok(l) => l,
        Err(_) => return None,
    };
    let pf: IPersistFile = match link.cast() {
        Ok(p) => p,
        Err(_) => return None,
    };
    let wide = to_wide(path);
    if pf.Load(PCWSTR::from_raw(wide.as_ptr()), STGM(0)).is_err() {
        return None;
    }
    // IShellLinkW::GetPath(&mut buf, &mut find_data, flags)；0x0001 = SLGP_RAWPATH
    let mut buf = [0u16; 260];
    let mut fd = WIN32_FIND_DATAW::default();
    if link.GetPath(&mut buf, &mut fd, 0x0001).is_err() {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let target = String::from_utf16_lossy(&buf[..end]);
    if target.is_empty() {
        None
    } else {
        Some(target)
    }
}

/// 启动快捷方式（.exe / .lnk 通用）。
pub fn launch(path: &str) -> Result<(), String> {
    // 005 A5 注入点：`GLASSSUITE_LAUNCH_STUB=slow:<ms>` → 不真的 ShellExecute，
    // 睡 <ms> 后返回 Ok（模拟慢启动），让「点击路径非阻塞 + 启动耗时」可机器验证。
    // 上限 10s 防误设死等；真实启动路径不受该变量影响。
    if let Ok(stub) = std::env::var("GLASSSUITE_LAUNCH_STUB") {
        if let Some(ms) = stub.trim().strip_prefix("slow:") {
            if let Ok(ms) = ms.trim().parse::<u64>() {
                std::thread::sleep(Duration::from_millis(ms.min(10_000)));
                return Ok(());
            }
        }
    }
    let wide = to_wide(path);
    let hinst = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR::from_raw(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    // 返回值 > 32 为成功
    if hinst.0 as usize > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW failed: {:?}", hinst.0 as usize))
    }
}

/// 点击图标：已运行则直接激活窗口（快路径），否则启动。
///
/// 背景：旧实现无条件 `ShellExecuteW`——每次点击都重新起一次进程，靠应用自身的
/// 单实例转发去恢复窗口，比系统任务栏（直接 `ShowWindow(SW_RESTORE)`）明显慢。
/// 这里复用运行指示已有的 `running::find_window_for_key`（同一套 key 体系），
/// 命中就直接还原并置前；未命中才回退 `launch`。
pub fn activate_or_launch(aumid: &Option<String>, path: &str) -> Result<(), String> {
    let key = crate::dock::running::key_for_shortcut(aumid, path);
    let found = crate::dock::running::find_window_for_key(&key);
    activate_or_launch_decide(found, path, launch)
}

/// 决策与执行分离（005 A1 注入点）：`found` 由调用方给（生产 = EnumWindows 匹配，
/// 测试 = 直接构造），启动动作用闭包注入（生产 = ShellExecuteW，测试 = 计数器）。
/// 已在运行（found=Some）→ 只激活、**不启新实例**。
fn activate_or_launch_decide(
    found: Option<(isize, bool)>,
    path: &str,
    do_launch: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    match found {
        Some((src, minimized)) => {
            activate_window(src, minimized);
            Ok(())
        }
        None => do_launch(path),
    }
}

/// 还原最小化 + 置前（activate_or_launch 专用；preview 行点击有自己的副本）。
fn activate_window(src: isize, minimized: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, ShowWindow, SwitchToThisWindow, SW_RESTORE,
    };
    let hwnd = HWND(src as *mut c_void);
    unsafe {
        if minimized {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        // 前台锁：用户刚点了 dock（本进程即前台），一次 SetForegroundWindow 通常够；
        // 被系统拒绝（返回 0）时退回 SwitchToThisWindow（无需 AttachThreadInput 的
        // 额外 feature，也不改动目标线程输入队列）。
        if !SetForegroundWindow(hwnd).as_bool() {
            SwitchToThisWindow(hwnd, true);
        }
    }
}

/// 文件对话框选程序。
pub fn pick_program() -> Option<String> {
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };

    let filter: Vec<u16> = "程序与快捷方式\0*.exe;*.lnk\0所有文件\0*.*\0"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let title: Vec<u16> = to_wide("选择要添加到图标栏的程序");
    let mut file = [0u16; 1024];
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: PCWSTR::from_raw(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(file.as_mut_ptr()),
        nMaxFile: 1024,
        lpstrTitle: PCWSTR::from_raw(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    unsafe {
        if GetOpenFileNameW(&mut ofn).as_bool() {
            let end = file.iter().position(|&c| c == 0).unwrap_or(0);
            Some(String::from_utf16_lossy(&file[..end]))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// dock 窗口运行时逻辑：定位 + 自动隐藏
// ---------------------------------------------------------------------------

/// dock 窗口逻辑线程：跟随任务栏定位、自动隐藏、全屏让位。
pub fn dock_logic(app: tauri::AppHandle, shutdown: std::sync::Arc<AtomicBool>) {
    let Some(dock) = app.get_webview_window("dock") else {
        eprintln!("dock window missing");
        return;
    };
    // 原生句柄：显示用 SW_SHOWNA（不抢焦点——sir 打字时划到底边不应被 dock 吃掉键盘输入）
    let hwnd = dock
        .hwnd()
        .ok()
        .map(|h| windows::Win32::Foundation::HWND(h.0));
    let cfg_path = config_path(&app);
    let auto_hide = cfg_path
        .as_deref()
        .map(load_config)
        .map(|c| c.auto_hide)
        .unwrap_or(true); // v0.2.y：默认自动隐藏（与 DockConfig::default 对齐）

    let mut visible = false;
    let mut last_active = Instant::now();
    let mut last_pos: Option<(i32, i32)> = None;
    let boot_at = Instant::now();
    // 首帧就绪时刻：None = 还没就绪（此时不显示 dock = 黑闪根因修复）
    let mut ready_at: Option<Instant> = None;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }

        // OC-20261115-002 D4：交互锁期间不做 set_position / ShowWindow / 自动隐藏。
        // 一次按压手势里窗口几何必须纹丝不动，否则 Chromium 不合成 click。
        // 超时由 interaction_locked() 就地强制释放，不会卡死定位循环。
        if interaction_locked() {
            std::thread::sleep(Duration::from_millis(POLL_MS));
            continue;
        }

        // 首帧就绪跟踪（前端上报 + 超时兜底）
        let ready = is_ready() || boot_at.elapsed() >= Duration::from_millis(FORCE_READY_AFTER_MS);
        if ready {
            if ready_at.is_none() {
                ready_at = Some(Instant::now());
            }
        } else {
            ready_at = None;
        }

        // v0.5：逻辑态每轮用真实可见态校准。`visible` 是逻辑态，而 ShowWindow 有各种
        // no-op/失败路径（SW_SHOWNA 对已可见窗口是 no-op，HIDE 失败不更新逻辑态）——
        // 漂移后唤出分支 `want_visible && !visible` 永远不触发。IsWindowVisible 是
        // 廉价查询，逐轮校准消灭这一整类漂移 bug 的根源（卡面 §1.1 修复方向 1）。
        if let Some(h) = hwnd {
            visible =
                unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(h) }.as_bool();
        }

        let tray = crate::dock::desktop::primary_taskbar_rect();
        let Some(tr) = tray else {
            if visible {
                let _ = dock.hide();
                visible = false;
            }
            std::thread::sleep(Duration::from_millis(POLL_MS));
            continue;
        };

        // 定位：水平居中 + 任务栏上方留 DOCK_MARGIN。
        //
        // 2026-11-16 修复（dock 整体不可见，OC-20261116-001）：旧代码 x 用
        // `primary_screen()` 的宽、y 用 `tr.top`，两者来源不同屏 —— 主屏任务栏被
        // 外部守卫 tb_guard.ps1 藏住时 `tr` 拿到的是**副屏**任务栏 →
        // x=(1920-1200)/2=360 落在主屏 x 带内、y=1598-114-6=1478 落在副屏 y 带内
        // → 窗口被摆到两块显示器之间的**无人区** → 完全不渲染。
        //
        // 现在 x 与 y 同源：都用主屏任务栏 `tr` 的左右边界。tr 本身就是主屏任务栏，
        // 它的 left/right 就是主屏的 x 范围，top 就是贴靠基准。
        let size = dock
            .outer_size()
            .unwrap_or(tauri::PhysicalSize::new(780, 76));
        let x = (tr.left + tr.right - size.width as i32) / 2;
        let y = tr.top - size.height as i32 - DOCK_MARGIN;
        if last_pos != Some((x, y)) {
            let _ = dock.set_position(PhysicalPosition::new(x, y));
            last_pos = Some((x, y));
        }

        let fullscreen = crate::dock::desktop::is_foreground_fullscreen();
        let maximized = crate::dock::desktop::is_foreground_maximized();
        let want_visible = if fullscreen || maximized {
            false
        } else if !auto_hide {
            true
        } else if crate::dock::menu::is_panel_visible(&app) {
            // v0.5.1（sir 反馈）：开始菜单面板开着 → dock 一直显示；
            // 面板关闭后走回下方光标逻辑 = 自动隐藏模式恢复（光标不在 dock 区
            // 则按 hide 延迟收回）。
            last_active = Instant::now();
            true
        } else {
            let (cx, cy) = cursor_pos();
            let near_edge = cy >= tr.bottom as i64 - REVEAL_EDGE_PX;
            let in_dock = dock_rect_contains(x, y, size, cx, cy);
            // 007 断桥修复：鼠标在多窗预览窗内视同在 dock 内——否则光标离开
            // dock 矩形进入预览窗的瞬间 want_visible 翻 false，dock 隐藏并
            // 连带 preview::hide()，预览窗永远点不到（2026-11-16 实测复现）。
            let in_preview = crate::dock::preview::current_rect()
                .map(|r| {
                    cx >= r.left as i64
                        && cx <= r.right as i64
                        && cy >= r.top as i64
                        && cy <= r.bottom as i64
                })
                .unwrap_or(false);
            if near_edge || in_dock || in_preview {
                last_active = Instant::now();
                true
            } else {
                visible && last_active.elapsed() < Duration::from_millis(HIDE_DELAY_MS)
            }
        };

        // 首帧闸门：WebView2 报就绪 + 额外等一拍，才允许显示（首帧黑闪修复）
        let show_ready = match ready_at {
            Some(t) => t.elapsed() >= Duration::from_millis(FIRST_SHOW_DELAY_MS),
            None => false,
        };

        if want_visible && !visible && show_ready {
            if let Some(h) = hwnd {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                        h,
                        windows::Win32::UI::WindowsAndMessaging::SW_SHOWNA,
                    );
                    // 不再 SetWindowPos(HWND_BOTTOM)：那会把 dock 压到桌面图标
                    // 视图（SHELLDLL_DefView）之下，真实鼠标点击被桌面层吃掉
                    // （2026-11-13 sir 反馈「图标点了没反应」，实锤命中测试：
                    // dock 可见态 WindowFromPoint(dock 中心) 返回的是桌面视图）。
                    // 「最大化窗口盖住 dock」改由 is_foreground_maximized 让位解决。
                }
            } else {
                let _ = dock.show();
            }
            visible = true;
        } else if !want_visible && visible {
            if let Some(h) = hwnd {
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                        h,
                        windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
                    );
                }
            } else {
                let _ = dock.hide();
            }
            // 任务 B：dock 隐藏（自动隐藏/全屏让位）时预览必须一起消失
            crate::dock::preview::hide();
            visible = false;
        }

        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

fn cursor_pos() -> (i64, i64) {
    let mut pt = windows::Win32::Foundation::POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_ok() {
        (pt.x as i64, pt.y as i64)
    } else {
        (-1, -1)
    }
}

fn dock_rect_contains(x: i32, y: i32, size: tauri::PhysicalSize<u32>, cx: i64, cy: i64) -> bool {
    let m = DOCK_HOVER_MARGIN;
    let (w, h) = (size.width as f64, size.height as f64);
    cx >= x as i64 - m as i64
        && cx <= (x as f64 + w + m) as i64
        && cy >= y as i64 - m as i64
        && cy <= (y as f64 + h + m) as i64
}

// ---------------------------------------------------------------------------
// v0.7 Phase 1（OC-20261102-002）：拖出成游离图标 / 拖回 dock
// ---------------------------------------------------------------------------

/// 拖出判定的位移阈值（CSS px，卡面 §1.3-4「24–32px」取上限 32）。
/// 验收 3 要求「dock 内位移 <24px 松开仍是行内排序」，验收 2 要求「≥32px 拖出」——
/// 32 同时满足两条，且离「误拖出」更远。
pub const DRAG_OUT_MIN_PX: f64 = 32.0;
/// 拖出判定的按住时长阈值（ms，卡面 §1.3-4「持续 ≥80ms」）。
pub const DRAG_OUT_MIN_MS: u64 = 80;

/// 拖出判定（纯函数——前端与探针共用同一套常量/口径）。
///
/// - `dx`/`dy`：相对按下点的位移（CSS px）
/// - `elapsed_ms`：按住时长
/// - `outside_row`：指针是否已越出 dock 行（药丸）矩形
///
/// 行内排序与拖出是**互斥**的：只要指针还在 dock 行内，就永远只是行内换位
/// （卡面 §1.4「行内排序已实现，不许推倒重来，只加『拖出边界后转游离』这一段」）。
pub fn drag_out_decision(dx: f64, dy: f64, elapsed_ms: u64, outside_row: bool) -> bool {
    if !outside_row {
        return false;
    }
    if elapsed_ms < DRAG_OUT_MIN_MS {
        return false;
    }
    let dist = (dx * dx + dy * dy).sqrt();
    dist >= DRAG_OUT_MIN_PX
}

/// 按 id 反查快捷方式（游离图标命令与探针共用）。
pub fn find_shortcut_by_id<'a>(cfg: &'a DockConfig, id: &str) -> Option<&'a Shortcut> {
    cfg.shortcuts.iter().find(|s| s.id == id)
}

/// 收回 dock 行：`slot` = 行内可见条目的插入序号（None = 末尾）。
///
/// dock 窗口客户区原点（物理像素）与缩放系数——CSS px ↔ 物理像素的唯一换算口径。
pub fn dock_metrics(app: &tauri::AppHandle) -> Option<(i32, i32, f64)> {
    let dock = app.get_webview_window("dock")?;
    let pos = dock.inner_position().ok()?;
    let scale = dock.scale_factor().unwrap_or(1.0);
    Some((pos.x, pos.y, scale))
}

/// dock 客户区 CSS 坐标 → 虚拟桌面物理像素坐标（副屏在左/上时结果为负）。
pub fn css_to_physical(app: &tauri::AppHandle, cx: f64, cy: f64) -> Option<(i32, i32)> {
    let (ox, oy, scale) = dock_metrics(app)?;
    Some((
        ox + (cx * scale).round() as i32,
        oy + (cy * scale).round() as i32,
    ))
}

// ---------------------------------------------------------------------------
// v0.8（OC-20261104-001）：dock 上的「未固定的运行中应用」临时图标。
//
// 数据流：running.rs 的 watcher 快照（复用，禁改 running.rs / 禁新轮询线程）−
// dock.json keys − menu.json keys → 临时条目（key + 显示名 + 可启动路径 + 图标）。
// 生命周期：应用退出 → 快照不再含 key → 图标消失；不持久化（重启后只恢复 dock.json）。
// ---------------------------------------------------------------------------

/// 临时图标上限（卡面 §1.3-7：防 WebView2/布局失控；超出的忽略并记日志）。
pub const TRANSIENT_MAX: usize = 10;

/// 临时运行条目（前端渲染 + 探针断言共用）。
#[derive(Serialize, Clone, Debug)]
pub struct TransientApp {
    /// 运行 key（`exe:<路径>` / `aumid:<小写 AUMID>`），与 running.rs 同一体系
    pub key: String,
    pub name: String,
    /// 可启动路径（pin 时写进 dock.json；单击激活时走 activate_or_launch）
    pub path: String,
    pub aumid: Option<String>,
    /// None = 图标尚未就绪（前端画占位块，与开始菜单同语义）
    pub icon: Option<IconData>,
}

/// 「已被固定语义占有」的 key 集合：dock.json 全部条目 + menu.json 收藏路径。
/// dock 条目走 `running::key_for_shortcut`（显式 aumid 字段优先）；
/// menu 收藏路径是 `shell:AppsFolder\<AUMID>` / .lnk / 真实路径——前缀形态
/// key_for_shortcut 会落成 `exe:shell:...`，与运行快照的 `aumid:` key 失配
/// （已收藏的 UWP 应用会错误地重复出现在临时图标里），这里按形态先解析。
pub fn owned_keys(dock_cfg: &DockConfig, menu_paths: &[String]) -> HashSet<String> {
    let mut set: HashSet<String> = dock_cfg
        .shortcuts
        .iter()
        .map(|s| crate::dock::running::key_for_shortcut(&s.aumid, &s.path))
        .collect();
    for p in menu_paths {
        let lower = p.trim().to_lowercase();
        if let Some(a) = lower.strip_prefix("shell:appsfolder\\") {
            set.insert(format!("aumid:{}", a));
        } else {
            set.insert(crate::dock::running::key_for_shortcut(&None, p));
        }
    }
    set
}

/// 纯函数：快照 − 占有集合 → 临时 key 列表（稳定顺序，单测/探针共用）。
pub fn transient_filter(snap: &HashSet<String>, owned: &HashSet<String>) -> Vec<String> {
    let mut keys: Vec<String> = snap.difference(owned).cloned().collect();
    keys.sort();
    keys
}

/// 纯函数：上限截断。返回 (保留的 key, 被丢弃的个数)。
pub fn cap_transients(keys: Vec<String>) -> (Vec<String>, usize) {
    if keys.len() <= TRANSIENT_MAX {
        return (keys, 0);
    }
    let dropped = keys.len() - TRANSIENT_MAX;
    (keys.into_iter().take(TRANSIENT_MAX).collect(), dropped)
}

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

/// 临时图标内存缓存（图标提取最贵 3s COM 超时；key 命中不重复提取）。
static TRANSIENT_ICONS: LazyLock<Mutex<HashMap<String, Option<IconData>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// exe 条目图标：dock.rs 三路提取（IShellItem GetImage → shell 命名空间 →
/// PrivateExtractIconsW）+ 内存缓存。禁止另写第四条提取路径（卡面硬约束 4）。
fn transient_icon_exe(path: &str) -> Option<IconData> {
    let key = format!("exe:{}", crate::dock::running::normalize_path(path));
    if let Ok(g) = TRANSIENT_ICONS.lock() {
        if let Some(hit) = g.get(&key) {
            return hit.clone();
        }
    }
    let icon = extract_icon_rgba(path);
    if let Ok(mut g) = TRANSIENT_ICONS.lock() {
        g.insert(key, icon.clone());
    }
    icon
}

/// AUMID 条目图标：先反查开始菜单目录缓存（menu.rs 提取管线，零重复提取），
/// 未命中走 dock.rs 的 AppsFolder IShellItem 提取（同三路口径），再入内存缓存。
fn transient_icon_aumid(aumid: &str) -> Option<IconData> {
    let key = format!("aumid:{}", aumid.to_lowercase());
    if let Ok(g) = TRANSIENT_ICONS.lock() {
        if let Some(hit) = g.get(&key) {
            return hit.clone();
        }
    }
    let icon = crate::dock::menu::catalog_lookup_aumid(aumid)
        .and_then(|h| h.icon)
        .or_else(|| aumid_fallback_icon(aumid));
    if let Ok(mut g) = TRANSIENT_ICONS.lock() {
        g.insert(key, icon.clone());
    }
    icon
}

/// AUMID 兜底提取：`shell:AppsFolder\<AUMID>` 的 IShellItemImageFactory
/// （apps_folder_info 的图标半边，独立 STA 线程 + 3s 超时）。
fn aumid_fallback_icon(aumid: &str) -> Option<IconData> {
    let (tx, rx) = std::sync::mpsc::channel();
    let aumid = aumid.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(unsafe { com_aumid_icon(&aumid) });
    });
    rx.recv_timeout(Duration::from_secs(3)).ok().flatten()
}

unsafe fn com_aumid_icon(aumid: &str) -> Option<IconData> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::System::Com::CoInitializeEx;
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
    };
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let parsing = format!("shell:AppsFolder\\{aumid}");
    let wide = to_wide(&parsing);
    let item: windows::Win32::UI::Shell::IShellItem =
        SHCreateItemFromParsingName(PCWSTR::from_raw(wide.as_ptr()), None).ok()?;
    let factory = item.cast::<IShellItemImageFactory>().ok()?;
    let hbmp = factory
        .GetImage(
            SIZE { cx: 48, cy: 48 },
            SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
        )
        .ok()?;
    let d = bitmap_to_rgba(hbmp);
    let _ = windows::Win32::Graphics::Gdi::DeleteObject(hbmp.into());
    d
}

/// 单个运行 key → 临时条目（显示名 / 可启动路径 / 图标）。
///
/// - `aumid:` 优先反查开始菜单目录（friendly name + 现成图标 + 解析路径）；
/// - `exe:` 用 exe 路径本身（三路提取图标）。
pub(crate) fn transient_entry(key: &str) -> TransientApp {
    if let Some(aumid) = key.strip_prefix("aumid:") {
        let hit = crate::dock::menu::catalog_lookup_aumid(aumid);
        let name = hit
            .as_ref()
            .map(|h| h.name.clone())
            .unwrap_or_else(|| fallback_name(aumid));
        // 目录里的解析路径保留原始大小写（启动/固定用它）；目录未命中才退小写 key
        let path = hit
            .as_ref()
            .map(|h| h.path.clone())
            .unwrap_or_else(|| format!("shell:AppsFolder\\{aumid}"));
        let icon = hit
            .as_ref()
            .and_then(|h| h.icon.clone())
            .or_else(|| transient_icon_aumid(aumid));
        return TransientApp {
            key: key.to_string(),
            name,
            path,
            aumid: Some(aumid.to_string()),
            icon,
        };
    }
    if let Some(path) = key.strip_prefix("exe:") {
        let name = Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
        let icon = crate::dock::menu::catalog_lookup_path(path)
            .and_then(|h| h.icon)
            .or_else(|| transient_icon_exe(path));
        return TransientApp {
            key: key.to_string(),
            name,
            path: path.to_string(),
            aumid: None,
            icon,
        };
    }
    TransientApp {
        key: key.to_string(),
        name: key.to_string(),
        path: String::new(),
        aumid: None,
        icon: None,
    }
}

// ---------------------------------------------------------------------------
// OC-20261115-003：差集取证 / 滞回去抖 / 归并去重 / 上限提示
// ---------------------------------------------------------------------------

/// 进入/退出都要连续几次采样才生效（2 次 × 500ms ≈ 1s，满足 §4-#5 的 ≤2s）。
pub const HYSTERESIS_SAMPLES: u8 = 2;

/// 滞回状态。`stable` 的元素是 `key|icon_ready` 指纹，图标就绪态抖动同样被去抖。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TransientHysteresis {
    pub stable: BTreeSet<String>,
    pub hits: BTreeMap<String, u8>,
    pub misses: BTreeMap<String, u8>,
}

impl TransientHysteresis {
    pub fn is_fresh(&self) -> bool {
        self.stable.is_empty() && self.hits.is_empty() && self.misses.is_empty()
    }
}

/// 纯函数：单次抖动不改变稳定集合；进入需连续 N 次命中，退出需连续 N 次缺失。
pub fn hysteresis_step(
    mut st: TransientHysteresis,
    observed: &BTreeSet<String>,
) -> TransientHysteresis {
    // 首帧直接定基：启动时不让所有已运行程序凭空消失 1s（不算「进入」）。
    if st.is_fresh() {
        st.stable = observed.clone();
        return st;
    }
    let mut hits = BTreeMap::new();
    for fp in observed {
        if st.stable.contains(fp) {
            continue;
        }
        let n = st.hits.get(fp).copied().unwrap_or(0) + 1;
        if n >= HYSTERESIS_SAMPLES {
            st.stable.insert(fp.clone());
        } else {
            hits.insert(fp.clone(), n);
        }
    }
    st.hits = hits;

    let mut misses = BTreeMap::new();
    for fp in st.stable.clone() {
        if observed.contains(&fp) {
            continue;
        }
        let n = st.misses.get(&fp).copied().unwrap_or(0) + 1;
        if n >= HYSTERESIS_SAMPLES {
            st.stable.remove(&fp);
        } else {
            misses.insert(fp, n);
        }
    }
    st.misses = misses;
    st
}

/// 稳定指纹 → 渲染用 key 列表（去掉 `|icon_ready` 后缀）。
pub fn stable_keys(st: &TransientHysteresis) -> Vec<String> {
    st.stable
        .iter()
        .map(|fp| {
            fp.split_once('|')
                .map_or(fp.clone(), |(k, _)| k.to_string())
        })
        .collect()
}

/// D4 纯函数：AUMID 优先、exe 路径兜底。同一应用若同时产出 `aumid:` 与
/// `exe:` 两个 key，只保留 AUMID 那条，避免临时区出现重复条目。
pub fn merge_alias_keys(keys: Vec<String>, aliases: &[(String, String)]) -> Vec<String> {
    let present: HashSet<&str> = keys.iter().map(String::as_str).collect();
    let mut shadowed: HashSet<&str> = HashSet::new();
    for (aumid_key, exe_key) in aliases {
        if present.contains(aumid_key.as_str()) {
            shadowed.insert(exe_key.as_str());
        }
    }
    keys.into_iter()
        .filter(|k| !shadowed.contains(k.as_str()))
        .collect()
}

static LAST_OBSERVED: LazyLock<Mutex<Option<BTreeSet<String>>>> =
    LazyLock::new(|| Mutex::new(None));
static TRANSIENT_HYST: LazyLock<Mutex<TransientHysteresis>> =
    LazyLock::new(|| Mutex::new(TransientHysteresis::default()));
/// 最近一次被上限挤出的条目数（供前端 notice，不再只 eprintln!）。
static TRANSIENT_DROPPED: AtomicUsize = AtomicUsize::new(0);

pub fn transient_dropped() -> usize {
    TRANSIENT_DROPPED.load(Ordering::Relaxed)
}

/// 临时条目全集（命令与探针共用）：快照 − dock.json − menu.json → 归并 → 截断 → 滞回。
/// `menu_paths` 由调用方给（app 进程走 menu::all_pinned_paths；探针直接读 menu.json）。
pub fn transient_apps(
    snap: &HashSet<String>,
    dock_cfg: &DockConfig,
    menu_paths: &[String],
) -> Vec<TransientApp> {
    // 目录懒启动：让 friendly name / 图标在首次查询后陆续可用（幂等，同面板线程）
    crate::dock::menu::ensure_catalog_started();
    let owned = owned_keys(dock_cfg, menu_paths);
    let filtered = transient_filter(snap, &owned);

    // D4：AUMID 与 exe 指向同一应用时只留 AUMID 一条。
    let aliases: Vec<(String, String)> = filtered
        .iter()
        .filter_map(|k| {
            let aumid = k.strip_prefix("aumid:")?;
            let hit = crate::dock::menu::catalog_lookup_aumid(aumid)?;
            Some((
                k.clone(),
                format!("exe:{}", crate::dock::running::normalize_path(&hit.path)),
            ))
        })
        .collect();
    let (keys, dropped) = cap_transients(merge_alias_keys(filtered, &aliases));
    TRANSIENT_DROPPED.store(dropped, Ordering::Relaxed);
    if dropped > 0 {
        // §5-4：不许只 eprintln!，用户要能看到提示（api.rs 会 emit 给前端 notice）。
        menu::debug_note(&format!(
            "dock: transient capped dropped={dropped} max={TRANSIENT_MAX}"
        ));
    }

    // 指纹 = key + 图标就绪态（D3：两者都参与，但单次抖动不改渲染）。
    let observed: BTreeSet<String> = keys
        .iter()
        .map(|k| {
            let ready = if transient_entry(k).icon.is_some() {
                1
            } else {
                0
            };
            format!("{k}|{ready}")
        })
        .collect();

    // D1 差集日志：只在**真的有变化**时打一行。静置零抖动 = 零日志，
    // 天然满足「验收后降为 debug 级、不许变成噪声源」。
    if let Ok(mut last) = LAST_OBSERVED.lock() {
        if let Some(prev) = last.as_ref() {
            let added: Vec<&String> = observed.difference(prev).collect();
            let removed: Vec<&String> = prev.difference(&observed).collect();
            if !added.is_empty() || !removed.is_empty() {
                menu::debug_note(&format!(
                    "dock: transient-diff added=[{}] removed=[{}]",
                    added
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                    removed
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            }
        }
        *last = Some(observed.clone());
    }

    // D3 滞回：渲染集合只看 stable，单次 ±1 抖动进不来。
    let st = if let Ok(mut g) = TRANSIENT_HYST.lock() {
        let next = hysteresis_step(std::mem::take(&mut *g), &observed);
        *g = next.clone();
        next
    } else {
        hysteresis_step(TransientHysteresis::default(), &observed)
    };
    stable_keys(&st)
        .iter()
        .map(|k| transient_entry(k))
        .collect()
}

/// 把临时条目固定进 dock.json（卡面 §1.3-3：atomic_write + bump 版本）。
/// 用 `dock::build_shortcut` 走完整解析（AppsFolder/别名/图标全复用），mode=Dock。
/// 返回新条目；key 已被占有（重复 pin / 非 transient key）返回 Err。
pub fn pin_transient_key(cfg_path: &Path, key: &str) -> Result<Shortcut, String> {
    let snap = crate::dock::running::scan_once();
    let cfg = load_config(cfg_path);
    let menu_paths = crate::dock::menu::all_pinned_paths();
    let owned = owned_keys(&cfg, &menu_paths);
    if owned.contains(key) {
        return Err(format!("key 已被固定，无需重复 pin: {key}"));
    }
    if !snap.contains(key) {
        return Err(format!("key 不在运行集合中（应用已退出？）: {key}"));
    }
    let entry = transient_entry(key);
    if entry.path.is_empty() {
        return Err(format!("临时条目无可启动路径: {key}"));
    }
    let sc = build_shortcut(&entry.path);
    if cfg
        .shortcuts
        .iter()
        .any(|s| normalize_path_key(&s.path) == normalize_path_key(&sc.path))
    {
        return Err(format!("dock.json 已有同路径条目: {}", sc.path));
    }
    let mut cfg2 = load_config(cfg_path);
    cfg2.shortcuts.push(sc.clone());
    save_config(cfg_path, &cfg2)?;
    Ok(sc)
}

/// 单击临时图标 → 激活该应用（复用 activate_or_launch 的 find_window_for_key 快路径；
/// 临时条目必然在运行，快路径命中，launch 只是类型完备的兜底）。
pub fn transient_activate(aumid: &Option<String>, path: &str) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, ShowWindow, SwitchToThisWindow, SW_RESTORE,
    };

    // OC-20261115-003 D4：同一应用多窗口已合并成一条临时图标；
    // 全部最小化 → 还原最前那一个，否则把最前的可见窗口置前。
    let key = crate::dock::running::key_for_shortcut(aumid, path);
    let wins = crate::dock::running::windows_for_key(&key);
    if !wins.is_empty() {
        let all_minimized = wins.iter().all(|(_, m)| *m);
        let target = if all_minimized {
            wins.first()
        } else {
            wins.iter().find(|(_, m)| !*m).or_else(|| wins.first())
        };
        if let Some((raw, _)) = target {
            let hwnd = HWND(*raw as *mut c_void);
            unsafe {
                if all_minimized {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                if !SetForegroundWindow(hwnd).as_bool() {
                    SwitchToThisWindow(hwnd, true);
                }
            }
            return Ok(());
        }
    }
    activate_or_launch(aumid, path)
}

// ---------------------------------------------------------------------------
// OC-20261115-002：dock 交互锁 + 按压手势口径
//
// 背景：transient 集合每 500ms 在 6↔7 之间翻转 → fit_dock 改窗口几何 →
// Chromium 的 pointer 序列被 resize 打断（mousedown 已派发、mouseup 落在旧坐标系，
// click 不合成）。交互锁让「一次按压手势」期间 Rust 侧完全不碰 dock 窗口几何，
// fit_dock 请求排队，松手（或 600ms 兜底）后再补执行。
// ---------------------------------------------------------------------------

/// 交互锁最长持有时间：前端漏发 end 时强制释放，防死锁。
pub const INTERACTION_TIMEOUT: Duration = Duration::from_millis(600);
/// 按压手势：位移上限（逻辑 px）、时长上限（ms）。
pub const PRESS_MAX_PX: f64 = 4.0;
pub const PRESS_MAX_MS: u64 = 500;
/// 真拖动的起拖阈值：超过它才获得 pointer capture。
pub const DRAG_START_PX: f64 = 6.0;

/// Some(开始时刻) = 交互进行中；超时在 `interaction_locked()` 里就地清掉。
static INTERACTION_LOCK: Mutex<Option<Instant>> = Mutex::new(None);
/// 锁期间最后一次 fit_dock 请求（只需最新一条，几何请求可合并）。
static PENDING_FIT: Mutex<Option<(f64, f64)>> = Mutex::new(None);

pub fn interaction_begin() {
    let mut slot = INTERACTION_LOCK.lock().unwrap();
    let renew = slot.is_some();
    *slot = Some(Instant::now());
    menu::debug_note(&format!("dock: interaction begin renew={renew}"));
}

pub fn interaction_end() {
    let mut slot = INTERACTION_LOCK.lock().unwrap();
    if slot.take().is_some() {
        menu::debug_note("dock: interaction end");
    }
}

/// 是否处于交互锁中；超过 INTERACTION_TIMEOUT 视为前端失联，强制释放。
pub fn interaction_locked() -> bool {
    let mut slot = INTERACTION_LOCK.lock().unwrap();
    match *slot {
        Some(start) if start.elapsed() < INTERACTION_TIMEOUT => true,
        Some(_) => {
            *slot = None;
            menu::debug_note("dock: interaction timeout force-released");
            false
        }
        None => false,
    }
}

pub fn queue_fit(w: f64, h: f64) {
    *PENDING_FIT.lock().unwrap() = Some((w, h));
}

pub fn take_pending_fit() -> Option<(f64, f64)> {
    PENDING_FIT.lock().unwrap().take()
}

/// 锁释放后补执行排队的 fit_dock（幂等：与超时兜底线程竞争 take，只有一个拿到）。
pub fn flush_pending_fit(app: &tauri::AppHandle) {
    let Some((w, h)) = take_pending_fit() else {
        return;
    };
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = handle.get_webview_window("dock") {
            let _ = crate::dock::api::apply_fit_dock(&win, w, h);
        }
    });
    menu::debug_note(&format!("dock: fit_dock flushed w={w} h={h}"));
}

/// 一次按压的采样点（Rust 侧口径权威定义；前端 `bindPress` 同构实现）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PressSample {
    pub pointer_id: i64,
    pub button: i16,
    pub x: f64,
    pub y: f64,
    pub elapsed_ms: u64,
}

/// 手势是否构成一次「按压」：pointerId 一致 + 同一按键 + 位移 < 4px + 时长 < 500ms。
/// 纯函数，单测覆盖；**不依赖浏览器合成 click**。
pub fn press_hit(start: PressSample, end: PressSample) -> bool {
    start.pointer_id == end.pointer_id
        && start.button == end.button
        && (end.x - start.x).hypot(end.y - start.y) < PRESS_MAX_PX
        && end.elapsed_ms.saturating_sub(start.elapsed_ms) < PRESS_MAX_MS
}

#[cfg(test)]
mod transient_tests {
    use super::*;

    fn set(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// 验收 2：key 过滤纯函数——排除固定（dock.json）条目。
    #[test]
    fn transient_filter_excludes_dock_keys() {
        let snap = set(&[
            "exe:c:\\a\\app1.exe",
            "exe:c:\\b\\pinned.exe",
            "aumid:x.y!app",
        ]);
        let dock = set(&["exe:c:\\b\\pinned.exe"]);
        let got = transient_filter(&snap, &dock);
        assert_eq!(
            got,
            vec![
                "aumid:x.y!app".to_string(),
                "exe:c:\\a\\app1.exe".to_string()
            ]
        );
    }

    /// 验收 2：key 过滤纯函数——排除收藏（menu.json）条目。
    #[test]
    fn transient_filter_excludes_menu_keys() {
        let snap = set(&["aumid:m.calc!app", "aumid:m.pinned!app"]);
        let owned = set(&["aumid:m.pinned!app"]);
        let got = transient_filter(&snap, &owned);
        assert_eq!(got, vec!["aumid:m.calc!app".to_string()]);
    }

    /// owned_keys：dock + menu 两条来源都进集合（key_for_shortcut 同口径）。
    #[test]
    fn owned_keys_merges_dock_and_menu() {
        let cfg = DockConfig {
            shortcuts: vec![Shortcut {
                id: "t".into(),
                name: "n".into(),
                path: "C:\\X\\App.exe".into(),
                icon: placeholder_icon(),
                aumid: None,
                mode: IconMode::Dock,
                free_rect: None,
            }],
            ..Default::default()
        };
        let owned = owned_keys(&cfg, &["shell:AppsFolder\\M.Calc!App".to_string()]);
        assert!(
            owned.contains("exe:c:\\x\\app.exe"),
            "dock key 归一化后进集合"
        );
        assert!(owned.contains("aumid:m.calc!app"), "menu key 小写后进集合");
    }

    /// 验收 7：10 上限——超出部分截断并报告个数（注入列表单测，不真开 11 个应用）。
    #[test]
    fn cap_transients_truncates_at_max() {
        let keys: Vec<String> = (0..12).map(|i| format!("exe:c:\\a\\app{i}.exe")).collect();
        let (kept, dropped) = cap_transients(keys);
        assert_eq!(kept.len(), TRANSIENT_MAX);
        assert_eq!(dropped, 2);
        // 未超上限时零丢弃
        let (kept, dropped) = cap_transients(vec!["a".into()]);
        assert_eq!((kept.len(), dropped), (1, 0));
    }

    /// sir 2026-11-12：默认快捷方式 = 控制面板 / 资源管理器 / 设置 三个
    ///（用户自行添加其余）。锁死顺序与数量，防漂移。
    #[test]
    fn default_shortcuts_are_exactly_three() {
        let s = default_shortcuts();
        let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["控制面板", "资源管理器", "设置"],
            "默认快捷方式应为 3 个（名称顺序）：{:?}",
            names
        );
        // 每个都必须有图标（容许 placeholder，但结构必须完整）
        for x in &s {
            assert!(x.icon.w > 0 && x.icon.h > 0, "{} 缺图标", x.name);
        }
    }

    // ---- OC-20261115-002 §4-1：手势判定纯函数 ----
    fn press_start() -> PressSample {
        PressSample {
            pointer_id: 7,
            button: 0,
            x: 100.0,
            y: 40.0,
            elapsed_ms: 1_000,
        }
    }

    #[test]
    fn press_hit_accepts_clean_tap() {
        let end = PressSample {
            x: 102.0,
            y: 41.0,
            elapsed_ms: 1_120,
            ..press_start()
        };
        assert!(press_hit(press_start(), end));
    }

    #[test]
    fn press_hit_rejects_drag_distance() {
        let end = PressSample {
            x: 112.0,
            elapsed_ms: 1_120,
            ..press_start()
        };
        assert!(!press_hit(press_start(), end));
    }

    #[test]
    fn press_hit_rejects_long_press() {
        let end = PressSample {
            elapsed_ms: 1_600,
            ..press_start()
        };
        assert!(!press_hit(press_start(), end));
    }

    #[test]
    fn press_hit_rejects_pointer_id_mismatch() {
        let end = PressSample {
            pointer_id: 8,
            elapsed_ms: 1_120,
            ..press_start()
        };
        assert!(!press_hit(press_start(), end));
    }

    #[test]
    fn press_hit_rejects_button_change() {
        let end = PressSample {
            button: 2,
            elapsed_ms: 1_120,
            ..press_start()
        };
        assert!(!press_hit(press_start(), end));
        // 右键本身也能构成一次有效按压
        let right = PressSample {
            button: 2,
            ..press_start()
        };
        let right_end = PressSample {
            elapsed_ms: 1_090,
            ..right
        };
        assert!(press_hit(right, right_end));
    }

    #[test]
    fn interaction_lock_pair_and_timeout_are_bounded() {
        // 只在这一条测试里碰全局锁，避免与其他并行测试互相干扰。
        *INTERACTION_LOCK.lock().unwrap() = None;
        assert!(!interaction_locked());
        interaction_begin();
        assert!(interaction_locked());
        interaction_end();
        assert!(!interaction_locked());

        // 前端漏发 end：把开始时刻拨到超时之前，下一次查询就地强制释放。
        *INTERACTION_LOCK.lock().unwrap() =
            Some(Instant::now() - INTERACTION_TIMEOUT - Duration::from_millis(10));
        assert!(!interaction_locked(), "超时必须强制释放");
        assert!(INTERACTION_LOCK.lock().unwrap().is_none());
        assert!(INTERACTION_TIMEOUT.as_millis() <= 600, "兜底不得超过 600ms");
    }

    // ---- OC-20261115-003 §4-1：滞回去抖 / 归并去重纯函数 ----
    fn fp_set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn hysteresis_single_flap_never_changes_rendered_set() {
        let base = fp_set(&["a|1", "b|1"]);
        let st = hysteresis_step(TransientHysteresis::default(), &base);
        assert_eq!(st.stable, base, "首帧直接定基");

        // c 只出现一次：不得进入稳定集合
        let flap = fp_set(&["a|1", "b|1", "c|1"]);
        let st = hysteresis_step(st, &flap);
        assert_eq!(st.stable, base);
        assert_eq!(st.hits.get("c|1"), Some(&1));

        // 回到只有 a：c 的命中计数被清零，b 只缺失一次也不得退出
        let back = fp_set(&["a|1"]);
        let st = hysteresis_step(st, &back);
        assert_eq!(st.stable, base);
        assert!(!st.hits.contains_key("c|1"), "单次命中的候选应被清空");
        assert_eq!(st.misses.get("b|1"), Some(&1));
    }

    #[test]
    fn hysteresis_enter_and_exit_need_two_consecutive_samples() {
        let base = fp_set(&["a|1"]);
        let mut st = hysteresis_step(TransientHysteresis::default(), &base);
        let with_b = fp_set(&["a|1", "b|1"]);
        st = hysteresis_step(st, &with_b);
        assert!(!st.stable.contains("b|1"), "第一次命中不进入");
        st = hysteresis_step(st, &with_b);
        assert!(st.stable.contains("b|1"), "连续第二次命中才进入");

        let only_b = fp_set(&["b|1"]);
        st = hysteresis_step(st, &only_b);
        assert!(st.stable.contains("a|1"), "第一次缺失不退出");
        st = hysteresis_step(st, &only_b);
        assert!(!st.stable.contains("a|1"), "连续第二次缺失才退出");
    }

    #[test]
    fn hysteresis_damps_icon_ready_flapping() {
        let with_icon = fp_set(&["a|1"]);
        let st = hysteresis_step(TransientHysteresis::default(), &with_icon);
        // 图标就绪态在 None↔Some 之间抖一次：渲染集合不变
        let no_icon = fp_set(&["a|0"]);
        let st = hysteresis_step(st, &no_icon);
        assert_eq!(stable_keys(&st), vec!["a".to_string()]);
        assert!(st.stable.contains("a|1"));
        // 连续两次才真正换指纹
        let st = hysteresis_step(st, &no_icon);
        assert!(st.stable.contains("a|0"));
        assert!(!st.stable.contains("a|1"));
        assert_eq!(stable_keys(&st), vec!["a".to_string()], "key 仍只有一条");
    }

    #[test]
    fn merge_alias_keys_prefers_aumid_over_exe() {
        let keys = vec![
            "aumid:m.calc!app".to_string(),
            "exe:c:\\program files\\calc.exe".to_string(),
            "exe:d:\\other\\app.exe".to_string(),
        ];
        let aliases = vec![(
            "aumid:m.calc!app".to_string(),
            "exe:c:\\program files\\calc.exe".to_string(),
        )];
        let got = merge_alias_keys(keys, &aliases);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&"aumid:m.calc!app".to_string()));
        assert!(!got.iter().any(|k| k.contains("calc.exe")));
        // 没有对应 AUMID 时 exe 条目原样保留
        let plain = merge_alias_keys(vec!["exe:d:\\other\\app.exe".to_string()], &aliases);
        assert_eq!(plain.len(), 1);
    }
}

#[cfg(test)]
mod uwp_merge_tests {
    use super::*;

    /// 006 ③：UWP 归并执行层——同一应用同时出现 AUMID key（宿主解析出的真实
    /// 身份）与 exe key（ApplicationFrameHost.exe 宿主路径）时，exe 被 AUMID
    /// 吸收；AUMID 不在场时不归并。
    /// （宿主→AUMID 的"发现"层需要真实 UWP 窗口，单测覆盖归并执行层。）
    #[test]
    fn uwp_alias_merge_drops_shadowed_exe_key() {
        let keys = vec![
            "aumid:app.x".to_string(),
            "exe:c:\\app.exe".to_string(),
            "exe:c:\\other.exe".to_string(),
        ];
        let aliases = vec![("aumid:app.x".to_string(), "exe:c:\\app.exe".to_string())];
        let merged = merge_alias_keys(keys, &aliases);
        assert_eq!(
            merged,
            vec!["aumid:app.x".to_string(), "exe:c:\\other.exe".to_string()],
            "宿主 exe key 被 AUMID 归并吸收"
        );

        let merged2 = merge_alias_keys(vec!["exe:c:\\app.exe".to_string()], &aliases);
        assert_eq!(
            merged2,
            vec!["exe:c:\\app.exe".to_string()],
            "AUMID 不在场 → 不归并"
        );
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    /// 008 步骤 4：旧 dock.json 里 mode:"free" 的条目在 load_config 时**原位**
    /// 收敛回 dock 行（free_rect 剥离，配置相对顺序不变）；纯 dock 文件零改动。
    #[test]
    fn load_config_migrates_free_entries_back_to_dock() {
        let dir = std::env::temp_dir().join("gs-dock-migration");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("dock.json");
        std::fs::write(
            &file,
            r#"{
                "auto_hide": false,
                "shortcuts": [
                    {"id":"a","name":"A","path":"C:\\a.exe","icon":{"w":1,"h":1,"rgba":""}},
                    {"id":"f1","name":"F1","path":"C:\\f1.exe","icon":{"w":1,"h":1,"rgba":""},
                     "mode":"free","free_rect":{"x":10,"y":20,"w":64,"h":64}},
                    {"id":"b","name":"B","path":"C:\\b.exe","icon":{"w":1,"h":1,"rgba":""}}
                ]
            }"#,
        )
        .unwrap();

        let cfg = load_config(&file);
        assert!(
            cfg.shortcuts.iter().all(|s| s.mode == IconMode::Dock),
            "迁移后不允许再有 free 条目"
        );
        assert_eq!(
            cfg.shortcuts
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "f1", "b"],
            "free 条目原位收敛（配置相对顺序不变，视觉扰动最小）"
        );
        assert!(cfg.shortcuts.iter().all(|s| s.free_rect.is_none()));

        // 纯 dock 文件：零改动往返
        let file2 = dir.join("dock2.json");
        std::fs::write(
            &file2,
            r#"{"auto_hide":false,"shortcuts":[{"id":"a","name":"A","path":"C:\\a.exe","icon":{"w":1,"h":1,"rgba":""}}]}"#,
        )
        .unwrap();
        let cfg2 = load_config(&file2);
        assert_eq!(cfg2.shortcuts.len(), 1);
        assert_eq!(cfg2.shortcuts[0].id, "a");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod config_resilience_tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// dock.json 损坏 + .bak 可用 → 恢复 .bak 内容，原文件被改名保留，主文件已恢复。
    #[test]
    fn corrupt_main_recovers_from_bak() {
        let dir = temp_dir("gs-dock-resilience-bak");
        let file = dir.join("dock.json");
        std::fs::write(&file, b"{ this is not valid json }").unwrap();
        std::fs::write(
            dir.join("dock.json.bak"),
            r#"{"auto_hide":false,"shortcuts":[{"id":"a","name":"FromBak","path":"C:\\a.exe","icon":{"w":1,"h":1,"rgba":""}}]}"#,
        )
        .unwrap();

        let cfg = load_config(&file);
        assert_eq!(cfg.shortcuts.len(), 1, "应恢复 .bak 的配置");
        assert_eq!(cfg.shortcuts[0].name, "FromBak");

        // 主文件已恢复：再次读盘必须是可解析的 .bak 内容
        let restored = std::fs::read(&file).unwrap();
        let parsed: DockConfig = serde_json::from_slice(&restored).unwrap();
        assert_eq!(parsed.shortcuts.len(), 1);
        assert_eq!(parsed.shortcuts[0].name, "FromBak");

        // 原坏文件被保留（不抹用户数据）
        let corrupt = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .find(|n| n.starts_with("dock.corrupt-"));
        assert!(corrupt.is_some(), "损坏文件应以 dock.corrupt-* 保留");
    }

    /// dock.json 损坏且无 .bak → 回退默认，但原文件不被覆盖、被改名保留。
    #[test]
    fn corrupt_main_without_bak_falls_back_to_default_but_preserves_file() {
        let dir = temp_dir("gs-dock-resilience-default");
        let file = dir.join("dock.json");
        std::fs::write(&file, b"%%% not json %%%").unwrap();

        let cfg = load_config(&file);
        assert!(cfg.shortcuts.is_empty(), "默认配置出厂值无固定图标");

        let corrupt = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .find(|n| n.starts_with("dock.corrupt-"));
        assert!(corrupt.is_some(), "原文必须保留为 dock.corrupt-*.json");
        let bak_exists = file.with_file_name("dock.json.bak").exists();
        assert!(!bak_exists, "没有 .bak，不应凭空生成");
    }

    /// save_config 落盘前先快照当前可解析版本为 .bak。
    #[test]
    fn save_config_writes_bak_snapshot_first() {
        let dir = temp_dir("gs-dock-resilience-savebak");
        let file = dir.join("dock.json");
        // 先写一份合法配置，模拟"上次的好配置"
        std::fs::write(
            &file,
            r#"{"auto_hide":false,"shortcuts":[{"id":"x","name":"Good","path":"C:\\good.exe","icon":{"w":1,"h":1,"rgba":""}}]}"#,
        )
        .unwrap();

        let mut cfg = DockConfig::default();
        cfg.shortcuts.push(Shortcut {
            id: "y".to_string(),
            name: "New".to_string(),
            path: "C:\\new.exe".to_string(),
            icon: IconData { w: 1, h: 1, rgba: String::new() },
            aumid: None,
            mode: IconMode::Dock,
            free_rect: None,
        });
        save_config(&file, &cfg).unwrap();

        let bak = std::fs::read(dir.join("dock.json.bak")).unwrap();
        let bak_cfg: DockConfig = serde_json::from_slice(&bak).unwrap();
        assert_eq!(bak_cfg.shortcuts[0].name, "Good", ".bak 必须是写前的好配置");
        let cur = std::fs::read(&file).unwrap();
        let cur_cfg: DockConfig = serde_json::from_slice(&cur).unwrap();
        assert_eq!(cur_cfg.shortcuts[0].name, "New", "主文件写入了新配置");
    }

    /// save_config 拒绝覆盖无法解析的 dock.json（最后闸门）。
    #[test]
    fn save_config_refuses_to_overwrite_unparseable() {
        let dir = temp_dir("gs-dock-resilience-refuse");
        let file = dir.join("dock.json");
        let bad_bytes = b"this is not json at all";
        std::fs::write(&file, bad_bytes).unwrap();

        let result = save_config(&file, &DockConfig::default());
        assert!(result.is_err(), "必须拒绝覆盖坏文件");
        let after = std::fs::read(&file).unwrap();
        assert_eq!(after, bad_bytes, "坏文件字节必须原样保留");
    }
}
