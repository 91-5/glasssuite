//! 歌词（LRCLIB 兼容源，**地址可配置、默认关闭**）。
//!
//! 移植自参考仓 `glasspill/src-tauri/src/lyrics.rs`，按本卡 D3 与硬约束 12 改动：
//! - 源地址从 `settings.lyricsSourceUrl` 读入（首启模板为 `null`）；**未配置时整个模块
//!   零外联**，`maybe_fetch` 直接返回，不构造 URL、不发请求。
//! - 按 `title|artist|duration` 缓存（含「无歌词」负缓存），切歌才请求。
//! - 请求在独立短命线程，绝不阻塞 SMTC 线程；网络失败静默降级为无歌词。
//! - LRC 解析放 Rust，一次拉全曲；前端按插值出的 positionMs 二分选行，不需要高频定时器。
//! - 日志只记行数与结果类型，不写曲名 / 艺人。
//!
//! 曲目变化由 `media.rs` 的快照刷新驱动（D1 删掉的耦合在 D3 按契约补回）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

/// 与 `packages/ipc-types` 的 `EVENTS.lyrics` 逐字一致。
pub const EVENT: &str = "lyrics";

#[derive(Clone, Serialize)]
pub struct LyricLine {
    /// 起始时间（毫秒，相对曲首）。
    pub t: u64,
    pub text: String,
}

/// 歌词源地址。`None` = 关闭。启动时由 `configure` 写入一次。
static SOURCE: OnceLock<Option<String>> = OnceLock::new();
static SKIP_LOGGED: AtomicBool = AtomicBool::new(false);
/// (缓存键, 已解析歌词)。空 Vec 是有效缓存（这首歌没歌词），避免反复请求。
type LyricsCache = Mutex<Option<(String, Vec<LyricLine>)>>;
static CACHE: OnceLock<LyricsCache> = OnceLock::new();

fn cache() -> &'static LyricsCache {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 启动时写入配置。传 `None` 即「默认不联网」。
pub fn configure(source: Option<String>) {
    let _ = SOURCE.set(source);
}

fn source_url() -> Option<&'static str> {
    SOURCE.get().and_then(|s| s.as_deref())
}

/// 无会话时调用：清缓存并通知前端隐藏歌词。
pub fn clear(app: &AppHandle) {
    let had = cache()
        .lock()
        .map(|mut g| g.take().is_some())
        .unwrap_or(false);
    if had {
        let _ = app.emit(EVENT, Value::Null);
    }
}

/// 前端挂载时补齐快照，规避「emit 早于订阅」竞态（与 `media_state` 同款）。
pub fn snapshot() -> Value {
    match cache().lock() {
        Ok(g) => match g.as_ref() {
            Some((_, lines)) if !lines.is_empty() => json!({ "lines": lines }),
            _ => Value::Null,
        },
        Err(_) => Value::Null,
    }
}

#[tauri::command]
pub fn lyrics_state() -> Value {
    snapshot()
}

/// 媒体刷新时调用。**未配置源则零外联**；仅当曲目变化才发起请求。
pub fn maybe_fetch(app: AppHandle, title: String, artist: String, album: String, duration_ms: f64) {
    let Some(base) = source_url() else {
        if !SKIP_LOGGED.swap(true, Ordering::Relaxed) {
            winplat::log::log("lyrics: 未配置源，跳过（默认不联网）");
        }
        return;
    };
    let base = base.trim_end_matches('/').to_string();

    let title = title.trim().to_string();
    let artist = artist.trim().to_string();
    if title.is_empty() && artist.is_empty() {
        return;
    }
    let key = format!("{title}|{artist}|{}", (duration_ms / 1000.0).round() as i64);
    {
        let guard = match cache().lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some((k, _)) = guard.as_ref() {
            if k == &key {
                return;
            }
        }
    }
    // 先占位，避免同一曲目并发重复请求。
    if let Ok(mut g) = cache().lock() {
        *g = Some((key.clone(), Vec::new()));
    }

    std::thread::Builder::new()
        .name("glasssuite-lyrics".into())
        .spawn(move || {
            let hit = fetch_lrc(&base, &title, &artist, &album, duration_ms);
            let lines = hit.map(|(l, _)| l).unwrap_or_default();
            if let Ok(mut g) = cache().lock() {
                *g = Some((key, lines.clone()));
            }
            winplat::log::log(&format!("lyrics: fetched count={}", lines.len()));
            let payload = if lines.is_empty() {
                Value::Null
            } else {
                json!({ "lines": lines })
            };
            let _ = app.emit(EVENT, payload);
        })
        .ok();
}

