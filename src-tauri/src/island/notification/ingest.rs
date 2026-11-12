//! 本地 ingest 端点 —— **应用主动上报通知的主路径**（OC-20261113-001 P2）。
//!
//! 对「听得着」的源（OpenCode / Agnes Code 这类能在完成时跑脚本钩子的工具），
//! 最可靠的通知路径不是去猜系统数据库，而是它们主动上报。本模块：
//! - 只绑 `127.0.0.1`，随机端口；端口 + token 写进
//!   `%APPDATA%\com.glasssuite.app\ingest.token`（JSON），同机调用方读它即可；
//! - `POST /ingest`，Bearer token 鉴权（token 生成熵有限——它只防「其他低权用户」，
//!   同一用户下的进程本来就能读这个文件，不需要密码学强度）；
//! - 结构化字段 app/title/body/level/ts；**body 只参与去重哈希，不进
//!   RawNotification、不上前端、不落盘**（与系统三源同一隐私口径：正文不上岛）；
//! - 去重两级：① 卡面主键 `(app, hash(title+body), floor(ts/2000))`；②
//!   内容键 `(app, hash(title+body))` + LRU(512, TTL 90s)——同一内容在 TTL 内
//!   无论重发多少次（跨 2s 桶也一样）只算一条（B4 的口径）；
//! - `effective_ts = min(native_ts, first_seen)`，客户端时钟漂移/回灌不会把旧
//!   通知顶到最前。
//!
//! 关停语义：`shutdown` 置位后 accept 循环退出、TcpListener 被 drop（端口即刻
//! 解绑，`netstat` 里不再有监听）、token 文件删除。总开关关闭 = 零监听。

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::now_unix_ms;
use crate::island::notification::RawNotification;

/// endpoint 文件名（`%APPDATA%\com.glasssuite.app\` 下）。
pub const ENDPOINT_FILE: &str = "ingest.token";

/// 请求体上限（字节）。通知摘要用不了这么大，超了直接 413。
const MAX_BODY: usize = 8 * 1024;
/// 单连接读写超时：本地可信调用方，超时即断，不积累。
const CONN_TIMEOUT: Duration = Duration::from_secs(2);
/// 去重条目 TTL。
const DEDUP_TTL: Duration = Duration::from_secs(90);
/// 去重容量（LRU 上限，卡面 512）。
const DEDUP_CAP: usize = 512;

/// 客户端上报载荷。**没有 body 字段会流入通知数据结构**——body 在 `into_raw`
/// 之前只以哈希形式存在。
#[derive(serde::Deserialize)]
struct IngestPayload {
    app: String,
    title: String,
    #[serde(default)]
    body: String,
    /// 契约字段（CLI --level）。呈现层暂只走 L1 不分级，不进日志、不进去重哈希。
    #[serde(default)]
    #[allow(dead_code)]
    level: String,
    /// 客户端时间（Unix 毫秒）。缺省 = 服务器收到的时间。
    #[serde(default)]
    ts: Option<u64>,
}

fn endpoint_path() -> std::path::PathBuf {
    crate::settings::data_dir().join(ENDPOINT_FILE)
}

/// endpoint 文件路径（CLI 读取用）。
pub fn endpoint_file() -> std::path::PathBuf {
    endpoint_path()
}

