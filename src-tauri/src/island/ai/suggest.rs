//! 主动建议 —— **纯本地硬规则，不调 API、不联网**。
//!
//! 从参考仓 `glasspill/src-tauri/src/ai/suggest.rs` 收敛，并按 OC-20261113-001 P3
//! 扩展为「到期窗口 + 每日标记」的主动建议引擎。核心判断（卡面引 dp）：**LLM 边际
//! 价值只在措辞，内容是确定的——架构重点放在「何时说、要不要说」**。
//!
//! 可测性（B5）：时钟（`Clock`）、输入空闲（`InputState`）、媒体播放、静音全部由
//! 调用方注入，规则层不碰 Win32、不读系统时钟，任何平台都能固定时钟单测。
//!
//! 规则一览（按优先级）：
//! - battery / cpu / memory：环境硬规则（沿用原四类里的三类）；
//! - lunch 11:15–11:30 / dinner 17:15–17:30：到期窗口 + 每日标记（一天最多一次）；
//! - sedentary：连续会话 ≥50min 且输入空闲 ≤5min（>5min 判「离开」= 重置会话），
//!   提醒间隔可配置（默认 60min），一天 ≤6 次；媒体播放中豁免（看视频不算久坐）；
//! - night_care（默认关）：01:00–07:00 持续打字时的一条关怀，一夜一次；
//! - 勿扰 23:00–07:00：除 night_care 外全静音（B5 判据）。深夜 23:00 的「该休息」
//!   改走**状态表达**（岛变暗/dock 降饱和，见 ai::mod 的 night-state 事件），
//!   不再作为建议弹出。
//!
//! 防打扰：全局最小间隔（GLOBAL_GAP）、同类最小间隔（SAME_KIND_GAP）、每日标记
//! 三层闸门；一次性标记只在真正发出时置位（被闸门拦下时提前置位会永久吞掉提醒）。

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub cpu: f64,
    pub mem_pct: f64,
    pub battery: Option<f64>,
    pub charging: bool,
}

/// 注入的本地时钟读数。`day` = 本地日历日（自 Unix 纪元的天数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    pub hour: u32,
    pub minute: u32,
    pub day: u64,
}

impl Clock {
    /// 「按夜计」的 key（卡面：`local_date(now - 4h)`，04:00 为界）——
    /// 23:50 的提醒和次日 00:10 的提醒算同一夜，不会各来一次。
    pub fn night_key(&self) -> u64 {
        if (self.hour, self.minute) < (4, 0) {
            self.day.saturating_sub(1)
        } else {
            self.day
        }
    }

    /// 勿扰时段（默认 23:00–07:00）：主动建议全静音。
    pub fn in_dnd(&self) -> bool {
        self.hour >= 23 || self.hour < 7
    }
}

/// 注入的输入状态（Windows 侧由 `GetLastInputInfo` 派生）。
#[derive(Clone, Copy, Debug, Default)]
pub struct InputState {
    /// 距上次键盘/鼠标输入的空闲时长。
    pub idle: Duration,
}

/// 一次评估的全部注入量：时间、输入、环境快照、开关，一律由调用方给。
pub struct Ctx<'a> {
    pub snap: &'a Snapshot,
    /// UI 静音（收起球右键/设置面板）：连模板都不出。
    pub muted: bool,
    pub now: Instant,
    pub clock: Clock,
    pub input: InputState,
    /// 媒体播放中：看视频不算久坐（豁免）。
    pub media_playing: bool,
    /// 熬夜关怀（settings.nightCare，默认关）。
    pub night_care: bool,
    /// 久坐提醒间隔（settings.sedentaryIntervalMin，默认 60min）。
    pub sedentary_interval: Duration,
}

/// 一条建议：kind 用于前端归类与去重，text 是模板文案（LLM 只做异步润色）。
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub kind: &'static str,
    pub text: String,
}

