//! 电源状态 —— 电池电量与是否在充电。
//!
//! 用 `GetSystemPowerStatus`（毫秒级，无依赖）。sysinfo 0.33 没有跨平台的
//! "portable batteries" 接口，台式机上本来也读不到电池，返回 `None` 即可。

use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

/// 电池读数。`None` = 没有电池（台式机）或读数不可用。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Battery {
    /// 电量百分比（0-100）。
    pub charge: f64,
    /// 是否接入交流电（充电中/已充满）。
    pub charging: bool,
}

/// 读一次电池状态。
pub fn battery() -> Option<Battery> {
    let mut sps = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut sps) }.is_err() {
        return None;
    }
    // `BatteryLifePercent`: 0-100，255 = 未知（无电池）。
    if sps.BatteryLifePercent == 255 {
        return None;
    }
    Some(Battery {
        charge: sps.BatteryLifePercent as f64,
        charging: sps.ACLineStatus == 1,
    })
}
