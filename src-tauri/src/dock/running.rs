//! 运行状态检测（OC-20261030-003 任务 A）。
//!
//! 维护「运行中应用 key」集合，key 两种形态：
//!   - `exe:<规范化小写路径>` —— 普通 Win32 应用（多进程应用所有窗口同路径，
//!     天然收敛为一条，不重复）
//!   - `aumid:<小写 AUMID>` —— UWP/打包应用。宿主是 ApplicationFrameHost.exe，
//!     进程路径是**宿主**的，必须用 AUMID 才能区分具体应用。
//!
//! 判定规则：EnumWindows 收集所有 IsWindowVisible 的顶层窗口（最小化窗口
//! IsWindowVisible 仍为 true，照常算「运行中」，与任务栏语义一致；本进程、
//! shell 桌面/任务栏类、WS_EX_TOOLWINDOW 小面板跳过）。每个窗口产出 0~1 个 key。
//!
//! UWP 取 AUMID 的路径（卡面 Review Q1）：ApplicationFrameWindow 的子窗口
//! `Windows.UI.Core.CoreWindow` 属于**应用自己的进程**（不是宿主），对该进程调
//! `GetApplicationUserModelId`；普通打包应用（Centennial，窗口属于真实 exe）直接
//! 对窗口所属进程调，成功即用 AUMID；都失败退回 exe 路径。
//! 注：卡面写的 `GetApplicationUserModelId(hwnd, ...)` 签名有误——该 API 收的是
//! **进程句柄**（kernel32/appmodel.h），windows 0.62 位于 `Win32::Packaging::Appx`。
//!
//! 刷新策略（卡面推荐）：`SetWinEventHook(WINEVENT_OUTOFCONTEXT)` 事件置脏 +
//! 即时重扫（150ms 节流）+ 500ms 兜底轮询。out-of-context 钩子由系统经本线程
//! 消息循环投递回调，**不注入任何目标进程**（卡面硬约束 2）。

use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, HANDLE, HWND, LPARAM};
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EnumWindows, FindWindowExW, GetClassNameW, GetWindowLongPtrW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, PeekMessageW, TranslateMessage,
    EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, GWL_EXSTYLE, MSG, PM_REMOVE, WINDOW_LONG_PTR_INDEX,
    WINEVENT_OUTOFCONTEXT, WM_QUIT,
};

const WS_EX_TOOLWINDOW: isize = 0x80;
const POLL_INTERVAL_MS: u64 = 500;
const RESCAN_THROTTLE_MS: u64 = 150;

/// 当前运行中的 key 集合（watcher 线程写入，前端命令读取）。
static RUNNING: std::sync::LazyLock<RwLock<HashSet<String>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashSet::new()));

/// WinEventHook 事件置脏标记。
static DIRTY: AtomicBool = AtomicBool::new(true);

/// 学到的「UWP frame 窗口 → AUMID」。UWP 挂起后子 CoreWindow 被销毁、AUMID 取不到，
/// 但 frame 窗口还在——沿用学习值，窗口销毁（下次扫描枚举不到）即清除。
/// 这保证后台挂起的 UWP 应用运行条不消失（与任务栏语义一致）。
static LEARNED_AUMIDS: LazyLock<Mutex<std::collections::HashMap<isize, String>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// 启动 watcher 线程（进程生命周期 = 线程生命周期，无需退出协议）。
pub fn spawn_watcher() {
    let _ = std::thread::Builder::new()
        .name("glassdesk-running".into())
        .spawn(|| unsafe { watcher() });
}

/// 当前运行中 key 集合快照。
pub fn snapshot() -> HashSet<String> {
    RUNNING.read().map(|s| s.clone()).unwrap_or_default()
}

