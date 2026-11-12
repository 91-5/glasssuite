//! WinRT `UserNotificationListener` 事件路径（补充源）。
//!
//! 已知限制：非 MSIX 打包的应用拿不到访问权（`0x80070490` / `AccessDenied`），
//! 这是 Windows 的平台限制，不是故障——此时静默降级，主路径 `db.rs` 仍然工作。
//!
//! 事件回调只把结果投递到汇聚通道，不做任何阻塞调用；日志不含应用名与标题。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Arc};
use std::time::Duration;

use windows::Foundation::TypedEventHandler;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::UI::Notifications::KnownNotificationBindings;
use windows::UI::Notifications::Management::{
    UserNotificationListener, UserNotificationListenerAccessStatus,
};

use super::{now_unix_ms, RawNotification};
use crate::island::media::await_op;

pub fn spawn(tx: Sender<RawNotification>, stop: Arc<AtomicBool>) -> bool {
    std::thread::Builder::new()
        .name("glasssuite-notification-listener".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            if run(tx, stop).is_err() {
                winplat::log::log("notifications: listener source unavailable, degraded count=1");
            }
        })
        .is_ok()
}

fn run(tx: Sender<RawNotification>, stop: Arc<AtomicBool>) -> windows::core::Result<()> {
    let listener = UserNotificationListener::Current()?;
    let mut access = listener.GetAccessStatus()?;
    if access == UserNotificationListenerAccessStatus::Unspecified {
        access = await_op(&listener.RequestAccessAsync()?)?;
    }
    if access != UserNotificationListenerAccessStatus::Allowed {
        return Err(windows::core::Error::new(
            windows::core::HRESULT(0x8007_04ECu32 as i32),
            "access denied",
        ));
    }

    let handler: TypedEventHandler<
        UserNotificationListener,
        windows::UI::Notifications::UserNotificationChangedEventArgs,
    > = TypedEventHandler::new(
        move |listener: windows::core::Ref<'_, UserNotificationListener>,
              args: windows::core::Ref<
            '_,
            windows::UI::Notifications::UserNotificationChangedEventArgs,
        >| {
            // 闭包参数必须显式标注：`TypedEventHandler::new` 无法从 `.ok()` 反推 Ref 的目标类型。
            let Ok(l) = listener.ok() else {
                return Ok(());
            };
            let Ok(a) = args.ok() else {
                return Ok(());
            };
            if a.ChangeKind()? != windows::UI::Notifications::UserNotificationChangedKind::Added {
                return Ok(());
            }
            let Ok(id) = a.UserNotificationId() else {
                return Ok(());
            };
            let Ok(notification) = l.GetNotification(id) else {
                return Ok(());
            };
            if let Some(item) = read_title(&notification) {
                let _ = tx.send(item);
            }
            Ok(())
        },
    );
    listener.NotificationChanged(&handler)?;
    winplat::log::log("notifications: listener source started count=1");

    // 保持 listener 与回调存活；WinRT 事件回调本身在系统线程池上，无需本地轮询。
    // 热停检查点：≤60s 内退出（非 socket 源，不影响 netstat 判据）。
    loop {
        if stop.load(Ordering::Relaxed) {
            winplat::log::log("notifications: listener source stopped count=1");
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// 只取第一段文本作标题，正文不构造、不外发。
fn read_title(
    notification: &windows::UI::Notifications::UserNotification,
) -> Option<RawNotification> {
    let app = notification
        .AppInfo()
        .and_then(|info| info.DisplayInfo())
        .and_then(|display| display.DisplayName())
        .map(|v| v.to_string())
        .unwrap_or_else(|_| "Windows 通知".to_string());
    let visual = notification.Notification().ok()?.Visual().ok()?;
    let binding_name = KnownNotificationBindings::ToastGeneric().ok()?;
    let binding = visual.GetBinding(&binding_name).ok()?;
    let texts = binding.GetTextElements().ok()?;
    for index in 0..texts.Size().ok()? {
        if let Ok(text) = texts.GetAt(index).and_then(|item| item.Text()) {
            let value = text.to_string();
            if !value.trim().is_empty() {
                return Some(RawNotification {
                    app,
                    title: value,
                    arrived_at: now_unix_ms(),
                    source: "user-notification-listener",
                });
            }
        }
    }
    None
}
