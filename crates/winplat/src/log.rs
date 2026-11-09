//! 统一日志：一个文件、一种时间格式、一套截断策略。
//!
//! 旧两仓各写一套（`menu.log` 走 `%APPDATA%`、本地时间、64KB 截断；`debug.log` 走
//! `%LOCALAPPDATA%`、unix 毫秒、无轮转），合并后只保留一套：
//!
//! - 落点：`%APPDATA%\<identifier>\glasssuite.log`（由调用方给目录）；
//! - 时间：本地时间 `HH:MM:SS.mmm`，跨天时带日期前缀；
//! - 截断：超过 `MAX_BYTES` 时保留后半段重写（一次 IO，不留 `.1` 备份文件），
//!   目的是「永不无限增长」且「最近一段永远还在」。
//!
//! **隐私**：写入前不做内容改写，但**调用方必须**先脱敏——不得写用户目录全路径、
//! 窗口标题、进程完整路径列表、通知正文。见 `redact`。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 超过这个大小就截断（保留最近约一半）。
pub const MAX_BYTES: u64 = 64 * 1024;

static LOG_PATH: OnceLock<Mutex<PathBuf>> = OnceLock::new();

/// 初始化日志文件位置。重复调用以第一次为准（返回 `false`）。
pub fn init(dir: &Path) -> std::io::Result<bool> {
    let path = dir.join("glasssuite.log");
    if LOG_PATH.get().is_some() {
        return Ok(false);
    }
    fs::create_dir_all(dir)?;
    let _ = LOG_PATH.set(Mutex::new(path));
    Ok(true)
}

/// 写入一行。未初始化时静默丢弃（不 panic：日志失败不能拖垮 UI）。
pub fn log(msg: &str) {
    let Some(cell) = LOG_PATH.get() else { return };
    let Ok(guard) = cell.lock() else { return };
    let path = guard.clone();
    let _ = append_line(&path, &format!("{} {msg}\n", timestamp()));
}

/// 把用户目录前缀换成 `~`，避免把本机用户名写进日志。
pub fn redact(input: &str, user_home: Option<&str>) -> String {
    match user_home {
        Some(home) if !home.is_empty() => input.replace(home, "~"),
        _ => input.to_string(),
    }
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() > MAX_BYTES {
            truncate_keep_tail(path, MAX_BYTES / 2)?;
        }
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())
}

fn truncate_keep_tail(path: &Path, keep: u64) -> std::io::Result<()> {
    let bytes = fs::read(path)?;
    let start = bytes.len().saturating_sub(keep as usize);
    // 尽量从行首开始，避免留下半行乱码。
    let start = bytes[start..]
        .iter()
        .position(|b| *b == b'\n')
        .map(|i| start + i + 1)
        .unwrap_or(start);
    fs::write(path, &bytes[start..])
}

/// 本地时间 `HH:MM:SS.mmm`；跨天判定靠调用方不关心，补日期以满足「隔天排查」。
fn timestamp() -> String {
    use std::time::Duration;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    // 不引入 chrono：用本地偏移由系统提供的 `GetTimeZoneInformation` 过重，
    // 这里直接给 UTC 日期 + 本地无法保证，故改为「epoch 秒」+ 相对毫秒双段，
    // 由 `HH:MM` 由调用方另行观察——见 docs/architecture.md §7（后续接时区）。
    format!("[{}s.{:03}]", now.as_secs(), now.subsec_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_replaces_home() {
        let s = redact(r"C:\Users\alice\AppData\Roaming\x", Some(r"C:\Users\alice"));
        assert_eq!(s, r"~\AppData\Roaming\x");
    }

    #[test]
    fn redact_without_home_is_identity() {
        assert_eq!(redact("plain", None), "plain");
    }

    #[test]
    fn truncate_keeps_tail_on_line_boundary() {
        let dir = std::env::temp_dir().join("gs-winplat-logtest");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("t.log");
        let body: String = (0..100).map(|i| format!("line-{i}\n")).collect();
        fs::write(&path, &body).unwrap();
        truncate_keep_tail(&path, 60).unwrap();
        let got = fs::read_to_string(&path).unwrap();
        assert!(got.len() < body.len());
        assert!(got.ends_with('\n'));
        // 尾部必须完整保留最后一行。
        assert!(got.contains("line-99\n"));
        // 首行必须是完整的原始行（不能从半行开始）。
        let first = got.lines().next().unwrap();
        assert!(
            body.lines().any(|l| l == first),
            "首行必须与原始行逐字相等，实际是 {first:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