/// 快捷方式对应的运行 key。
/// 匹配优先级：显式 `aumid` → **应用执行别名解析**（`system32\notepad.exe` 这类
/// 0 字节 reparse point，v0.3 之前的旧配置靠它自动补齐）→ exe 路径。
/// .lnk 目标解析（进程内缓存）：每条 .lnk 只解析一次，避免每次匹配都起 COM 线程 + 3s 超时。
/// 缓存无失效——.lnk 目标在运行期视为稳定；用户改快捷方式后重启 glassdesk 生效。
pub(crate) fn cached_lnk_target(lnk: &str) -> Option<String> {
    static LNK_CACHE: LazyLock<Mutex<HashMap<String, Option<String>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let cache = &*LNK_CACHE;
    if let Ok(map) = cache.lock() {
        if let Some(hit) = map.get(lnk) {
            return hit.clone();
        }
    }
    let resolved = crate::dock::resolve_lnk_target(lnk);
    if let Ok(mut map) = cache.lock() {
        map.insert(lnk.to_string(), resolved.clone());
    }
    resolved
}

/// 快捷方式 → 运行态 key。.lnk 先解析目标再规范化（评审 C1：.lnk 短路径与运行
/// exe 路径恒不等，不解析则结构性 false-negative）。
pub fn key_for_shortcut(aumid: &Option<String>, path: &str) -> String {
    if let Some(a) = aumid.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return format!("aumid:{}", a.to_lowercase());
    }
    if let Some(a) = alias_aumid(path) {
        return format!("aumid:{}", a.to_lowercase());
    }
    let mut owned: String = path.trim().to_string();
    if owned.to_lowercase().ends_with(".lnk") {
        if let Some(target) = cached_lnk_target(&owned) {
            owned = target;
        }
    }
    format!("exe:{}", normalize_path(&owned))
}

/// 快捷方式是否在运行。
pub fn is_running(snap: &HashSet<String>, aumid: &Option<String>, path: &str) -> bool {
    snap.contains(&key_for_shortcut(aumid, path))
}

/// 路径规范化（匹配两侧同规）：小写、`/`→`\`、去 `\\?\` 前缀、展开 8.3 短名与符号链接
/// （文件存在时 canonicalize / GetLongPathNameW）。
pub fn normalize_path(p: &str) -> String {
    let mut s = p.trim().to_lowercase().replace('/', "\\");
    if let Some(stripped) = s.strip_prefix("\\\\?\\") {
        s = stripped.to_string();
    }
    if Path::new(&s).exists() {
        if let Ok(real) = std::fs::canonicalize(&s) {
            let mut r = real.to_string_lossy().to_lowercase();
            if let Some(stripped) = r.strip_prefix("\\\\?\\") {
                r = stripped.to_string();
            }
            return r;
        }
        let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        let mut buf = [0u16; 1024];
        // 安全性：GetLongPathNameW 只读输入缓冲、写入定长输出缓冲
        let n = unsafe {
            windows::Win32::Storage::FileSystem::GetLongPathNameW(
                PCWSTR::from_raw(wide.as_ptr()),
                Some(&mut buf),
            )
        };
        if n > 0 && (n as usize) < buf.len() {
            return String::from_utf16_lossy(&buf[..n as usize]).to_lowercase();
        }
    }
    s
}

/// 「应用执行别名」（app execution alias）解析：`C:\Windows\System32\notepad.exe`
/// 这类 0 字节 reparse point（IO_REPARSE_TAG_APPEXECLINK）里带着打包应用的 AUMID。
/// payload 是 NUL 分隔的 UTF-16 串序列（包名/入口点/可执行路径/AUMID 等，顺序不假设），
/// 按「含 ! 且不含路径分隔符」的形状识别 AUMID，解析失败无害（只是没有 aumid key）。
/// 结果按路径缓存（别名文件内容基本不变；负缓存避免对普通 exe 反复读 reparse point）。
pub fn alias_aumid(path: &str) -> Option<String> {
    static CACHE: LazyLock<Mutex<HashMap<String, Option<String>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    if let Ok(c) = CACHE.lock() {
        if let Some(hit) = c.get(path) {
            return hit.clone();
        }
    }
    let resolved = unsafe { read_alias_aumid(path) };
    if let Ok(mut c) = CACHE.lock() {
        c.insert(path.to_string(), resolved.clone());
    }
    resolved
}

