//! dock 配置：`dock.json` / `menu.json` 的读写、排序持久化与旧配置迁移。
//!
//! 数据结构与旧 `glassdesk` 保持兼容（字段名、`serde(default)` 缺省语义），
//! 这样从 `%APPDATA%\dev.glassdesk.app` 迁过来的文件不用转换就能读，
//! **图标顺序与收藏原样保留**（B8）。
//!
//! 迁移口径：只复制、不删除旧文件；目标已存在则跳过（不覆盖用户新配置）。
//! 日志只记文件名与数量，不记路径里的用户名。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 运行时从系统图标缓存取回的位图，缓存进配置避免每次启动重复调用 Shell API。
///
/// 不随发行包分发任何第三方图标资源：这是用户本机 `SHGetFileInfoW` 的结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IconData {
    #[serde(default)]
    pub w: u32,
    #[serde(default)]
    pub h: u32,
    /// `data:image/bmp;base64,…`，前端直接塞进 `<img src>`。
    #[serde(default, rename = "dataUrl")]
    pub data_url: String,
    /// 旧 `glassdesk` 把裸 RGBA base64 直接内联进配置（单个文件可达 90KB）。
    /// 读进来后统一丢弃、改由系统 API 运行时重取，既不写回旧格式也不分发图标资源。
    #[serde(default, rename = "rgba", skip_serializing)]
    pub legacy_rgba: Option<String>,
}

/// 固定 / 游离两种摆放模式。旧配置缺省为 dock。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum IconMode {
    #[default]
    Dock,
    Free,
}

/// 一条固定项。`id` 是 path 的稳定哈希，旧数据里已有则原样保留。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shortcut {
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub aumid: Option<String>,
    #[serde(default)]
    pub icon: Option<IconData>,
    #[serde(default)]
    pub mode: IconMode,
    #[serde(default, rename = "freeRect", alias = "free_rect")]
    pub free_rect: Option<FreeRect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FreeRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DockConfig {
    #[serde(default)]
    pub auto_hide: bool,
    #[serde(default)]
    pub shortcuts: Vec<Shortcut>,
    #[serde(default, rename = "dockTopLayer", alias = "dock_top_layer")]
    pub dock_top_layer: bool,
}

/// 开始菜单收藏。旧格式 `{ "apps": [ {name, path} ] }`。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MenuConfig {
    #[serde(default)]
    pub apps: Vec<MenuEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MenuEntry {
    pub name: String,
    pub path: String,
}

pub fn dock_path(data_dir: &Path) -> PathBuf {
    data_dir.join("dock.json")
}

pub fn menu_path(data_dir: &Path) -> PathBuf {
    data_dir.join("menu.json")
}

/// 稳定 id：与旧仓同口径（DefaultHasher(path) 的十六进制）。
pub fn stable_id(path: &str) -> String {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    format!("sc{:016x}", h.finish())
}

fn shortcut(name: &str, path: &str) -> Shortcut {
    Shortcut {
        id: stable_id(path),
        name: name.to_string(),
        path: path.to_string(),
        aumid: None,
        icon: None,
        mode: IconMode::Dock,
        free_rect: None,
    }
}

/// 首启种子：只用系统自带命令名，不内置任何人的绝对路径或安装目录。
pub fn seed_shortcuts() -> Vec<Shortcut> {
    vec![
        shortcut("文件资源管理器", "explorer.exe"),
        shortcut("记事本", "notepad.exe"),
        shortcut("计算器", "calc.exe"),
        shortcut("终端", "cmd.exe"),
        shortcut("浏览器", "msedge.exe"),
    ]
}

/// 迁移旧 `dev.glassdesk.app` 的两个配置文件。返回是否至少迁移了一个。
pub fn migrate_from(old_dir: &Path, new_dir: &Path) -> bool {
    let mut any = false;
    for name in ["dock.json", "menu.json"] {
        if winplat::config::migrate_file(&old_dir.join(name), &new_dir.join(name)) {
            // 日志含 `migrated` 关键字（B8 判据），且不含用户名/路径。
            winplat::log::log(&format!("dock: migrated {name} count=1"));
            any = true;
        }
    }
    any
}

/// 生产入口：按 identifier 派生旧目录，调纯函数完成迁移。
pub fn migrate_legacy(data_dir: &Path) -> bool {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return false;
    };
    migrate_from(&PathBuf::from(appdata).join("dev.glassdesk.app"), data_dir)
}

/// 把旧格式收敛掉：内联的裸 RGBA 图标一律丢弃（改由 `dock_icon` 运行时重取），
/// 顺带把 90KB 级的旧文件瘦回几 KB。返回是否真的改动过。
fn normalize_legacy(cfg: &mut DockConfig) -> bool {
    let mut changed = false;
    for s in &mut cfg.shortcuts {
        if s.icon
            .as_ref()
            .is_some_and(|i| i.data_url.is_empty() || i.legacy_rgba.is_some())
        {
            s.icon = None;
            changed = true;
        }
    }
    changed
}

pub fn load_dock(data_dir: &Path) -> DockConfig {
    let path = dock_path(data_dir);
    if let Some(mut cfg) = winplat::config::load_json::<DockConfig>(&path) {
        // 旧文件缺 shortcuts 键时给一组种子，避免空 dock 无从下手。
        if cfg.shortcuts.is_empty() {
            cfg.shortcuts = seed_shortcuts();
            let _ = winplat::config::save_json(&path, &cfg);
        } else if normalize_legacy(&mut cfg) {
            let _ = winplat::config::save_json(&path, &cfg);
        }
        return cfg;
    }
    let cfg = DockConfig {
        auto_hide: false,
        shortcuts: seed_shortcuts(),
        dock_top_layer: false,
    };
    if !path.exists() {
        let _ = winplat::config::save_json(&path, &cfg);
    } else {
        winplat::log::log("dock: 配置解析失败，使用默认值（文件保持原样）");
    }
    cfg
}

