//! 通知中心数据库读取（主路径）—— **WAL 快照只读复制**。
//!
//! 数据源：`%LOCALAPPDATA%\Microsoft\Windows\Notifications\wpndatabase.db`（SQLite，WAL 模式）。
//!
//! 为什么必须复制快照而不是直接打开活动库：
//! - WPN 服务长期只把新通知写进 `-wal`、主库数小时不 checkpoint，**只复制 `.db` 会漏掉全部新通知**；
//! - 直接打开活动库会与系统服务争锁，且属于对系统文件的侵入式访问。
//!
//! 三个实测坑（旧仓踩过，按此绕行）：
//! 1. `-shm` 是共享内存索引，从活库复制过来会让副本状态不一致、打开即崩 → **只复制 db + wal**，
//!    并删掉目标目录残留的 shm/journal，让 SQLite 在副本上自行重建。
//! 2. 数据库被 Windows 重建后 `Id` 会变小 → 水位以 **ArrivalTime（FILETIME，墙钟单调）为主键**，
//!    Id 仅作同刻并列 tiebreak；检测到 Id 回退时放弃 Id 水位。
//! 3. 复制 ~4MB/轮太吵 → 先比对 WAL 的 (mtime, size)，没变化就整轮跳过。
//!
//! 隐私：临时副本放在 `%TEMP%`，**读完立即删除**；只解析 toast 的第一个 `<text>` 作标题，
//! 其余正文根本不构造进内存，不写日志、不写业务文件。
//! 硬约束 14：复制 + 查询在独立短命线程，外层 `recv_timeout(5s)` 硬截止，超时跳过本轮，
//! 绝不阻塞 UI / 事件循环。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use rusqlite::Connection;

use super::{now_unix_ms, RawNotification};

const POLL: Duration = Duration::from_millis(2_000);
const SCAN_DEADLINE: Duration = Duration::from_secs(5);

static SEQ: AtomicU64 = AtomicU64::new(0);

fn db_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|base| {
        PathBuf::from(base)
            .join("Microsoft")
            .join("Windows")
            .join("Notifications")
    })
}

/// 每次扫描用**唯一副本名**：硬超时后旧线程可能仍在收尾，共用一个文件名会互相踩。
fn temp_copy_path() -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("glasssuite_wpndb_{}_{n}.db", std::process::id()))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// FILETIME（100ns since 1601）→ Unix 毫秒。
///
/// 纪元差 116_444_736_000_000_000（100ns）除以 10_000 得 **11_644_473_600_000 毫秒**。
/// 参考仓写成了 116_444_736_000_000（多一个 0），结果所有现代 FILETIME 都被 clamp 成 0，
/// 表现为通知到达时间恒为 1970 —— 移植时按实测修正，并用单测锁住。
const UNIX_EPOCH_FILETIME_MS: i64 = 11_644_473_600_000;

fn filetime_to_unix_ms(ft: i64) -> u64 {
    (ft / 10_000 - UNIX_EPOCH_FILETIME_MS).max(0) as u64
}

/// 只取 toast XML 里**第一个** `<text>`，兼容 `<text id="1">` 带属性写法。
///
/// 刻意不返回全部文本：正文没有理由进入本进程，原文归 Windows 自己的横幅。
fn extract_title(xml: &str) -> Option<String> {
    let mut rest = xml;
    while let Some(pos) = rest.find("<text") {
        let after = &rest[pos..];
        let gt = after.find('>')?;
        let content = &after[gt + 1..];
        let end = content.find("</text>")?;
        let text = content[..end].trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
        rest = &content[end + 7..];
    }
    None
}

/// 发送方 ID 友好化：去掉包哈希后缀与 `!App` 后缀。
fn friendly_app(primary_id: &str) -> String {
    let base = primary_id.split('!').next().unwrap_or(primary_id);
    let base = base.split('_').next().unwrap_or(base);
    if base.is_empty() {
        primary_id.to_string()
    } else {
        base.to_string()
    }
}