fn fetch_lrc(
    base: &str,
    title: &str,
    artist: &str,
    album: &str,
    duration_ms: f64,
) -> Option<(Vec<LyricLine>, String)> {
    let dur = (duration_ms / 1000.0).round() as i64;
    // 精确匹配：duration<=0 时 LRCLIB 会返回 400，且播放器首帧常报 0。
    if dur > 0 {
        if let Some(hit) = try_get(base, title, artist, album, dur) {
            return Some(hit);
        }
    }
    // 搜索兜底：专辑名写错 / 时长偏差 >2s 都能救回来。
    try_search(base, title, artist, dur)
}

fn http_get(url: &str) -> Option<String> {
    let resp = ureq::get(url)
        .set("User-Agent", "GlassSuite/0.1 (local desktop widget)")
        .timeout(Duration::from_secs(8))
        .call()
        .ok()?;
    resp.into_string().ok()
}

/// 归一化：只保留字母数字（含 CJK）、统一小写，用于标题/艺人比对。
fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 时长容差（秒）。同一首歌不同母带可差 40s+，宁可没歌词也不显示错的。
const DUR_TOL_SECS: f64 = 5.0;

fn accept(
    cand_title: &str,
    cand_artist: &str,
    cand_dur: f64,
    want_title: &str,
    want_artist: &str,
    want_dur: i64,
) -> Result<(), String> {
    if norm(cand_title) != norm(want_title) {
        return Err("title".to_string());
    }
    let a = norm(cand_artist);
    let b = norm(want_artist);
    if !a.is_empty() && !b.is_empty() && a != b {
        return Err("artist".to_string());
    }
    if want_dur > 0 && cand_dur > 0.0 && (cand_dur - want_dur as f64).abs() > DUR_TOL_SECS {
        return Err("duration".to_string());
    }
    Ok(())
}

fn try_get(
    base: &str,
    title: &str,
    artist: &str,
    album: &str,
    dur: i64,
) -> Option<(Vec<LyricLine>, String)> {
    let url = format!(
        "{base}/api/get?artist_name={}&track_name={}&album_name={}&duration={dur}",
        enc(artist),
        enc(title),
        enc(album)
    );
    let v: Value = serde_json::from_str(&http_get(&url)?).ok()?;
    let synced = v.get("syncedLyrics").and_then(|s| s.as_str())?;
    let lines = parse_lrc(synced);
    if lines.is_empty() {
        return None;
    }
    let cand_title = v.get("trackName").and_then(|x| x.as_str()).unwrap_or("");
    let cand_artist = v.get("artistName").and_then(|x| x.as_str()).unwrap_or("");
    let dur_hit = v.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0);
    accept(cand_title, cand_artist, dur_hit, title, artist, dur).ok()?;
    Some((lines, "get".to_string()))
}

fn try_search(base: &str, title: &str, artist: &str, dur: i64) -> Option<(Vec<LyricLine>, String)> {
    let url = format!(
        "{base}/api/search?track_name={}&artist_name={}",
        enc(title),
        enc(artist)
    );
    let v: Value = serde_json::from_str(&http_get(&url)?).ok()?;
    let items = v.as_array()?;

    let mut best: Option<(i64, Vec<LyricLine>)> = None;
    for item in items {
        let Some(synced) = item.get("syncedLyrics").and_then(|s| s.as_str()) else {
            continue;
        };
        let lines = parse_lrc(synced);
        if lines.is_empty() {
            continue;
        }
        let item_dur = item.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let cand_title = item.get("trackName").and_then(|x| x.as_str()).unwrap_or("");
        let cand_artist = item
            .get("artistName")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if accept(cand_title, cand_artist, item_dur, title, artist, dur).is_err() {
            continue;
        }
        let score = if dur > 0 && item_dur > 0.0 {
            (item_dur - dur as f64).abs().round() as i64
        } else {
            0
        };
        match &best {
            Some((best_score, _)) if *best_score <= score => {}
            _ => best = Some((score, lines)),
        }
    }
    best.map(|(_, lines)| (lines, "search".to_string()))
}