const GLOBAL_GAP: Duration = Duration::from_secs(20 * 60);
const SAME_KIND_GAP: Duration = Duration::from_secs(6 * 60 * 60);
const CPU_HIGH: f64 = 85.0;
const CPU_SUSTAIN: Duration = Duration::from_secs(180);
const MEM_HIGH: f64 = 92.0;
const BATTERY_LOW: f64 = 20.0;

/// 日程提醒窗口：午饭 11:15–11:30、晚饭 17:15–17:30。
const LUNCH_WINDOW: (u32, u32) = (11, 15);
const DINNER_WINDOW: (u32, u32) = (17, 15);
/// 窗口长度（分钟）。前 5 分钟留给 LLM 预生成（见 ai::mod），触发在窗口余段。
const WINDOW_MINS: u32 = 15;
const PREGEN_MINS: u32 = 5;

/// 久坐：连续会话 ≥50min 且输入空闲 ≤5min 判「久坐」；>5min 判「离开」= 重置。
const SEDENTARY_SESSION: Duration = Duration::from_secs(50 * 60);
const INPUT_AWAY: Duration = Duration::from_secs(5 * 60);
/// 一天最多 6 次（卡面频率硬约束）。
const SEDENTARY_DAILY_CAP: u32 = 6;
/// 熬夜关怀：01:00 后仍在打字（输入空闲 <2min）才算「持续打字」。
const NIGHT_CARE_TYPING: Duration = Duration::from_secs(2 * 60);

pub fn minutes(h: u32, m: u32) -> u32 {
    h * 60 + m
}

/// 午饭提醒是否处于触发窗（11:20–11:30；11:15–11:20 是预生成窗）。
pub fn lunch_trigger(clock: Clock) -> bool {
    let t = minutes(clock.hour, clock.minute);
    let (h, m) = LUNCH_WINDOW;
    (t >= minutes(h, m) + PREGEN_MINS) && (t < minutes(h, m) + WINDOW_MINS)
}

/// 晚饭提醒是否处于触发窗（17:20–17:30）。
pub fn dinner_trigger(clock: Clock) -> bool {
    let t = minutes(clock.hour, clock.minute);
    let (h, m) = DINNER_WINDOW;
    (t >= minutes(h, m) + PREGEN_MINS) && (t < minutes(h, m) + WINDOW_MINS)
}

/// 日程类预生成窗（卡面：11:30 的午饭提醒在 11:15–11:20 先跑 LLM，延迟归零）。
/// 返回 Some(kind) 表示此刻该为哪条日程预生成。
pub fn meal_pregen(clock: Clock) -> Option<&'static str> {
    let t = minutes(clock.hour, clock.minute);
    let (lh, lm) = LUNCH_WINDOW;
    if t >= minutes(lh, lm) && t < minutes(lh, lm) + PREGEN_MINS {
        return Some("lunch");
    }
    let (dh, dm) = DINNER_WINDOW;
    if t >= minutes(dh, dm) && t < minutes(dh, dm) + PREGEN_MINS {
        return Some("dinner");
    }
    None
}

/// 日程类模板文案（预生成与触发共用同一句，保证「先算好再展示」语义）。
pub fn meal_template(kind: &str) -> &'static str {
    match kind {
        "lunch" => "快到中午了，可以准备吃午饭啦",
        "dinner" => "傍晚了，记得吃晚饭",
        _ => "",
    }
}

#[derive(Default)]
pub struct Suggester {
    last_emit: Option<Instant>,
    last_kind: HashMap<&'static str, Instant>,
    cpu_since: Option<Instant>,
    // 每日标记：rule → 已触发的本地日（check-then-set 与闸门在同一把 &mut self 内）。
    fired_day: HashMap<&'static str, u64>,
    // 久坐会话：None = 观察到「离开」（输入空闲 >5min），重新计时。
    session_since: Option<Instant>,
    sedentary_day: u64,
    sedentary_count: u32,
    // 熬夜关怀的「夜」标记（按夜不按天）。
    night_care_night: Option<u64>,
}

