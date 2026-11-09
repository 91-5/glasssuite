//! 开始菜单：列举本机「开始菜单 / 程序」里的快捷方式并启动。
//!
//! 只做**本地目录枚举**，不解析 `.lnk` 二进制、不查询商店、不联网；启动交给
//! `ShellExecuteW`（由 Windows 自己解析快捷方式）。目录遍历与启动都放进独立短命线程
//! 并设硬截止：网络映射的公共开始菜单目录可能让 `read_dir` 卡住，单进程下会出现
//! dock 卡住连带 island 卡住（卡面 D5 / 硬约束 14）。
//!
//! 日志只记事件类型与数量，不记应用名 / 路径（硬约束 13）。

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use windows::core::{w, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const LIST_DEADLINE: Duration = Duration::from_secs(2);
const LAUNCH_DEADLINE: Duration = Duration::from_secs(1);
const MAX_ENTRIES: usize = 200;
const MAX_DEPTH: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MenuApp {
    pub name: String,
    /// `.lnk` / `.exe` 的绝对路径（用户本机，绝不随包分发）。
    pub path: String,
}

/// 两次 `read_dir` 的经典卡点是共享/网络目录；外层 2s 硬截止后返回已收集为空集合。
pub fn list_apps_bounded() -> Vec<MenuApp> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("glasssuite-menu-scan".into())
        .spawn(move || {
            let _ = tx.send(scan_dirs(&start_menu_dirs()));
        })
        .is_ok();
    if !spawned {
        return Vec::new();
    }
    match rx.recv_timeout(LIST_DEADLINE) {
        Ok(v) => v,
        Err(_) => {
            winplat::log::log("dock: menu scan timeout, degraded count=1");
            Vec::new()
        }
    }
}

fn start_menu_dirs() -> Vec<PathBuf> {
    let tail = PathBuf::from(r"Microsoft\Windows\Start Menu\Programs");
    ["APPDATA", "ProgramData"]
        .iter()
        .copied()
        .filter_map(std::env::var_os)
        .map(|base| PathBuf::from(base).join(&tail))
        .collect()
}

fn is_app_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("lnk") | Some("exe")
    )
}

/// 纯函数：给一组目录，返回去重后的条目（便于单测，不依赖真实机器）。
fn scan_dirs(dirs: &[PathBuf]) -> Vec<MenuApp> {
    let mut out: Vec<MenuApp> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in dirs {
        walk(dir, 0, &mut out, &mut seen);
        if out.len() >= MAX_ENTRIES {
            break;
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out.truncate(MAX_ENTRIES);
    out
}

fn walk(
    dir: &Path,
    depth: usize,
    out: &mut Vec<MenuApp>,
    seen: &mut std::collections::HashSet<String>,
) {
    if depth > MAX_DEPTH || out.len() >= MAX_ENTRIES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, depth + 1, out, seen);
            continue;
        }
        if !is_app_file(&path) {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let lower = path.to_string_lossy().to_lowercase();
        if !seen.insert(lower.clone()) {
            continue;
        }
        out.push(MenuApp {
            name: name.to_string(),
            path: path.to_string_lossy().to_string(),
        });
        if out.len() >= MAX_ENTRIES {
            return;
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 启动一个应用。1s 没返回就记一行降级日志并让线程自行结束，不阻塞命令线程。
pub fn launch_bounded(path: &str) -> bool {
    if path.trim().is_empty() {
        return false;
    }
    let (tx, rx) = mpsc::channel();
    let owned = path.to_string();
    let spawned = std::thread::Builder::new()
        .name("glasssuite-launch".into())
        .spawn(move || {
            let _ = tx.send(launch(&owned));
        })
        .is_ok();
    if !spawned {
        return false;
    }
    match rx.recv_timeout(LAUNCH_DEADLINE) {
        Ok(v) => v,
        Err(_) => {
            winplat::log::log("dock: launch timeout, degraded count=1");
            false
        }
    }
}

fn launch(path: &str) -> bool {
    let file = wide(path);
    unsafe {
        let ret = ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        // ShellExecute 的返回值 > 32 表示成功（否则是错误码）。
        ret.0 as isize > 32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gs-menu-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        dir
    }

    #[test]
    fn only_shortcuts_and_exes_are_listed() {
        assert!(is_app_file(Path::new("a.lnk")));
        assert!(is_app_file(Path::new("A.EXE")));
        assert!(!is_app_file(Path::new("readme.txt")));
        assert!(!is_app_file(Path::new("no-extension")));
    }

    #[test]
    fn scan_is_recursive_sorted_and_deduplicated() {
        let dir = tmp("scan");
        std::fs::write(dir.join("Zed.lnk"), b"x").unwrap();
        std::fs::write(dir.join("alpha.lnk"), b"x").unwrap();
        std::fs::write(dir.join("skip.txt"), b"x").unwrap();
        std::fs::write(dir.join("sub").join("Beta.exe"), b"x").unwrap();
        let apps = scan_dirs(&[dir.clone(), dir.clone()]);
        assert_eq!(apps.len(), 3, "重复目录不能产生重复条目");
        assert_eq!(apps[0].name, "alpha");
        assert_eq!(apps[1].name, "Beta");
        assert_eq!(apps[2].name, "Zed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_path_is_not_launched() {
        assert!(!launch_bounded(""));
    }
}
