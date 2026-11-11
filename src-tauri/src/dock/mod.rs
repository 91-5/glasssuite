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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
pub mod free;
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

#[derive(Serialize, Deserialize, Clone)]
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

pub fn load_config(path: &Path) -> DockConfig {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_config(path: &Path, cfg: &DockConfig) -> Result<(), String> {
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

/// 出厂默认快捷方式：系统里真实存在的常见程序。
pub fn default_shortcuts() -> Vec<Shortcut> {
    let candidates: [(&str, &str); 4] = [
        ("文件资源管理器", r"C:\Windows\explorer.exe"),
        ("记事本", r"C:\Windows\System32\notepad.exe"),
        ("命令提示符", r"C:\Windows\System32\cmd.exe"),
        (
            "Edge",
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        ),
    ];
    candidates
        .iter()
        .filter(|(_, p)| Path::new(p).exists())
        .map(|(_, p)| build_shortcut(p))
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
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, ShowWindow, SwitchToThisWindow, SW_RESTORE,
    };

    let key = crate::dock::running::key_for_shortcut(aumid, path);
    if let Some((src, minimized)) = crate::dock::running::find_window_for_key(&key) {
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
        return Ok(());
    }
    launch(path)
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

        // 定位：主屏水平居中，任务栏上方留 DOCK_MARGIN
        let size = dock
            .outer_size()
            .unwrap_or(tauri::PhysicalSize::new(780, 76));
        let (sw, _sh) = crate::dock::desktop::primary_screen();
        let x = (sw - size.width as i32) / 2;
        let y = tr.top - size.height as i32 - DOCK_MARGIN;
        if last_pos != Some((x, y)) {
            let _ = dock.set_position(PhysicalPosition::new(x, y));
            last_pos = Some((x, y));
        }

        let fullscreen = crate::dock::desktop::is_foreground_fullscreen();
        let want_visible = if fullscreen {
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
            if near_edge || in_dock {
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
                    // v0.7.2（sir 反馈「dock 一直在最上面」）：SW_SHOWNA 把 dock 提到
                    // 同层窗口之上，观感=置顶。D7 语义是桌面层（可被窗口盖住），显示后
                    // 立刻压回同层底部（HWND_BOTTOM）——已开窗口盖得住 dock。
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                        h,
                        Some(windows::Win32::UI::WindowsAndMessaging::HWND_BOTTOM),
                        0,
                        0,
                        0,
                        0,
                        windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                            | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                            | windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
                    );
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

/// 切成游离模式并记下矩形（物理像素虚拟桌面坐标）。
pub fn set_free_mode(cfg: &mut DockConfig, id: &str, rect: FreeRect) -> bool {
    match cfg.shortcuts.iter_mut().find(|s| s.id == id) {
        Some(s) => {
            s.mode = IconMode::Free;
            s.free_rect = Some(rect);
            true
        }
        None => false,
    }
}

/// 收回 dock 行：`slot` = 行内可见条目的插入序号（None = 末尾）。
///
/// 游离条目在数组里占位但不占行内位置，所以插入点按**dock 模式条目的下标**算。
pub fn set_dock_mode(cfg: &mut DockConfig, id: &str, slot: Option<usize>) -> bool {
    let Some(idx) = cfg.shortcuts.iter().position(|s| s.id == id) else {
        return false;
    };
    let mut sc = cfg.shortcuts.remove(idx);
    sc.mode = IconMode::Dock;
    sc.free_rect = None;
    let dock_idxs: Vec<usize> = cfg
        .shortcuts
        .iter()
        .enumerate()
        .filter(|(_, s)| s.mode == IconMode::Dock)
        .map(|(i, _)| i)
        .collect();
    let insert = match slot {
        Some(s) => dock_idxs.get(s).copied().unwrap_or(cfg.shortcuts.len()),
        None => cfg.shortcuts.len(),
    };
    cfg.shortcuts.insert(insert.min(cfg.shortcuts.len()), sc);
    true
}

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
#[derive(Serialize, Clone)]
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

use std::collections::{HashMap, HashSet};
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
fn transient_entry(key: &str) -> TransientApp {
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

/// 临时条目全集（命令与探针共用）：快照 − dock.json − menu.json，截断到上限。
/// `menu_paths` 由调用方给（app 进程走 menu::all_pinned_paths；探针直接读 menu.json）。
pub fn transient_apps(
    snap: &HashSet<String>,
    dock_cfg: &DockConfig,
    menu_paths: &[String],
) -> Vec<TransientApp> {
    // 目录懒启动：让 friendly name / 图标在首次查询后陆续可用（幂等，同面板线程）
    crate::dock::menu::ensure_catalog_started();
    let owned = owned_keys(dock_cfg, menu_paths);
    let (keys, dropped) = cap_transients(transient_filter(snap, &owned));
    if dropped > 0 {
        eprintln!("transient icons over cap: {dropped} hidden (max {TRANSIENT_MAX})");
    }
    keys.iter().map(|k| transient_entry(k)).collect()
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
    activate_or_launch(aumid, path)
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
}