impl Suggester {
    /// 由调用方注入全部输入（B5 的注入式测试口）。
    pub fn evaluate(&mut self, ctx: &Ctx) -> Option<Suggestion> {
        // 久坐会话记账必须每次都跑（无论最终谁触发）：空闲 >5min 判「离开」。
        if ctx.input.idle > INPUT_AWAY {
            self.session_since = None;
        } else if self.session_since.is_none() {
            self.session_since = Some(ctx.now);
        }

        let suggestion = self.pick(ctx)?;
        if let Some(prev) = self.last_emit {
            if ctx.now.duration_since(prev) < GLOBAL_GAP {
                return None;
            }
        }
        if let Some(prev) = self.last_kind.get(suggestion.kind) {
            if ctx.now.duration_since(*prev) < SAME_KIND_GAP {
                return None;
            }
        }
        self.last_emit = Some(ctx.now);
        self.last_kind.insert(suggestion.kind, ctx.now);
        self.mark_fired(&suggestion, ctx);
        Some(suggestion)
    }

    fn mark_fired(&mut self, s: &Suggestion, ctx: &Ctx) {
        match s.kind {
            "lunch" | "dinner" => {
                self.fired_day.insert(s.kind, ctx.clock.day);
            }
            "sedentary" => {
                self.sedentary_count += 1;
                self.sedentary_day = ctx.clock.day;
            }
            "night_care" => {
                self.night_care_night = Some(ctx.clock.night_key());
            }
            _ => {}
        }
    }

    fn pick(&mut self, ctx: &Ctx) -> Option<Suggestion> {
        // 勿扰 23:00–07:00：全静音；唯一例外是显式开启的熬夜关怀（01:00–07:00）。
        if ctx.clock.in_dnd() {
            return self.pick_night_care(ctx);
        }
        // 深夜关怀在勿扰外不可达（它是深夜专属），daytime 到这就走普通规则。
        self.pick_daytime(ctx)
    }

    fn pick_daytime(&mut self, ctx: &Ctx) -> Option<Suggestion> {
        if let Some(b) = ctx.snap.battery.filter(|_| !ctx.snap.charging) {
            if b <= BATTERY_LOW {
                return Some(Suggestion {
                    kind: "battery",
                    text: format!("电量 {b:.0}%，该插电了"),
                });
            }
        }
        if let Some(since) = self.cpu_since {
            if ctx.now.duration_since(since) >= CPU_SUSTAIN {
                self.cpu_since = None; // 已提示，重新累计下一轮持续高负载
                return Some(Suggestion {
                    kind: "cpu",
                    text: format!(
                        "CPU 已持续 {} 分钟高负载",
                        ctx.now.duration_since(since).as_secs() / 60
                    ),
                });
            }
        }
        if ctx.snap.cpu >= CPU_HIGH {
            self.cpu_since.get_or_insert(ctx.now);
        } else {
            self.cpu_since = None;
        }
        if ctx.snap.mem_pct >= MEM_HIGH {
            return Some(Suggestion {
                kind: "memory",
                text: format!("内存已用 {:.0}%", ctx.snap.mem_pct),
            });
        }
        self.pick_schedule(ctx)
    }

    fn pick_schedule(&mut self, ctx: &Ctx) -> Option<Suggestion> {
        // 日程类：窗口内 + 当天未发过（每日标记在 mark_fired 统一置位）。
        if lunch_trigger(ctx.clock) && self.fired_day.get("lunch") != Some(&ctx.clock.day) {
            return Some(Suggestion {
                kind: "lunch",
                text: meal_template("lunch").to_string(),
            });
        }
        if dinner_trigger(ctx.clock) && self.fired_day.get("dinner") != Some(&ctx.clock.day) {
            return Some(Suggestion {
                kind: "dinner",
                text: meal_template("dinner").to_string(),
            });
        }
        self.pick_sedentary(ctx)
    }

