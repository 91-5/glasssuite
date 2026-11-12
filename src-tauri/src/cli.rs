//! `GlassSuite emit` —— 同机应用主动上报通知的 CLI（OC-20261113-001 P2 主路径）。
//!
//! 用法：
//! ```text
//! GlassSuite emit --app opencode --title "工作完成" --body "任务 A" --level info
//! ```
//!
//! 工作方式：读 `%APPDATA%\com.glasssuite.app\ingest.token`（由运行中的
//! GlassSuite 在通知开关打开时写入：`{port, token}`），向
//! `http://127.0.0.1:{port}/ingest` POST 一条结构化通知。
//!
//! 退出码：0 = 已受理（200）；1 = 失败（GlassSuite 未运行 / 通知未启用 / 网络错）。
//! 在 `main.rs` 里**先于**单实例锁分发——CLI 只是短命客户端，不是第二实例。

use std::time::Duration;

const USAGE: &str =
    "用法: GlassSuite emit --app <名> --title <标题> [--body <正文>] [--level info|warn|error]";

/// ingest 上报结果。
pub fn emit_main(args: &[String]) -> i32 {
    let mut app = String::new();
    let mut title = String::new();
    let mut body = String::new();
    let mut level = "info".to_string();
    let mut i = 2; // args[0]=程序名, args[1]=emit
    while i < args.len() {
        let flag = args[i].as_str();
        match flag {
            "--app" | "-a" => match args.get(i + 1) {
                Some(v) => {
                    app = v.clone();
                    i += 2;
                }
                None => return fail(USAGE),
            },
            "--title" | "-t" => match args.get(i + 1) {
                Some(v) => {
                    title = v.clone();
                    i += 2;
                }
                None => return fail(USAGE),
            },
            "--body" | "-b" => match args.get(i + 1) {
                Some(v) => {
                    body = v.clone();
                    i += 2;
                }
                None => return fail(USAGE),
            },
            "--level" | "-l" => match args.get(i + 1) {
                Some(v) => {
                    level = v.clone();
                    i += 2;
                }
                None => return fail(USAGE),
            },
            "--help" | "-h" => {
                println!("{USAGE}");
                return 0;
            }
            other => return fail(&format!("未知参数 {other}。{USAGE}")),
        }
    }
    if app.trim().is_empty() || title.trim().is_empty() {
        return fail(USAGE);
    }

    // 读 endpoint 文件
    let path = crate::island::notification::ingest::endpoint_file();
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("GlassSuite 未运行或通知同步未启用（读不到 {path:?}）");
        return 1;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        eprintln!("ingest.token 内容损坏");
        return 1;
    };
    let (Some(port), Some(token)) = (
        v["port"].as_u64().map(|p| p as u16),
        v["token"].as_str().map(|s| s.to_string()),
    ) else {
        eprintln!("ingest.token 缺 port/token 字段");
        return 1;
    };

    let payload = serde_json::json!({
        "app": app,
        "title": title,
        "body": body,
        "level": level,
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default(),
    });
    let json = payload.to_string();
    let result = ureq::post(&format!("http://127.0.0.1:{port}/ingest"))
        .set("Authorization", &format!("Bearer {token}"))
        .timeout(Duration::from_secs(5))
        .send_string(&json);
    match result {
        Ok(resp) if resp.status() == 200 => {
            println!("delivered");
            0
        }
        Ok(resp) => {
            eprintln!("ingest 返回 HTTP {}", resp.status());
            1
        }
        Err(ureq::Error::Status(st, _)) => {
            eprintln!("ingest 返回 HTTP {st}");
            1
        }
        Err(e) => {
            eprintln!("上报失败：{e}");
            1
        }
    }
}

fn fail(msg: &str) -> i32 {
    eprintln!("{msg}");
    1
}
