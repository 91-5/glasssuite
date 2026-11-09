//! SMTC（System Media Transport Controls）媒体集成 —— **事件驱动，不轮询**。
//!
//! 移植自参考仓 `glasspill/src-tauri/src/media/smtc.rs`，按本卡 D1 收敛：
//! - 专用线程 `CoInitializeEx(MTA)` → `SessionManager::RequestAsync`；
//! - 订阅 manager 的 `SessionsChanged` 与每会话 `MediaPropertiesChanged` /
//!   `PlaybackInfoChanged` / `TimelinePropertiesChanged`，**回调只投递 `Refresh`**，
//!   不做任何 WinRT 阻塞调用，避免阻塞系统线程池；
//! - 本线程收到消息后重建快照并 emit `media-state`，控制指令也走同一条 mpsc；
//! - **歌词耦合已删除**（歌词在 `island::lyrics` 独立模块，按 D3 接入）；
//! - 保留 `baseUnixMs` / `rate` / `positionMs` / `durationMs`，前端用 1Hz 插值推进进度，
//!   不引入 30Hz 定时器：SMTC 的 position 更新粒度很粗，插值比高频轮询准且省 CPU。
//!
//! 会话选择：优先 `Playing`，否则取第一个；无会话 push `null`，前端据此回落时钟。
//!
//! 隐私：日志只记事件类型与数量，不写曲名 / 艺人 / 应用名。

use std::sync::{mpsc, Arc};
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlayStatus,
    MediaPropertiesChangedEventArgs, PlaybackInfoChangedEventArgs, SessionsChangedEventArgs,
    TimelinePropertiesChangedEventArgs,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows_future::{AsyncStatus, IAsyncOperation};

use crate::state::SharedState;

/// Windows FILETIME 纪元（1601-01-01）到 Unix 纪元的 100ns 差。
const WIN_EPOCH_100NS: i64 = 116_444_736_000_000_000;

/// 事件名。与 `packages/ipc-types` 的 `EVENTS.mediaState` 逐字一致。
pub const EVENT: &str = "media-state";

pub enum MediaMsg {
    Refresh,
    Control {
        action: String,
        position_ms: Option<f64>,
    },
}

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
    state: Arc<SharedState>,
    tx: mpsc::Sender<MediaMsg>,
}

/// 启动 SMTC 线程。只在 Windows 上有意义；其它平台整个模块不参与编译（见 `island/mod.rs`）。
pub fn spawn(app: AppHandle, state: Arc<SharedState>) {
    std::thread::Builder::new()
        .name("glasssuite-smtc".into())
        .spawn(move || {
            // WinRT 需要 COM；MTA 与回调跑在线程池的事实相符。
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            if let Err(e) = run(app, state) {
                winplat::log::log(&format!("smtc: 初始化失败：{e}"));
            }
        })
        .expect("spawn smtc thread");
}

/// `windows-future` 0.3 没有阻塞 `get()`（已改 IntoFuture）。专用线程上用 `Status()`
/// 轮询等待，初始化/控制指令调用频度极低，5ms 轮询开销可忽略。
///
/// ⚠ 不要试图换成 `.get()` 阻塞：卡面 §7「已知坑」记录了旧仓踩过的编译与死锁问题。
pub fn await_op<T: windows::core::RuntimeType>(
    op: &IAsyncOperation<T>,
) -> windows::core::Result<T> {
    loop {
        match op.Status()? {
            AsyncStatus::Completed => return op.GetResults(),
            AsyncStatus::Error | AsyncStatus::Canceled => {
                let hr = op.ErrorCode()?;
                return Err(windows::core::Error::from_hresult(hr));
            }
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

fn run(app: AppHandle, state: Arc<SharedState>) -> windows::core::Result<()> {
    let (tx, rx) = mpsc::channel::<MediaMsg>();
    let ctx = Ctx { app, state, tx };

    let manager = await_op(&SessionManager::RequestAsync()?)?;
    // 通道交出去之后，命令与 WinRT 回调都往同一线程投递，串行执行、无锁竞争。
    let _ = ctx.state.media_tx.set(ctx.tx.clone());

    {
        let c = ctx.clone();
        let handler: TypedEventHandler<SessionManager, SessionsChangedEventArgs> =
            TypedEventHandler::new(move |_, _| {
                let _ = c.tx.send(MediaMsg::Refresh);
                Ok(())
            });
        manager.SessionsChanged(&handler)?;
    }

    let mut sessions = current_sessions(&manager)?;
    // tokens 持有 Session 克隆与事件注册 token，Drop 即退订，必须活到线程结束。
    let mut tokens = subscribe_all(&ctx, &sessions)?;
    winplat::log::log(&format!("smtc: subscribed {} session(s)", tokens.len()));
    refresh(&ctx, &manager).ok();

    loop {
        match rx.recv() {
            Ok(MediaMsg::Refresh) => {
                let fresh = current_sessions(&manager).unwrap_or_default();
                if signature(&fresh) != signature(&sessions) {
                    tokens = subscribe_all(&ctx, &fresh)?;
                    winplat::log::log(&format!("smtc: resubscribed {} session(s)", tokens.len()));
                    sessions = fresh;
                }
                refresh(&ctx, &manager).ok();
            }
            Ok(MediaMsg::Control {
                action,
                position_ms,
            }) => {
                if let Err(e) = control(&manager, &action, position_ms) {
                    winplat::log::log(&format!("smtc: control 失败（action={action}）：{e}"));
                }
                // 部分播放器（尤其浏览器）事件回报滞后，延迟补一次刷新兜底。
                let c = ctx.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(250));
                    let _ = c.tx.send(MediaMsg::Refresh);
                });
            }
            Err(_) => break,
        }
    }
    Ok(())
}