/// FNV-1a 64bit：卡面写 xxhash；这里用零依赖的确定性哈希达成同一语义
/// （同进程内稳定、内容微变哈希剧变），不为 8 字节哈希新增依赖面。
fn fnv64(data: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// token：进程内可得到的熵（pid / 时间纳秒 / 端口 / 栈地址）过 FNV 两轮。
/// 威胁模型里它只需防「不同用户的本机进程」——文件 ACL 已限本用户。
fn make_token(port: u16) -> String {
    let a = &port as *const u16 as usize;
    let mut h = fnv64(&format!(
        "{}|{}|{}|{a}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
        port
    ));
    h = h.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ fnv64(&h.to_string());
    format!("{h:032x}")
}

/// 去重表：两级 key 共用一张 FIFO+TTL 表（卡面：主键 + LRU(512, TTL 90s)）。
#[derive(Default)]
struct DedupLru {
    seen: HashMap<String, Instant>,
    order: VecDeque<String>,
}

impl DedupLru {
    fn insert(&mut self, key: String) -> bool {
        // 先清过期（表头是最老的）
        loop {
            let expired = match self.order.front() {
                None => break,
                Some(front) => match self.seen.get(front) {
                    Some(t) => t.elapsed() >= DEDUP_TTL,
                    None => true,
                },
            };
            if !expired {
                break;
            }
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        if self.seen.contains_key(&key) {
            return false;
        }
        self.seen.insert(key.clone(), Instant::now());
        self.order.push_back(key);
        while self.seen.len() > DEDUP_CAP {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

/// 启动 ingest 服务器。`shutdown` 由调用方持有（`NotificationState.ingest_shutdown`）：
/// **同一把旗**，热停置位后 accept 循环退出、TcpListener drop、端口解绑。
/// 失败返回 None（不阻断其他通知源）。
pub fn spawn(tx: Sender<RawNotification>, shutdown: Arc<AtomicBool>) -> Option<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    let token = make_token(port);
    write_endpoint(port, &token);
    winplat::log::log(&format!(
        "notifications: ingest listening count=1 port={port}"
    ));

    let sd = Arc::clone(&shutdown);
    std::thread::Builder::new()
        .name("glasssuite-ingest".into())
        .spawn(move || {
            let _ = listener.set_nonblocking(true);
            let dedup = Mutex::new(DedupLru::default());
            loop {
                if sd.load(Ordering::Relaxed) {
                    break; // TcpListener 在此 drop —— 端口即刻解绑（B3 netstat 判据）
                }
                match listener.accept() {
                    Ok((stream, _)) => handle_conn(stream, &token, &tx, &dedup),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(150));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(300)),
                }
            }
            let _ = std::fs::remove_file(endpoint_path());
            winplat::log::log("notifications: ingest stopped count=1");
        })
        .ok()?;

    Some(port)
}

fn write_endpoint(port: u16, token: &str) {
    let body = serde_json::json!({ "port": port, "token": token }).to_string();
    if std::fs::write(endpoint_path(), body).is_err() {
        // 错误细节可能带本机路径，不进日志
        winplat::log::log("notifications: ingest token write failed count=1");
    }
}

/// 处理单连接：短请求短应答，`Connection: close`，异常一律不 panic。
fn handle_conn(
    mut stream: TcpStream,
    token: &str,
    tx: &Sender<RawNotification>,
    dedup: &Mutex<DedupLru>,
) {
    let _ = stream.set_read_timeout(Some(CONN_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONN_TIMEOUT));
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    // 手动逐行读（read_line）：BufReader 的缓冲在头部之后紧接收 body，
    // 同一个 reader 继续 read_exact 才不会丢字节。
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    // 头部
    let mut content_length = 0usize;
    let mut auth_ok = false;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return, // 连接提前断开
            Ok(_) => {}
            Err(_) => return,
        }
        let h = line.trim_end();
        if h.is_empty() {
            break;
        }
        let lower = h.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        if let Some(v) = lower.strip_prefix("authorization:") {
            let v = v.trim();
            let expect = format!("Bearer {token}");
            auth_ok = v.eq_ignore_ascii_case(&expect);
        }
    }
    if method != "POST" {
        respond(&mut stream, 405, "{\"error\":\"method\"}");
        return;
    }
    if path != "/ingest" {
        respond(&mut stream, 404, "{\"error\":\"path\"}");
        return;
    }
    if !auth_ok {
        respond(&mut stream, 401, "{\"error\":\"unauthorized\"}");
        return;
    }
    if content_length > MAX_BODY {
        respond(&mut stream, 413, "{\"error\":\"too large\"}");
        return;
    }
    let mut body = vec![0u8; content_length];
    if reader.read_exact(&mut body).is_err() {
        respond(&mut stream, 400, "{\"error\":\"body\"}");
        return;
    }
    let Ok(payload) = serde_json::from_slice::<IngestPayload>(&body) else {
        respond(&mut stream, 400, "{\"error\":\"json\"}");
        return;
    };
    if payload.app.trim().is_empty() || payload.title.trim().is_empty() {
        respond(&mut stream, 422, "{\"error\":\"empty app/title\"}");
        return;
    }

    // 两级去重：主键（含 2s 时间桶）+ 内容键（TTL 90s）。命中任一都只算一条。
    let hash = fnv64(&format!("{}\u{1f}{}", payload.title, payload.body));
    let native_ts = payload.ts.unwrap_or_else(now_unix_ms);
    let now = now_unix_ms();
    let first_seen = now; // 服务器视角的 first_seen
    let mut ok = false;
    if let Ok(mut d) = dedup.lock() {
        let k1 = format!("{}|{hash:016x}|{}", payload.app, native_ts / 2000);
        let k2 = format!("{}|{hash:016x}", payload.app);
        ok = d.insert(k1);
        if ok {
            ok = d.insert(k2);
        }
    }
    if ok {
        // effective_ts = min(native_ts, first_seen)：旧时间戳不回灌
        let arrived_at = native_ts.min(first_seen);
        let _ = tx.send(RawNotification {
            app: payload.app,
            title: payload.title,
            arrived_at,
            source: "ingest",
        });
        respond(&mut stream, 200, "{\"ok\":true}");
    } else {
        respond(&mut stream, 200, "{\"ok\":true,\"dedup\":true}");
    }
}