type WalStamp = Option<(std::time::SystemTime, u64)>;

fn wal_stamp(dir: &Path) -> WalStamp {
    let wal = dir.join("wpndatabase.db-wal");
    let meta = std::fs::metadata(&wal).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// 复制 db + wal，删掉目标侧的 shm/journal 残留（坑 1）。
fn copy_snapshot(dir: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::copy(dir.join("wpndatabase.db"), dest)?;
    let _ = std::fs::remove_file(with_suffix(dest, "-shm"));
    let _ = std::fs::remove_file(with_suffix(dest, "-journal"));
    // WAL 可能刚 checkpoint 掉而暂时不存在，忽略缺失。
    let _ = std::fs::copy(dir.join("wpndatabase.db-wal"), with_suffix(dest, "-wal"));
    Ok(())
}

/// 副本及其伴生文件一律删除：通知数据不允许留在我们自己的临时目录里。
fn cleanup(dest: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(with_suffix(dest, suffix));
    }
}

/// 扫描水位。ArrivalTime 为主，Id 仅作并列 tiebreak（坑 2）。
#[derive(Clone, Copy)]
struct Mark {
    arrival: i64,
    id: i64,
}

pub fn spawn(tx: Sender<RawNotification>) -> bool {
    let Some(dir) = db_dir() else {
        winplat::log::log("notifications: db source unavailable (no LOCALAPPDATA)");
        return false;
    };
    if !dir.join("wpndatabase.db").exists() {
        winplat::log::log("notifications: db source unavailable (no database)");
        return false;
    }

    std::thread::Builder::new()
        .name("glasssuite-notification-db".into())
        .spawn(move || {
            // 启动基线：不回放历史通知。
            let mut mark = baseline(&dir).unwrap_or(Mark {
                arrival: i64::MAX,
                id: 0,
            });
            let mut known_wal = wal_stamp(&dir);
            winplat::log::log("notifications: db source started count=1");

            loop {
                std::thread::sleep(POLL);
                let current_wal = wal_stamp(&dir);
                if current_wal == known_wal {
                    continue; // 无变化：零 I/O
                }
                known_wal = current_wal;
                if let Some(next) = scan_bounded(&dir, mark, tx.clone()) {
                    mark = next;
                }
            }
        })
        .is_ok()
}