unsafe fn read_alias_aumid(path: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;
    use windows::Win32::System::SystemServices::IO_REPARSE_TAG_APPEXECLINK;
    use windows::Win32::System::IO::DeviceIoControl;

    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let h = CreateFileW(
        PCWSTR::from_raw(wide.as_ptr()),
        FILE_READ_ATTRIBUTES.0,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        None,
        OPEN_EXISTING,
        FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
        None,
    )
    .ok()?;
    let mut buf = [0u8; 16 * 1024];
    let mut returned = 0u32;
    let ok = DeviceIoControl(
        h,
        FSCTL_GET_REPARSE_POINT,
        None,
        0,
        Some(buf.as_mut_ptr() as *mut c_void),
        buf.len() as u32,
        Some(&mut returned),
        None,
    );
    let _ = windows::Win32::Foundation::CloseHandle(h);
    if ok.is_err() || (returned as usize) < 16 {
        return None;
    }
    // 头部：tag(4) + data length(2) + reserved(2)。tag 不对（普通 exe/非别名）即退。
    let tag = u32::from_le_bytes(buf[0..4].try_into().ok()?);
    if tag != IO_REPARSE_TAG_APPEXECLINK {
        return None;
    }
    parse_aumid_from_utf16(buf.get(12..returned as usize)?)
}

/// 在 NUL 分隔的 UTF-16 串序列中按形状找 AUMID（`Family_Publisher!App`）。
fn parse_aumid_from_utf16(data: &[u8]) -> Option<String> {
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let mut cur = String::new();
    for u in units {
        if u == 0 {
            if looks_like_aumid(&cur) {
                return Some(cur);
            }
            cur.clear();
        } else if let Some(ch) = char::from_u32(u as u32) {
            cur.push(ch);
        }
    }
    looks_like_aumid(&cur).then_some(cur)
}

fn looks_like_aumid(s: &str) -> bool {
    let t = s.trim();
    (8..=200).contains(&t.len())
        && t.contains('!')
        && !t.contains('\\')
        && !t.contains('/')
        && t.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "._-!()".contains(ch))
}

/// 按 key 找「z 序最前」的匹配顶层窗口（任务 B 悬停预览的源窗口定位）。
/// EnumWindows 按 z 序枚举 → 第一个可见匹配即最近激活（卡面 2.4「z序最前」）。
/// 返回 (hwnd, minimized)。DWMWA_CLOAKED（虚拟桌面隐藏）窗口跳过——
/// 展示其缩略图等于泄漏用户隐藏的内容；同 key 有可见窗口时优先展示可见的。
pub fn find_window_for_key(key: &str) -> Option<(isize, bool)> {
    struct P<'a> {
        key: &'a str,
        seen: &'a mut Vec<isize>,
        hit: Option<(isize, bool)>,
    }
    unsafe extern "system" fn find_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let p = &mut *(lparam.0 as *mut P);
        // window_key 自带可见性/本进程/shell 类/工具窗过滤（与运行指示同语义）
        if let Some(k) = window_key(hwnd, p.seen) {
            if k == *p.key {
                if is_cloaked(hwnd) {
                    return true.into(); // cloaked：跳过，继续找 z 序更深的可见匹配
                }
                p.hit = Some((hwnd.0 as isize, IsIconic(hwnd).as_bool()));
                return false.into();
            }
        }
        true.into()
    }
    let mut seen: Vec<isize> = Vec::new();
    let mut payload = P {
        key,
        seen: &mut seen,
        hit: None,
    };
    unsafe {
        let _ = EnumWindows(Some(find_proc), LPARAM(&mut payload as *mut P as isize));
    }
    payload.hit
}