fn respond(stream: &mut TcpStream, code: u16, body: &str) {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_lru_primary_key_then_content_key() {
        // 复刻 handle_conn 的两级判定：主键（含 2s 桶）先查，命中后再查内容键。
        let mut d = DedupLru::default();
        let post = |d: &mut DedupLru, k1: &str| -> bool {
            if !d.insert(k1.to_string()) {
                return false;
            }
            d.insert("a|hash".to_string())
        };
        assert!(post(&mut d, "a|hash|1"), "首见 → 放行");
        assert!(!post(&mut d, "a|hash|1"), "同 2s 桶重发 → 主键命中");
        assert!(
            !post(&mut d, "a|hash|9"),
            "跨桶重发 → 内容键命中（B4 口径）"
        );

        // 不同内容 → 两级都是新条目
        let fresh = d.insert("a|hash2|9".to_string()) && d.insert("a|hash2".to_string());
        assert!(fresh);
        // 不同 app → 新条目
        let fresh = d.insert("b|hash|9".to_string()) && d.insert("b|hash".to_string());
        assert!(fresh);
    }

    #[test]
    fn dedup_lru_capacity_bound() {
        let mut d = DedupLru::default();
        for i in 0..(DEDUP_CAP + 50) {
            d.insert(format!("k{i}"));
        }
        assert!(d.seen.len() <= DEDUP_CAP, "容量必须封顶");
        // 最老的被逐出：k0 不在表里，重新插入算新条目
        assert!(d.insert("k0".into()));
    }

    #[test]
    fn fnv64_is_deterministic_and_sensitive() {
        assert_eq!(fnv64("hello"), fnv64("hello"));
        assert_ne!(fnv64("hello"), fnv64("hello "));
        assert_ne!(fnv64("标题|正文"), fnv64("标题|另一正文"));
    }

    /// 端到端：起服务器 → 带 token POST → 聚合通道收到；关停 → 端口拒绝连接。
    /// shutdown 旗由调用方传入（与 NotificationState.ingest_shutdown 同一把，
    /// 回归测试：热停必须解绑真实监听端口）。
    /// （B3/B4 的进程级等价证据；CLI 与真实 UI 走查见交付报告。）
    #[test]
    fn ingest_end_to_end_and_shutdown_unbinds_port() {
        let (tx, rx) = std::sync::mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let port = spawn(tx, Arc::clone(&shutdown)).expect("ingest 服务器应能启动");
        let token = read_token(port).expect("endpoint 文件应可读");

        // B4：同一内容连发 3 次 → 通道里只有 1 条
        for _ in 0..3 {
            let st = post_ingest(port, &token, "opencode", "工作完成", "任务 A");
            assert_eq!(st, 200);
        }
        let first = rx.recv_timeout(Duration::from_secs(2)).expect("应有 1 条");
        assert_eq!(first.app, "opencode");
        assert_eq!(first.title, "工作完成");
        assert_eq!(first.source, "ingest");
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "重复上报不得再进通道"
        );

        // 不同内容 → 再进一条
        assert_eq!(
            post_ingest(port, &token, "opencode", "构建结束", "exit 0"),
            200
        );
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());

        // 错 token → 401
        assert_eq!(post_ingest(port, "bad-token", "x", "y", ""), 401);

        // 关停 → 端口解绑（netstat 判据的进程级等价：connect 被拒绝）
        shutdown.store(true, Ordering::Relaxed);
        // 等 accept 循环退出（≤ 一轮 150ms 轮询 + 余量）
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
            "关停后端口必须拒绝连接（零监听）"
        );
    }

    fn read_token(port: u16) -> Option<String> {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(endpoint_path()).ok()?).ok()?;
        if v["port"].as_u64()? != u64::from(port) {
            return None;
        }
        Some(v["token"].as_str()?.to_string())
    }

    fn post_ingest(port: u16, token: &str, app: &str, title: &str, body: &str) -> u16 {
        let json = serde_json::json!({
            "app": app, "title": title, "body": body,
            "level": "info", "ts": now_unix_ms(),
        });
        match ureq::post(&format!("http://127.0.0.1:{port}/ingest"))
            .set("Authorization", &format!("Bearer {token}"))
            .timeout(Duration::from_secs(3))
            .send_string(&json.to_string())
        {
            Ok(resp) => resp.status(),
            Err(ureq::Error::Status(st, _)) => st,
            Err(_) => 0,
        }
    }
}