fn baseline(dir: &Path) -> Option<Mark> {
    let copy = temp_copy_path();
    let result = (|| {
        copy_snapshot(dir, &copy).ok()?;
        let conn = Connection::open(&copy).ok()?;
        let (arrival, id): (i64, i64) = conn
            .query_row(
                "SELECT IFNULL(MAX(ArrivalTime),0), IFNULL(MAX(Id),0) FROM Notification",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()?;
        Some(Mark { arrival, id })
    })();
    cleanup(&copy);
    result
}

/// 独立线程 + 硬截止：超时只是跳过本轮，绝不让 SQLite/文件 I/O 拖住进程。
fn scan_bounded(dir: &Path, mark: Mark, tx: Sender<RawNotification>) -> Option<Mark> {
    let copy = temp_copy_path();
    let dir = dir.to_path_buf();
    let (done_tx, done_rx) = mpsc::channel();

    std::thread::Builder::new()
        .name("glasssuite-notification-scan".into())
        .spawn(move || {
            let mut next = mark;
            let result = scan(&dir, &copy, &mut next, &tx);
            cleanup(&copy);
            // 只把「成功的新水位」送回去：Box<dyn Error> 不是 Send，且错误文本
            // 可能带本机路径，不该进日志。失败一律由外层记一条无内容的计数。
            let _ = done_tx.send(result.ok().map(|_| next));
        })
        .ok()?;

    match done_rx.recv_timeout(SCAN_DEADLINE) {
        Ok(Some(next)) => Some(next),
        Ok(None) => {
            winplat::log::log("notifications: db scan error, skipped count=1");
            None
        }
        Err(_) => {
            winplat::log::log("notifications: db scan timeout, skipped count=1");
            None
        }
    }
}

fn scan(
    dir: &Path,
    copy: &Path,
    mark: &mut Mark,
    tx: &Sender<RawNotification>,
) -> Result<(), Box<dyn std::error::Error>> {
    copy_snapshot(dir, copy)?;
    let conn = Connection::open(copy)?;

    let (max_id, max_arr): (i64, i64) = conn.query_row(
        "SELECT IFNULL(MAX(Id),0), IFNULL(MAX(ArrivalTime),0) FROM Notification",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if max_id < mark.id {
        // 数据库被重建：放弃 Id 水位，只靠时间水位（坑 2）。
        mark.id = 0;
        winplat::log::log("notifications: db id reset, fallback to time watermark count=1");
    }
    if max_arr < mark.arrival {
        mark.arrival = max_arr;
        mark.id = max_id;
        return Ok(());
    }

    let mut stmt = conn.prepare(
        "SELECT n.Id, n.Payload, n.ArrivalTime, IFNULL(h.PrimaryId, '') \
         FROM Notification n LEFT JOIN NotificationHandler h ON n.HandlerId = h.RecordId \
         WHERE IFNULL(n.ArrivalTime,0) > ?1 \
            OR (IFNULL(n.ArrivalTime,0) = ?1 AND n.Id > ?2) \
         ORDER BY IFNULL(n.ArrivalTime,0) ASC, n.Id ASC LIMIT 20",
    )?;

    let rows = stmt.query_map([mark.arrival, mark.id], |row| {
        let id: i64 = row.get(0)?;
        let payload: Option<Vec<u8>> = row.get(1)?;
        let arrival: Option<i64> = row.get(2)?;
        let handler: String = row.get(3)?;
        Ok((id, payload, arrival, handler))
    })?;

    for row in rows {
        let (id, payload, arrival, handler) = row?;
        let arr = arrival.unwrap_or(0);
        if arr > mark.arrival {
            mark.arrival = arr;
            mark.id = id;
        } else if arr == mark.arrival {
            mark.id = mark.id.max(id);
        }

        let Some(bytes) = payload else { continue };
        // 只解析标题；正文 XML 的其余部分随 bytes 一起在函数结束时释放。
        let Some(title) = extract_title(&String::from_utf8_lossy(&bytes)) else {
            continue;
        };
        let _ = tx.send(RawNotification {
            app: friendly_app(&handler),
            title,
            arrived_at: arrival.map(filetime_to_unix_ms).unwrap_or_else(now_unix_ms),
            source: "wpndatabase",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_first_text_with_attributes() {
        let xml = r#"<toast><binding><text id="1">第一条</text><text id="2">正文不应出现</text></binding></toast>"#;
        assert_eq!(extract_title(xml).as_deref(), Some("第一条"));
    }

    #[test]
    fn skips_empty_text_nodes() {
        let xml = "<toast><text>   </text><text>真正标题</text></toast>";
        assert_eq!(extract_title(xml).as_deref(), Some("真正标题"));
    }

    #[test]
    fn no_text_returns_none() {
        assert_eq!(extract_title("<toast><visual/></toast>"), None);
    }

    #[test]
    fn friendly_app_strips_package_hash() {
        assert_eq!(
            friendly_app("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App"),
            "Microsoft.WindowsNotepad"
        );
        assert_eq!(
            friendly_app("ai.opencode.desktop!App"),
            "ai.opencode.desktop"
        );
    }

    #[test]
    fn filetime_conversion() {
        // 1970-01-01 的 FILETIME = 116444736000000000 (100ns)
        assert_eq!(filetime_to_unix_ms(116_444_736_000_000_000), 0);
        assert_eq!(filetime_to_unix_ms(116_444_736_010_000_000), 1_000);
    }
}
