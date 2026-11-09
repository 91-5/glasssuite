//! 主动建议 —— **纯本地硬规则，不调 API、不联网**。
//!
//! 从参考仓 `glasspill/src-tauri/src/ai/suggest.rs` 收敛：去掉对 `SharedState` 与
//! `GetLocalTime` 的直接依赖，改为「时钟与快照由调用方注入」，这样规则本身可在任何
//! 平台单测，也不必在规则层碰 Win32。
//!
//! 防打扰靠间隔：全局 5 分钟、同类 20 分钟；免打扰时不产生任何建议。

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub cpu: f64,
    pub mem_pct: f64,
    pub battery: Option<f64>,
    pub charging: bool,
}

const GLOBAL_GAP: Duration = Duration::from_secs(5 * 60);
const SAME_KIND_GAP: Duration = Duration::from_secs(20 * 60);
const CPU_HIGH: f64 = 85.0;
const CPU_SUSTAIN: Duration = Duration::from_secs(180);
const MEM_HIGH: f64 = 92.0;
const BATTERY_LOW: f64 = 20.0;

#[derive(Default)]
pub struct Suggester {
    last_emit: Option<Instant>,
    last_kind: HashMap<&'static str, Instant>,
    cpu_since: Option<Instant>,
    night_day: Option<u64>,
    day: u64,
}

impl Suggester {
    /// 由调用方注入 `now` 与本地 (时, 分)，便于固定时钟单测。
    pub fn evaluate(
        &mut self,
        snap: &Snapshot,
        muted: bool,
        now: Instant,
        hm: Option<(u32, u32)>,
    ) -> Option<(&'static str, String)> {
        if muted {
            return None;
        }
        self.roll_day();
        if snap.cpu >= CPU_HIGH {
            self.cpu_since.get_or_insert(now);
        } else {
            self.cpu_since = None;
        }

        let (kind, text) = self.pick(snap, hm, now)?;
        if let Some(prev) = self.last_emit {
            if now.duration_since(prev) < GLOBAL_GAP {
                return None;
            }
        }
        if let Some(prev) = self.last_kind.get(kind) {
            if now.duration_since(*prev) < SAME_KIND_GAP {
                return None;
            }
        }
        self.last_emit = Some(now);
        self.last_kind.insert(kind, now);
        // 一次性标记只在真正发出时置位：被闸门拦下时若提前置位会永久吞掉提醒。
        if kind == "night" {
            self.night_day = Some(self.day);
        }
        Some((kind, text))
    }

    fn pick(
        &self,
        snap: &Snapshot,
        hm: Option<(u32, u32)>,
        now: Instant,
    ) -> Option<(&'static str, String)> {
        if let Some(b) = snap.battery.filter(|_| !snap.charging) {
            if b <= BATTERY_LOW {
                return Some(("battery", format!("电量 {b:.0}%，该插电了")));
            }
        }
        if let Some(since) = self.cpu_since {
            if now.duration_since(since) >= CPU_SUSTAIN {
                return Some((
                    "cpu",
                    format!(
                        "CPU 已持续 {} 分钟高负载",
                        now.duration_since(since).as_secs() / 60
                    ),
                ));
            }
        }
        if snap.mem_pct >= MEM_HIGH {
            return Some(("memory", format!("内存已用 {:.0}%", snap.mem_pct)));
        }
        if let Some((hour, minute)) = hm {
            let is_night = !(6..23).contains(&hour);
            if is_night && self.night_day != Some(self.day) {
                return Some(("night", format!("已经 {hour}:{minute:02}，早点休息")));
            }
        }
        None
    }

    fn roll_day(&mut self) {
        let today = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() / 86_400)
            .unwrap_or(0);
        self.day = today;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(s: &mut Suggester, snap: &Snapshot, now: Instant) -> Option<(&'static str, String)> {
        s.evaluate(snap, false, now, None)
    }

    #[test]
    fn battery_low_fires() {
        let mut s = Suggester::default();
        let hit = eval(
            &mut s,
            &Snapshot {
                battery: Some(12.0),
                charging: false,
                ..Default::default()
            },
            Instant::now(),
        )
        .expect("低电量应给建议");
        assert_eq!(hit.0, "battery");
        assert!(hit.1.contains("12%"));
    }

    #[test]
    fn charging_battery_does_not_fire() {
        let mut s = Suggester::default();
        assert!(eval(
            &mut s,
            &Snapshot {
                battery: Some(12.0),
                charging: true,
                ..Default::default()
            },
            Instant::now(),
        )
        .is_none());
    }

    #[test]
    fn muted_suppresses_everything() {
        let mut s = Suggester::default();
        assert!(s
            .evaluate(
                &Snapshot {
                    battery: Some(5.0),
                    charging: false,
                    ..Default::default()
                },
                true,
                Instant::now(),
                None,
            )
            .is_none());
    }

    #[test]
    fn global_gap_blocks_second_suggestion() {
        let t0 = Instant::now();
        let mut s = Suggester::default();
        assert!(eval(
            &mut s,
            &Snapshot {
                battery: Some(10.0),
                charging: false,
                ..Default::default()
            },
            t0,
        )
        .is_some());
        assert!(eval(
            &mut s,
            &Snapshot {
                mem_pct: 99.0,
                battery: Some(90.0),
                charging: true,
                ..Default::default()
            },
            t0,
        )
        .is_none());
    }

    #[test]
    fn night_rule_respects_injected_clock() {
        let mut s = Suggester::default();
        let snap = Snapshot::default();
        let hit = s
            .evaluate(&snap, false, Instant::now(), Some((2, 1)))
            .expect("深夜规则");
        assert_eq!(hit.0, "night");
        assert!(s
            .evaluate(&snap, false, Instant::now(), Some((14, 0)))
            .is_none());
    }

    #[test]
    fn no_daily_cap_after_cooldown() {
        let t0 = Instant::now();
        let mut s = Suggester::default();
        for i in 0..6u64 {
            let snap = if i % 2 == 0 {
                Snapshot {
                    battery: Some(10.0),
                    charging: false,
                    ..Default::default()
                }
            } else {
                Snapshot {
                    mem_pct: 99.0,
                    battery: Some(90.0),
                    charging: true,
                    ..Default::default()
                }
            };
            assert!(eval(&mut s, &snap, t0 + Duration::from_secs(i * 21 * 60)).is_some());
        }
    }
}