/// 窗口是否被 DWM 遮罩（虚拟桌面隐藏 / UWP 挂起等）。
fn is_cloaked(hwnd: HWND) -> bool {
    let mut c: u32 = 0;
    unsafe {
        let _ = windows::Win32::Graphics::Dwm::DwmGetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWA_CLOAKED,
            &mut c as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
    c != 0
}

/// 单次全量扫描（watcher 循环与调试探针共用）。
pub fn scan_once() -> HashSet<String> {
    let mut keys = HashSet::new();
    let mut seen_frames: Vec<isize> = Vec::new();
    unsafe {
        let payload = ScanPayload {
            keys: &mut keys,
            seen_frames: &mut seen_frames,
        };
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM(&payload as *const ScanPayload as *mut ScanPayload as isize),
        );
    }
    // 清理已销毁窗口的学习条目
    if let Ok(mut learned) = LEARNED_AUMIDS.lock() {
        learned.retain(|hwnd, _| seen_frames.contains(hwnd));
    }
    keys
}

struct ScanPayload<'a> {
    keys: &'a mut HashSet<String>,
    seen_frames: &'a mut Vec<isize>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let payload = &mut *(lparam.0 as *mut ScanPayload);
    let key = window_key(hwnd, payload.seen_frames);
    if let Some(k) = key {
        payload.keys.insert(k);
    }
    true.into()
}

/// 单个窗口 → 运行 key（None = 跳过）。
unsafe fn window_key(hwnd: HWND, seen_frames: &mut Vec<isize>) -> Option<String> {
    // 最小化窗口 IsWindowVisible 仍为 true —— 照常算运行中（任务栏同语义）
    if !IsWindowVisible(hwnd).as_bool() {
        return None;
    }
    let mut pid = 0u32;
    let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    if is_skipped_class(hwnd) || is_toolwindow(hwnd) {
        return None;
    }

    let exe = exe_path_of(pid);

    // 1) UWP/WinUI3 托管窗：frame 窗口里「pid ≠ 宿主」的子窗口属于应用真实进程
    //    （UWP-XAML 是 Windows.UI.Core.CoreWindow，WinUI3 是内容岛类，不硬编码类名），
    //    对该进程取 AUMID 并学习；应用挂起后子窗口销毁，用学习值兜底（见 LEARNED_AUMIDS）。
    let is_frame = exe
        .as_deref()
        .map(|e| e.to_lowercase().ends_with("applicationframehost.exe"))
        .unwrap_or(false);
    if is_frame {
        seen_frames.push(hwnd.0 as isize);
        let aumid = real_app_pid(hwnd, pid)
            .and_then(|app_pid| process_aumid(app_pid))
            .or_else(|| LEARNED_AUMIDS.lock().ok()?.get(&(hwnd.0 as isize)).cloned());
        if let Some(a) = aumid {
            if let Ok(mut learned) = LEARNED_AUMIDS.lock() {
                learned.insert(hwnd.0 as isize, a.clone());
            }
            return Some(format!("aumid:{}", a.to_lowercase()));
        }
        // 托管窗拿不到应用身份时不落 framehost 的 key：它不属于任何用户应用，
        // 也不会有快捷方式与它匹配。挂起恢复后子窗口回来，学习/解析会续上。
        return None;
    }

    // 2) 普通打包应用（Centennial）：窗口所属进程自带包标识
    if let Some(a) = process_aumid(pid) {
        return Some(format!("aumid:{}", a.to_lowercase()));
    }
    // 3) 普通 Win32：exe 路径
    exe.map(|e| format!("exe:{}", normalize_path(&e)))
    // frame 窗口拿不到 AUMID 时不落 framehost 的 exe key：它不属于任何用户应用，
    // 也不会有快捷方式与它匹配，落了只会污染 key 集合。
}