/// 会话集合签名：只比 AUMID 列表，避免每次事件都全量重挂 handler。
fn signature(sessions: &[Session]) -> Vec<String> {
    let mut v: Vec<String> = sessions
        .iter()
        .map(|s| {
            s.SourceAppUserModelId()
                .map(|i| i.to_string())
                .unwrap_or_default()
        })
        .collect();
    v.sort();
    v
}

fn current_sessions(manager: &SessionManager) -> windows::core::Result<Vec<Session>> {
    let view = manager.GetSessions()?;
    let n = view.Size()?;
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        out.push(view.GetAt(i)?);
    }
    Ok(out)
}

fn subscribe_all(
    ctx: &Ctx,
    sessions: &[Session],
) -> windows::core::Result<Vec<(Session, [i64; 3])>> {
    let mut tokens = Vec::with_capacity(sessions.len());
    for s in sessions {
        let t1 = {
            let c = ctx.clone();
            let h: TypedEventHandler<Session, MediaPropertiesChangedEventArgs> =
                TypedEventHandler::new(move |_, _| {
                    let _ = c.tx.send(MediaMsg::Refresh);
                    Ok(())
                });
            s.MediaPropertiesChanged(&h)?
        };
        let t2 = {
            let c = ctx.clone();
            let h: TypedEventHandler<Session, PlaybackInfoChangedEventArgs> =
                TypedEventHandler::new(move |_, _| {
                    let _ = c.tx.send(MediaMsg::Refresh);
                    Ok(())
                });
            s.PlaybackInfoChanged(&h)?
        };
        let t3 = {
            let c = ctx.clone();
            let h: TypedEventHandler<Session, TimelinePropertiesChangedEventArgs> =
                TypedEventHandler::new(move |_, _| {
                    let _ = c.tx.send(MediaMsg::Refresh);
                    Ok(())
                });
            s.TimelinePropertiesChanged(&h)?
        };
        tokens.push((s.clone(), [t1, t2, t3]));
    }
    Ok(tokens)
}