    fn pick_sedentary(&mut self, ctx: &Ctx) -> Option<Suggestion> {
        // 「离开」（>5min 空闲）不判久坐；媒体播放豁免。
        if ctx.input.idle > INPUT_AWAY || ctx.media_playing {
            return None;
        }
        let Some(since) = self.session_since else {
            return None;
        };
        let elapsed = ctx.now.duration_since(since);
        if elapsed < SEDENTARY_SESSION {
            return None;
        }
        // 当日次数上限 + 提醒间隔
        if self.sedentary_day != ctx.clock.day {
            self.sedentary_count = 0;
        }
        if self.sedentary_count >= SEDENTARY_DAILY_CAP {
            return None;
        }
        if let Some(prev) = self.last_kind.get("sedentary") {
            if ctx.now.duration_since(*prev) < ctx.sedentary_interval {
                return None;
            }
        }
        Some(Suggestion {
            kind: "sedentary",
            text: format!("已连续 {} 分钟，起来活动一下吧", elapsed.as_secs() / 60),
        })
    }

    fn pick_night_care(&mut self, ctx: &Ctx) -> Option<Suggestion> {
        // 熬夜关怀（显式开启）：01:00–07:00 且仍在打字，一夜一次（按夜计）。
        if !ctx.night_care {
            return None;
        }
        let night = ctx.clock.night_key();
        if self.night_care_night == Some(night) {
            return None;
        }
        // 01:00 前（23:00–01:00）走状态表达，不出文案；01:00 后持续打字才提示。
        if ctx.clock.hour < 1 || ctx.input.idle > NIGHT_CARE_TYPING {
            return None;
        }
        Some(Suggestion {
            kind: "night_care",
            text: format!(
                "已经 {}:{}，夜深了，早点休息",
                ctx.clock.hour, ctx.clock.minute
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(
        snap: &Snapshot,
        now: Instant,
        hour: u32,
        minute: u32,
        day: u64,
        idle: Duration,
    ) -> Ctx<'_> {
        Ctx {
            snap,
            muted: false,
            now,
            clock: Clock { hour, minute, day },
            input: InputState { idle },
            media_playing: false,
            night_care: false,
            sedentary_interval: Duration::from_secs(60 * 60),
        }
    }

    #[test]
    fn battery_low_fires() {
        let mut s = Suggester::default();
        let snap = Snapshot {
            battery: Some(12.0),
            charging: false,
            ..Default::default()
        };
        let hit = s
            .evaluate(&ctx(&snap, Instant::now(), 14, 0, 20_000, Duration::ZERO))
            .expect("低电量应给建议");
        assert_eq!(hit.kind, "battery");
        assert!(hit.text.contains("12%"));
    }

    #[test]
    fn charging_battery_does_not_fire() {
        let mut s = Suggester::default();
        let snap = Snapshot {
            battery: Some(12.0),
            charging: true,
            ..Default::default()
        };
        assert!(s
            .evaluate(&ctx(&snap, Instant::now(), 14, 0, 20_000, Duration::ZERO))
            .is_none());
    }

    #[test]
    fn muted_suppresses_everything() {
        let mut s = Suggester::default();
        let snap = Snapshot {
            battery: Some(5.0),
            charging: false,
            ..Default::default()
        };
        let mut c = ctx(&snap, Instant::now(), 14, 0, 20_000, Duration::ZERO);
        c.muted = true;
        assert!(s.evaluate(&c).is_none());
    }

    #[test]
    fn global_gap_blocks_second_suggestion() {
        let t0 = Instant::now();
        let mut s = Suggester::default();
        let low = Snapshot {
            battery: Some(10.0),
            charging: false,
            ..Default::default()
        };
        let mem = Snapshot {
            battery: Some(90.0),
            mem_pct: 99.0,
            charging: true,
            ..Default::default()
        };
        assert!(s.evaluate(&ctx(&low, t0, 14, 0, 20_000, Duration::ZERO)).is_some());
        assert!(s.evaluate(&ctx(&mem, t0, 14, 0, 20_000, Duration::ZERO)).is_none());
    }

    // ---- B5：午饭窗口一天一次 -----------------------------------------------

    #[test]
    fn lunch_fires_once_per_day_and_window() {
        let mut s = Suggester::default();
        let snap = Snapshot::default();
        let t0 = Instant::now();
        // 窗口前（11:10）不触发
        assert!(s
            .evaluate(&ctx(&snap, t0, 11, 10, 20_000, Duration::ZERO))
            .is_none());
        // 预生成窗（11:16）不触发（触发在 11:20 之后）
        assert!(s
            .evaluate(&ctx(&snap, t0 + Duration::from_secs(6 * 60), 11, 16, 20_000, Duration::ZERO))
            .is_none());
        // 触发窗（11:21）触发一次
        let hit = s
            .evaluate(&ctx(&snap, t0 + Duration::from_secs(7 * 60), 11, 21, 20_000, Duration::ZERO))
            .expect("触发窗应给午饭建议");
        assert_eq!(hit.kind, "lunch");
        // 同窗口内第二次（11:25）不再触发
        assert!(s
            .evaluate(&ctx(&snap, t0 + Duration::from_secs(11 * 60), 11, 25, 20_000, Duration::ZERO))
            .is_none());
        // 第二天同窗口 → 再次触发（每日标记按天翻转）
        let hit2 = s
            .evaluate(&ctx(&snap, t0 + Duration::from_secs(86_400 + 7 * 60), 11, 21, 20_001, Duration::ZERO))
            .expect("第二天应再触发");
        assert_eq!(hit2.kind, "lunch");
    }

    #[test]
    fn dinner_fires_once_per_day() {
        let mut s = Suggester::default();
        let snap = Snapshot::default();
        let t0 = Instant::now();
        let hit = s
            .evaluate(&ctx(&snap, t0, 17, 22, 20_000, Duration::ZERO))
            .expect("晚饭触发窗");
        assert_eq!(hit.kind, "dinner");
        assert!(s
            .evaluate(&ctx(&snap, t0 + Duration::from_secs(300), 17, 27, 20_000, Duration::ZERO))
            .is_none());
    }

    // ---- B5：久坐 5min 空档重置 ---------------------------------------------

    #[test]
    fn sedentary_requires_50min_session_and_5min_gap_resets() {
        let mut s = Suggester::default();
        let snap = Snapshot::default();
        let t0 = Instant::now();
        let active = Duration::from_secs(30);
        // 会话 49 分钟：不触发
        assert!(s
            .evaluate(&ctx(&snap, t0 + Duration::from_secs(49 * 60), 14, 0, 20_000, active))
            .is_none());
        // 51 分钟：触发
        let hit = s
            .evaluate(&ctx(&snap, t0 + Duration::from_secs(51 * 60), 14, 1, 20_000, active))
            .expect("久坐应触发");
        assert_eq!(hit.kind, "sedentary");
        // 间隔未到（默认 60min）：即使会话继续也不触发
        assert!(s
            .evaluate(&ctx(&snap, t0 + Duration::from_secs(80 * 60), 14, 20, 20_000, active))
            .is_none());
        // 5 分钟空档（离开）→ 会话重置，回来后 30 分钟不触发
        assert!(s
            .evaluate(&ctx(&snap, t0 + Duration::from_secs(140 * 60), 16, 20, 20_000, Duration::from_secs(6 * 60)))
            .is_none());
        assert!(s
            .evaluate(&ctx(&snap, t0 + Duration::from_secs(140 * 60 + 30 * 60), 17, 20, 20_000, active))
            .is_none(), "重置后 30min 不算久坐");
        // 重置后再满 51min → 再次触发（间隔 60min 已过）
        let hit2 = s
            .evaluate(&ctx(&snap, t0 + Duration::from_secs(140 * 60 + 51 * 60), 17, 51, 20_000, active))
            .expect("重置后的新会话满 50min 应再触发");
        assert_eq!(hit2.kind, "sedentary");
    }

    #[test]
    fn sedentary_daily_cap_six() {
        let mut s = Suggester::default();
        let snap = Snapshot::default();
        let t0 = Instant::now();
        let mut n = 0;
        for i in 0..10u64 {
            let r = s.evaluate(&ctx(
                &snap,
                t0 + Duration::from_secs(60 * 60 * i + 51 * 60),
                8 + i as u32 % 12,
                0,
                20_000,
                Duration::from_secs(30),
            ));
            if r.is_some() {
                n += 1;
            }
        }
        assert_eq!(n, 6, "一天最多 6 次久坐提醒");
    }

    #[test]
    fn media_playing_exempts_sedentary() {
        let mut s = Suggester::default();
        let snap = Snapshot::default();
        let mut c = ctx(&snap, Instant::now(), 14, 30, 20_000, Duration::from_secs(55 * 60));
        c.media_playing = true;
        assert!(s.evaluate(&c).is_none(), "看视频不算久坐");
    }

    // ---- B5：勿扰时段全静音 ---------------------------------------------------

    #[test]
    fn dnd_window_silences_everything_except_night_care() {
        let mut s = Suggester::default();
        let low_batt = Snapshot {
            battery: Some(5.0),
            charging: false,
            ..Default::default()
        };
        // 23:00–07:00 内：电量/内存/日程全静音
        for (h, m) in [(23, 10), (0, 30), (3, 0), (6, 50)] {
            let c = ctx(&low_batt, Instant::now(), h, m, 20_000, Duration::ZERO);
            assert!(s.evaluate(&c).is_none(), "勿扰时段 {h}:{m} 不应触发");
        }
        // 熬夜关怀开启后：01:00+ 持续打字 → 允许一条
        let mut c = ctx(&low_batt, Instant::now(), 1, 30, 20_000, Duration::from_secs(60));
        c.night_care = true;
        let hit = s.evaluate(&c).expect("熬夜关怀应触发");
        assert_eq!(hit.kind, "night_care");
        // 一夜一次（23:50 与次日 00:10 同夜——night_key 按夜计）
        assert!(s
            .evaluate(&ctx(&low_batt, Instant::now(), 2, 0, 20_000, Duration::from_secs(60)))
            .is_none());
    }

    #[test]
    fn night_care_requires_typing_and_explicit_opt_in() {
        let mut s = Suggester::default();
        let snap = Snapshot::default();
        // 未开启：永不触发
        assert!(s
            .evaluate(&ctx(&snap, Instant::now(), 2, 0, 20_000, Duration::from_secs(60)))
            .is_none());
        // 开启但已离开（空闲 >2min）：不触发
        let mut c = ctx(&snap, Instant::now(), 2, 0, 20_000, Duration::from_secs(3 * 60));
        c.night_care = true;
        assert!(s.evaluate(&c).is_none());
        // 开启 + 打字中：触发
        let mut c2 = ctx(&snap, Instant::now(), 2, 0, 20_000, Duration::from_secs(60));
        c2.night_care = true;
        assert!(s.evaluate(&c2).is_some());
    }

    #[test]
    fn night_key_uses_4am_boundary() {
        let late = Clock { hour: 23, minute: 50, day: 100 };
        let early = Clock { hour: 0, minute: 10, day: 101 };
        assert_eq!(late.night_key(), early.night_key(), "23:50 与次日 00:10 同夜");
        let after4 = Clock { hour: 4, minute: 0, day: 101 };
        assert_ne!(after4.night_key(), late.night_key(), "04:00 起算新的一夜");
    }

    #[test]
    fn meal_windows_and_pregen_partition_the_window() {
        let day = 20_000;
        // 11:14 未到窗；11:16 预生成；11:21 触发；11:31 出窗
        assert!(meal_pregen(Clock { hour: 11, minute: 16, day }) == Some("lunch"));
        assert!(meal_pregen(Clock { hour: 11, minute: 21, day }).is_none());
        assert!(lunch_trigger(Clock { hour: 11, minute: 16, day }) == false);
        assert!(lunch_trigger(Clock { hour: 11, minute: 21, day }));
        assert!(!lunch_trigger(Clock { hour: 11, minute: 31, day }));
        assert!(meal_pregen(Clock { hour: 17, minute: 17, day }) == Some("dinner"));
        assert!(dinner_trigger(Clock { hour: 17, minute: 21, day }));
        assert!(meal_pregen(Clock { hour: 12, minute: 16, day }).is_none());
    }
}
