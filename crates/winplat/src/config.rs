//! 配置存取：纯 JSON 落盘 + 旧版迁移。
//!
//! 不依赖 Tauri：目录由调用方传入（生产环境是 `%APPDATA%\<identifier>`），
//! 这样单测可以在临时目录里跑，也便于 `examples/probe` 直接读取真实配置。
//!
//! **迁移口径**：只搬运、不删除旧文件；目标已存在则跳过（不覆盖用户新配置）。
//! 旧版 identifier 是 `dev.glassdesk.app` / `dev.glasspill.app`，新的是 `com.glasssuite.app`。

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// 读 JSON。文件不存在、内容损坏、类型不符 → `None`（调用方用默认值，不阻断启动）。
pub fn load_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// 原子写 JSON：先写临时文件再替换，避免断电留下半截配置。
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &body)?;
    fs::rename(&tmp, path)
}

/// `%APPDATA%\<identifier>`（Roaming）。无 `APPDATA`（非 Windows / 精简环境）时返回 `None`。
pub fn app_data_dir(identifier: &str) -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join(identifier))
}

/// 旧配置迁移：`from` 存在且 `to` 不存在时复制一次。
///
/// 返回 `true` 表示本次发生了迁移（调用方应记日志）。
pub fn migrate_file(from: &Path, to: &Path) -> bool {
    if !from.is_file() || to.exists() {
        return false;
    }
    if let Some(parent) = to.parent() {
        if fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    fs::copy(from, to).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        pinned: Vec<String>,
        margin: i32,
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gs-winplat-cfg-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn roundtrip() {
        let dir = tmp("roundtrip");
        let path = dir.join("dock.json");
        let v = Sample {
            pinned: vec!["a".into(), "b".into()],
            margin: 6,
        };
        save_json(&path, &v).unwrap();
        let back: Sample = load_json(&path).unwrap();
        assert_eq!(back, v);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_returns_none_not_panic() {
        let dir = tmp("corrupt");
        let path = dir.join("bad.json");
        fs::write(&path, b"{ not json").unwrap();
        assert!(load_json::<Sample>(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_does_not_overwrite() {
        let dir = tmp("migrate");
        let from = dir.join("old.json");
        let to = dir.join("new.json");
        fs::write(&from, br#"{"pinned":[],"margin":1}"#).unwrap();

        assert!(migrate_file(&from, &to));
        assert_eq!(
            fs::read_to_string(&to).unwrap(),
            r#"{"pinned":[],"margin":1}"#
        );

        fs::write(&to, br#"{"pinned":["keep"],"margin":9}"#).unwrap();
        assert!(!migrate_file(&from, &to), "目标已存在时必须跳过");
        assert!(fs::read_to_string(&to).unwrap().contains("keep"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_is_noop_without_source() {
        let dir = tmp("migrate-none");
        assert!(!migrate_file(&dir.join("nope.json"), &dir.join("x.json")));
        let _ = fs::remove_dir_all(&dir);
    }
}