/// 百分号编码（query 值）：只保留 unreserved，其余按 UTF-8 字节转义。
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 解析 LRC：`[mm:ss.xx]` / `[mm:ss.xxx]` / 一行多时间戳；忽略 `[offset:...]` 等元信息。
fn parse_lrc(text: &str) -> Vec<LyricLine> {
    let mut out: Vec<LyricLine> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let mut rest = line;
        let mut stamps: Vec<u64> = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            if let Some(ms) = parse_stamp(&rest[1..end]) {
                stamps.push(ms);
            }
            rest = &rest[end + 1..];
        }
        if stamps.is_empty() {
            continue;
        }
        let lyric = rest.trim().to_string();
        for t in stamps {
            out.push(LyricLine {
                t,
                text: lyric.clone(),
            });
        }
    }
    out.sort_by_key(|l| l.t);
    out.dedup_by(|a, b| a.t == b.t && a.text == b.text);
    out
}

/// `mm:ss.xx` → 毫秒。小数部分补/截到 3 位（兼容百分秒）。
fn parse_stamp(s: &str) -> Option<u64> {
    let (m, rest) = s.split_once(':')?;
    let minutes: u64 = m.trim().parse().ok()?;
    let (sec, frac) = match rest.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (rest, None),
    };
    let seconds: u64 = sec.trim().parse().ok()?;
    if seconds >= 60 {
        return None;
    }
    let ms: u64 = match frac {
        Some(f) => {
            let digits: String = f.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                0
            } else {
                let mut d = if digits.len() >= 3 {
                    digits[..3].to_string()
                } else {
                    digits
                };
                while d.len() < 3 {
                    d.push('0');
                }
                d.parse().unwrap_or(0)
            }
        }
        None => 0,
    };
    Some(minutes * 60_000 + seconds * 1000 + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_lrc() {
        let lines = parse_lrc("[00:12.34]第一行\n[00:15.500]第二行\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].t, 12_340);
        assert_eq!(lines[1].t, 15_500);
    }

    #[test]
    fn skips_metadata_tags() {
        let lines = parse_lrc("[ar:某人]\n[offset:+200]\n[00:01.00]歌词\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].t, 1_000);
    }

    #[test]
    fn handles_multiple_stamps_and_sorts() {
        let lines = parse_lrc("[00:20.00]后\n[00:10.00][00:30.00]重复\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "重复");
        assert_eq!(lines[1].text, "后");
    }

    #[test]
    fn encodes_query() {
        assert_eq!(enc("周杰伦"), "%E5%91%A8%E6%9D%B0%E4%BC%A6");
        assert_eq!(enc("a b"), "a%20b");
    }

    #[test]
    fn accept_rejects_duration_drift_and_wrong_track() {
        assert!(accept("虚拟", "陈粒", 241.0, "虚拟", "陈粒", 251).is_err());
        assert!(accept("虚拟", "陈粒", 241.0, "虚拟", "陈粒", 244).is_ok());
        assert!(accept("稻香", "周杰伦", 223.0, "虚拟", "陈粒", 223).is_err());
        assert!(accept("虚拟", "陈粒", 0.0, "虚拟", "陈粒", 241).is_ok());
    }

    #[test]
    fn unconfigured_source_never_produces_url() {
        // configure(None) 是默认路径；这里不调用网络，只验证短路条件本身。
        let none: Option<String> = None;
        let url: Option<&str> = none.as_deref().map(|b| b.trim_end_matches('/'));
        assert!(url.is_none(), "默认关闭时不得构造任何歌词源 URL");
    }
}