/// 重建快照并推送。无会话时 push `null` 清空 UI（前端回落时钟）。
fn refresh(ctx: &Ctx, manager: &SessionManager) -> windows::core::Result<()> {
    let sessions = current_sessions(manager)?;
    let Some(active) = pick_active(&sessions) else {
        *ctx.state.last_media.lock().unwrap() = None;
        ctx.app.emit(EVENT, Value::Null).ok();
        return Ok(());
    };

    let info = active.GetPlaybackInfo()?;
    let status = info.PlaybackStatus()?;
    let playing = status == PlayStatus::Playing;
    let controls = info.Controls()?;
    // IReference 多数播放器不设置，空则按 1.0。
    let rate = info.PlaybackRate().and_then(|r| r.Value()).unwrap_or(1.0);

    let props = await_op(&active.TryGetMediaPropertiesAsync()?)?;
    let title = props.Title()?.to_string();
    let artist = props.Artist()?.to_string();
    let album = props.AlbumTitle()?.to_string();
    let artwork = read_thumbnail(&props.Thumbnail()?)
        .ok()
        .flatten()
        .map(|(ct, bytes)| {
            format!(
                "data:{ct};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        });

    let tl = active.GetTimelineProperties()?;
    let position_ms = tl.Position()?.Duration / 10_000;
    let duration_ms = tl.EndTime()?.Duration / 10_000;
    let updated = tl.LastUpdatedTime()?.UniversalTime;
    let base_unix_ms = (updated - WIN_EPOCH_100NS) / 10_000;

    let payload = json!({
        "title": title,
        "artist": artist,
        "album": album,
        "appId": active.SourceAppUserModelId()?.to_string(),
        "status": format!("{status:?}"),
        "playing": playing,
        "positionMs": position_ms,
        "baseUnixMs": base_unix_ms,
        "rate": if rate > 0.0 { rate } else { 1.0 },
        "durationMs": duration_ms,
        "canPlay": controls.IsPlayEnabled().unwrap_or(false),
        "canPause": controls.IsPauseEnabled().unwrap_or(false),
        "canNext": controls.IsNextEnabled().unwrap_or(false),
        "canPrev": controls.IsPreviousEnabled().unwrap_or(false),
        "artwork": artwork,
    });

    *ctx.state.last_media.lock().unwrap() = Some(payload.clone());
    ctx.app.emit(EVENT, payload).ok();
    Ok(())
}

fn pick_active(sessions: &[Session]) -> Option<Session> {
    sessions
        .iter()
        .find(|s| {
            s.GetPlaybackInfo()
                .and_then(|i| i.PlaybackStatus())
                .map(|st| st == PlayStatus::Playing)
                .unwrap_or(false)
        })
        .cloned()
        .or_else(|| sessions.first().cloned())
}

/// 读 SMTC 缩略流 → (content-type, bytes)。超过 8MB 直接放弃（异常封面不值得占内存）。
fn read_thumbnail(
    t: &IRandomAccessStreamReference,
) -> windows::core::Result<Option<(String, Vec<u8>)>> {
    let stream = await_op(&t.OpenReadAsync()?)?;
    let ct = stream.ContentType()?.to_string();
    let size = stream.Size()?;
    if size == 0 || size > 8_000_000 {
        return Ok(None);
    }
    let input = stream.GetInputStreamAt(0)?;
    let reader = DataReader::CreateDataReader(&input)?;
    let loaded = await_op(&reader.LoadAsync(size as u32)?)?;
    let mut bytes = vec![0u8; loaded as usize];
    reader.ReadBytes(&mut bytes)?;
    let _ = reader.Close();
    let _ = stream.Close();
    let ct = if ct.is_empty() {
        "image/png".to_string()
    } else {
        ct
    };
    Ok(Some((ct, bytes)))
}

fn control(
    manager: &SessionManager,
    action: &str,
    position_ms: Option<f64>,
) -> windows::core::Result<()> {
    let sessions = current_sessions(manager)?;
    let Some(s) = pick_active(&sessions) else {
        return Ok(());
    };
    match action {
        "play" => {
            await_op(&s.TryPlayAsync()?)?;
        }
        "pause" => {
            await_op(&s.TryPauseAsync()?)?;
        }
        "play_pause" => {
            let playing = s
                .GetPlaybackInfo()
                .and_then(|i| i.PlaybackStatus())
                .map(|st| st == PlayStatus::Playing)
                .unwrap_or(false);
            if playing {
                await_op(&s.TryPauseAsync()?)?;
            } else {
                await_op(&s.TryPlayAsync()?)?;
            }
        }
        "next" => {
            await_op(&s.TrySkipNextAsync()?)?;
        }
        "prev" => {
            await_op(&s.TrySkipPreviousAsync()?)?;
        }
        "seek" => {
            if let Some(ms) = position_ms {
                await_op(&s.TryChangePlaybackPositionAsync((ms * 10_000.0) as i64)?)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// 前端控制指令。`positionMs` 由 Tauri 从 camelCase 映射到 `position_ms`。
#[tauri::command]
pub fn media_control(
    state: tauri::State<'_, Arc<SharedState>>,
    action: String,
    position_ms: Option<f64>,
) -> Result<(), String> {
    let tx = state
        .media_tx
        .get()
        .ok_or_else(|| "media service not ready".to_string())?;
    tx.send(MediaMsg::Control {
        action,
        position_ms,
    })
    .map_err(|e| e.to_string())
}

/// 窗口重建 / 首帧取初值，规避「emit 早于前端订阅」的竞态（与歌词快照同款思路）。
#[tauri::command]
pub fn media_state(state: tauri::State<'_, Arc<SharedState>>) -> Option<Value> {
    state.last_media.lock().unwrap().clone()
}
