//! 主屏任务栏锚点推导的单测（2026-11-16 dock 不可见事故的门禁）。
//!
//! 纯逻辑本体在 `desktop.rs::resolve_primary_taskbar_rect`；这里只放用例。
//!
//! 事故根因：`primary_taskbar_rect()` 旧实现取「任意一条**可见**任务栏」。
//! 外部守卫 `tb_guard.ps1` 把**主屏** `Shell_TrayWnd` 藏起来（vis=false），
//! 而 explorer 把**副屏** `Shell_SecondaryTrayWnd` 弹了回来（vis=true）→
//! 可见列表只剩副屏那条 → dock 的 y 用副屏底边（top=1598），
//! 而水平居中用主屏宽（1920）→ x=(1920-1200)/2=360、y=1598-114-6=1478，
//! 窗口被摆到两块显示器之间的**无人区**（主屏 x 带内、副屏 y 带内），
//! 那里没有任何显示表面 → dock 完全不渲染（PrintWindow 仍能截到内容，
//! 因为 DComp 表面本身是好的，只是没被合成到屏幕）。
//!
//! 旧守卫每 250ms 硬藏**两条**任务栏 → 可见列表恒空 → 一直走「隐藏态重建」
//! 分支贴主屏，潜伏缺陷一直没暴露；新守卫改成「仅状态切换才动作」后
//! 才第一次踩中。

use windows::Win32::Foundation::RECT;

use super::resolve_primary_taskbar_rect;

fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

/// 事故场景：主屏任务栏被藏、副屏任务栏可见 → 必须用主屏那条。
#[test]
fn hidden_primary_taskbar_wins_over_visible_secondary() {
    let sw = 1920;
    let sh = 1080;
    // 主屏 Shell_TrayWnd：被守卫 SW_HIDE，rect 停在停泊位 (0,1078)-(1920,1126)。
    let primary = Some((rect(0, 1078, 1920, 1126), false));
    // 副屏 Shell_SecondaryTrayWnd：explorer 弹回来了，仍可见，rect (1920,1598)-(4480,1670)。
    let visible = vec![rect(1920, 1598, 4480, 1670)];

    let got = resolve_primary_taskbar_rect(primary, &visible, sw, sh).expect("必须有主屏锚");
    assert_eq!(
        got.top, 1078,
        "必须用主屏任务栏的 top，不能用副屏的 1598（否则 dock 落进两块屏的无人区）"
    );
    assert_eq!(got.left, 0);
    assert_eq!(got.right, sw);
    // 底边裁到屏内：dock.rs 的贴边唤出阈值 tr.bottom - REVEAL_EDGE_PX 必须可达。
    assert_eq!(got.bottom, sh);
}

/// 主屏任务栏正常可见时，直接用它自己的 rect（不做隐藏态重建）。
#[test]
fn visible_primary_taskbar_used_verbatim() {
    let primary = Some((rect(0, 1032, 1920, 1080), true));
    let got = resolve_primary_taskbar_rect(primary, &[], 1920, 1080).expect("必须有锚");
    assert_eq!(got.top, 1032);
    assert_eq!(got.bottom, 1080);
}

/// 幻影矩形：位置停在可见态、状态却是隐藏 → 按屏底重建停泊位（2026-11-07 热修口径）。
#[test]
fn phantom_hidden_rect_is_rebuilt_at_screen_bottom() {
    let primary = Some((rect(0, 1032, 1920, 1080), false));
    let got = resolve_primary_taskbar_rect(primary, &[], 1920, 1080).expect("必须有锚");
    assert_eq!(got.top, 1078, "幻影矩形必须按屏底重建");
    assert_eq!(got.bottom, 1080, "底边裁到屏内，保证贴边唤出阈值可达");
}

/// 极端兜底：主屏任务栏窗口不存在（explorer 重启窗口期）时，
/// 只能从可见列表里挑**与主屏相交**的那条——绝不能拿副屏的。
#[test]
fn fallback_picks_visible_bar_intersecting_primary_only() {
    let visible = vec![rect(1920, 1598, 4480, 1670)];
    assert!(
        resolve_primary_taskbar_rect(None, &visible, 1920, 1080).is_none(),
        "只有副屏可见时不得拿它当主屏锚"
    );

    let visible = vec![rect(1920, 1598, 4480, 1670), rect(0, 1032, 1920, 1080)];
    let got = resolve_primary_taskbar_rect(None, &visible, 1920, 1080).expect("主屏那条可用");
    assert_eq!(got.top, 1032);
}