/// frame 窗口里「pid ≠ 宿主」的第一个子窗口 → 应用真实进程 pid。
/// UWP-XAML：Windows.UI.Core.CoreWindow；WinUI3 内容岛类名不同——不硬编码，
/// 通用按 pid 区分（子窗口宿主内容线程属于应用进程）。
unsafe fn real_app_pid(frame: HWND, host_pid: u32) -> Option<u32> {
    let mut after = HWND::default();
    loop {
        let child = match FindWindowExW(Some(frame), Some(after), PCWSTR::null(), PCWSTR::null()) {
            Ok(c) if !c.0.is_null() => c,
            _ => break,
        };
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(child, Some(&mut pid));
        if pid != 0 && pid != host_pid {
            return Some(pid);
        }
        after = child;
    }
    None
}

/// 进程的 AUMID（无包标识返回 None——对非打包进程失败极快）。
unsafe fn process_aumid(pid: u32) -> Option<String> {
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let r = process_aumid_handle(h);
    let _ = windows::Win32::Foundation::CloseHandle(h);
    r
}

unsafe fn process_aumid_handle(h: HANDLE) -> Option<String> {
    let mut len = 0u32;
    let err = GetApplicationUserModelId(h, &mut len, None);
    if err != ERROR_INSUFFICIENT_BUFFER || len == 0 || len as usize > 2048 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    let err2 = GetApplicationUserModelId(h, &mut len, Some(windows::core::PWSTR(buf.as_mut_ptr())));
    if err2.is_ok() {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..end]);
        (!s.is_empty()).then_some(s)
    } else {
        None
    }
}

unsafe fn exe_path_of(pid: u32) -> Option<String> {
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let r = QueryFullProcessImageNameW(
        h,
        PROCESS_NAME_WIN32,
        windows::core::PWSTR(buf.as_mut_ptr()),
        &mut len,
    );
    let _ = windows::Win32::Foundation::CloseHandle(h);
    r.ok().and_then(|_| {
        let s = String::from_utf16_lossy(&buf[..len as usize]);
        (!s.is_empty()).then_some(s)
    })
}

fn is_skipped_class(hwnd: HWND) -> bool {
    unsafe {
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut buf);
        let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        )
    }
}

unsafe fn is_toolwindow(hwnd: HWND) -> bool {
    let style = GetWindowLongPtrW(hwnd, WINDOW_LONG_PTR_INDEX(GWL_EXSTYLE.0));
    style & WS_EX_TOOLWINDOW != 0
}

unsafe fn watcher() {
    // 事件范围 CREATE(32768)..SHOW(32771) 覆盖 CREATE/DESTROY/HIDE/SHOW 四类
    let hook: HWINEVENTHOOK = SetWinEventHook(
        EVENT_OBJECT_CREATE,
        EVENT_OBJECT_SHOW,
        None,
        Some(winevent_proc),
        0,
        0,
        WINEVENT_OUTOFCONTEXT,
    );
    // 钩子随线程存活；本线程即进程生命周期，不做 UnhookWinEvent
    let _keep = hook;

    let mut last_scan = Instant::now() - Duration::from_secs(60);
    loop {
        // out-of-context 钩子回调经本线程消息循环投递，必须持续泵消息
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
                if msg.message == WM_QUIT {
                    return;
                }
            }
        }

        let now = Instant::now();
        let dirty = DIRTY.swap(false, Ordering::Relaxed);
        if (dirty && now - last_scan >= Duration::from_millis(RESCAN_THROTTLE_MS))
            || now - last_scan >= Duration::from_millis(POLL_INTERVAL_MS)
        {
            last_scan = now;
            let keys = scan_once();
            if let Ok(mut w) = RUNNING.write() {
                *w = keys;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

unsafe extern "system" fn winevent_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _idobject: i32,
    _idchild: i32,
    _ideventthread: u32,
    _dwmseventtime: u32,
) {
    DIRTY.store(true, Ordering::Relaxed);
}