pub fn save_dock(data_dir: &Path, cfg: &DockConfig) -> Result<(), String> {
    winplat::config::save_json(&dock_path(data_dir), cfg).map_err(|e| e.to_string())
}

pub fn load_menu(data_dir: &Path) -> MenuConfig {
    winplat::config::load_json::<MenuConfig>(&menu_path(data_dir)).unwrap_or_default()
}

pub fn save_menu(data_dir: &Path, cfg: &MenuConfig) -> Result<(), String> {
    winplat::config::save_json(&menu_path(data_dir), cfg).map_err(|e| e.to_string())
}

/// 按 path 顺序重排；未在列表里的条目保持相对顺序追加到尾部。
pub fn reorder(cfg: &mut DockConfig, paths: &[String]) {
    let mut out = Vec::with_capacity(cfg.shortcuts.len());
    for p in paths {
        if let Some(idx) = cfg.shortcuts.iter().position(|s| &s.path == p) {
            out.push(cfg.shortcuts[idx].clone());
        }
    }
    for s in &cfg.shortcuts {
        if !paths.contains(&s.path) {
            out.push(s.clone());
        }
    }
    cfg.shortcuts = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gs-dock-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn seed_has_no_personal_paths_and_is_stable() {
        let seed = seed_shortcuts();
        assert_eq!(seed.len(), 5);
        assert!(seed
            .iter()
            .all(|s| !s.path.contains('\\') && !s.path.contains(':')));
        assert_eq!(seed[0].id, stable_id("explorer.exe"));
    }

    #[test]
    fn migration_preserves_order_and_favourites() {
        let old = tmp("migrate-old");
        let new = tmp("migrate-new");
        let body = r#"{"auto_hide":true,"shortcuts":[
            {"id":"a","name":"A","path":"a.exe"},
            {"id":"b","name":"B","path":"b.exe"},
            {"id":"c","name":"C","path":"c.exe"}],"dockTopLayer":false}"#;
        std::fs::write(old.join("dock.json"), body).unwrap();
        std::fs::write(
            old.join("menu.json"),
            r#"{"apps":[{"name":"X","path":"x.exe"}]}"#,
        )
        .unwrap();

        assert!(migrate_from(&old, &new));
        let cfg = load_dock(&new);
        assert_eq!(
            cfg.shortcuts
                .iter()
                .map(|s| s.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.exe", "b.exe", "c.exe"],
            "迁移后顺序必须保持"
        );
        assert_eq!(load_menu(&new).apps[0].path, "x.exe", "收藏必须保持");
        assert!(old.join("dock.json").is_file(), "旧文件不得删除");

        let _ = std::fs::remove_dir_all(&old);
        let _ = std::fs::remove_dir_all(&new);
    }

    #[test]
    fn migration_does_not_overwrite_newer_config() {
        let old = tmp("migrate-noover-old");
        let new = tmp("migrate-noover-new");
        std::fs::write(old.join("dock.json"), r#"{"shortcuts":[]}"#).unwrap();
        std::fs::write(
            new.join("dock.json"),
            r#"{"shortcuts":[{"id":"k","name":"K","path":"k.exe"}]}"#,
        )
        .unwrap();
        migrate_from(&old, &new);
        assert_eq!(load_dock(&new).shortcuts[0].path, "k.exe");
        let _ = std::fs::remove_dir_all(&old);
        let _ = std::fs::remove_dir_all(&new);
    }

    #[test]
    fn legacy_snake_case_config_keeps_order_and_drops_inline_icons() {
        let dir = tmp("legacy-shape");
        // 旧 glassdesk 的真实形状：snake_case 顶层键、free_rect、裸 RGBA 图标。
        let body = r#"{"auto_hide":true,"dock_top_layer":false,"shortcuts":[
            {"id":"a","name":"A","path":"a.exe","icon":{"w":48,"h":48,"rgba":"AAAA"},"aumid":null,"mode":"dock","free_rect":null},
            {"id":"b","name":"B","path":"b.exe","icon":null,"aumid":null,"mode":"dock","free_rect":null}]}"#;
        std::fs::write(dir.join("dock.json"), body).unwrap();
        let cfg = load_dock(&dir);
        assert_eq!(
            cfg.shortcuts
                .iter()
                .map(|s| s.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.exe", "b.exe"],
            "迁移后顺序必须原样保留"
        );
        assert!(cfg.auto_hide, "旧 auto_hide 生效");
        assert!(!cfg.dock_top_layer);
        assert!(cfg.shortcuts[0].icon.is_none(), "内联裸 RGBA 必须丢弃");
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("dock.json")).unwrap()).unwrap();
        assert!(saved.to_string().len() < body.len(), "旧文件应被瘦回新格式");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reorder_persists_across_restart() {
        let dir = tmp("reorder");
        let mut cfg = DockConfig {
            shortcuts: seed_shortcuts(),
            ..Default::default()
        };
        let paths = vec!["calc.exe".to_string(), "explorer.exe".to_string()];
        reorder(&mut cfg, &paths);
        save_dock(&dir, &cfg).unwrap();
        let back = load_dock(&dir);
        assert_eq!(back.shortcuts[0].path, "calc.exe");
        assert_eq!(back.shortcuts[1].path, "explorer.exe");
        assert_eq!(back.shortcuts.len(), 5, "未列出的条目不能丢");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
