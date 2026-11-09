//! 单实例保护。
//!
//! 口径：同名互斥量已存在 → 本次是「第二次启动」，调用方应立即退出（退出码 0，并记一行
//! `already running`），**不得**抢焦点或弹窗。
//!
//! 名字用 `Local\` 前缀：只在当前登录会话内唯一，符合桌面应用语义（多用户同时登录时
//! 各自有各自的实例；`Global\` 则需要额外权限）。旧 glassdesk 用
//! `Local\GlassDesk.SingleInstance`，合并后旧名必须消失，否则新旧两个 exe 互不相识、
//! 会同时驻留。

#[cfg(windows)]
pub struct SingleInstance {
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
// SAFETY: 互斥量句柄可由任意线程持有；Drop 里只做 CloseHandle，无跨线程共享可变状态。
unsafe impl Send for SingleInstance {}

#[cfg(windows)]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: 句柄由 CreateMutexW 创建，且只在这里关闭一次。
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

/// 尝试取得单实例锁。
///
/// - `Ok(Some(guard))`：本次是首个实例，`guard` 需存活到进程结束（Drop 时释放）。
/// - `Ok(None)`：已有实例在运行。
/// - `Err`：创建互斥量本身失败（权限等）。
#[cfg(windows)]
pub fn acquire(name: &str) -> Result<Option<SingleInstance>, String> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
    use windows::Win32::System::Threading::CreateMutexW;

    let wide = HSTRING::from(name);
    // SAFETY: 传入的是本地构造的宽字符串；不请求所有权（bInitialOwner=false），
    // 有效性完全靠「名字是否已存在」判定，不依赖线程归属。
    let handle = unsafe { CreateMutexW(None, false, &wide) }
        .map_err(|e| format!("CreateMutexW({name}) 失败：{e}"))?;

    let already = unsafe { windows::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS;
    if already {
        // 句柄已持有但语义上是「别人先到」，立即关掉，免得泄漏。
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
        return Ok(None);
    }

    Ok(Some(SingleInstance { handle }))
}

/// 本产品的互斥量名。改名字等于放弃与旧实例互斥，需同一次提交内处理迁移。
pub const NAME: &str = r"Local\GlassSuite.SingleInstance";

#[cfg(test)]
mod tests {
    #[test]
    fn name_is_session_local() {
        // 用 `Local\` 而非 `Global\`：多用户登录时各自一份，且不需要额外权限。
        assert!(super::NAME.starts_with(r"Local\"));
        assert!(!super::NAME.contains("GlassDesk"));
        assert!(!super::NAME.contains("GlassPill"));
    }

    /// 同进程内重复取得同名互斥量 → 第二次必须返回 `None`（与「第二次启动」同语义）。
    #[cfg(windows)]
    #[test]
    fn second_acquire_reports_already_running() {
        let name = r"Local\GlassSuite.Test.SecondAcquire";
        let first = super::acquire(name).expect("首次创建互斥量不应失败");
        assert!(first.is_some(), "首次应为 Some");
        let second = super::acquire(name).expect("重复创建不应报错");
        assert!(second.is_none(), "第二次必须识别为已运行");
        drop(first);
        // 释放后再取应重新成功（保证 Drop 真的关掉了句柄）。
        let third = super::acquire(name).expect("释放后重建不应失败");
        assert!(third.is_some(), "释放后应可重新取得");
    }
}
