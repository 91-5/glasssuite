//! GPU 使用率采样 —— 直连 NVML（NVIDIA 驱动自带的 `nvml.dll`）。
//!
//! 为什么不用 `nvidia-smi`：每次调用要 spawn 一个进程（实测 200-500ms），
//! 1s 轮询下开销可观；NVML 是同一份驱动接口的直接 FFI 调用，微秒级。
//!
//! 为什么不用 PDH 的 "GPU Engine" 计数器：需要跨多个引擎聚合，口径复杂，
//! 且包含 3D/Copy/VideoDecode 等子引擎，拿不到与任务管理器一致的单一百分比。
//!
//! 设计：动态加载 `nvml.dll`（`LoadLibraryW` + `GetProcAddress`），因此没有 NVIDIA
//! 显卡或驱动缺失时不会导致程序启动失败 —— 初始化失败就返回 `None`，界面显示 "—"。
//! 这也意味着**不引入 NVML 的链接期依赖**，AMD/Intel 机器上同样能启动。

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

/// `nvmlUtilization_t`：{ gpu, memory }，单位都是百分比。
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct NvmlUtilization {
    gpu: u32,
    memory: u32,
}

type FnInit = unsafe extern "C" fn() -> i32;
type FnGetHandle = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
type FnGetUtil = unsafe extern "C" fn(*mut c_void, *mut NvmlUtilization) -> i32;
type FnShutdown = unsafe extern "C" fn() -> i32;

/// NVML 句柄集合。只应在单一线程内使用（采样线程）。
pub struct GpuProbe {
    get_util: FnGetUtil,
    shutdown: Option<FnShutdown>,
    device: *mut c_void,
    /// 上一次成功读到的值（读取偶发失败时沿用，避免曲线断点）。
    last: Option<u32>,
}

// 采样线程独占持有，跨线程不共享（raw pointer 不实现 Send，这里显式声明仅在线程内使用）。
unsafe impl Send for GpuProbe {}

impl GpuProbe {
    /// 初始化：加载 `nvml.dll` 并拿到 0 号设备句柄。
    /// 任何一步失败都返回 `None`（无 NVIDIA 卡 / 驱动缺失 / 被安全软件拦截）。
    pub fn init() -> Option<Self> {
        unsafe {
            let dll_name: Vec<u16> = "nvml.dll\0".encode_utf16().collect();
            let module = LoadLibraryW(PCWSTR(dll_name.as_ptr())).ok()?;

            let init: FnInit =
                std::mem::transmute(GetProcAddress(module, windows::core::s!("nvmlInit_v2"))?);
            let get_handle: FnGetHandle = std::mem::transmute(GetProcAddress(
                module,
                windows::core::s!("nvmlDeviceGetHandleByIndex_v2"),
            )?);
            let get_util: FnGetUtil = std::mem::transmute(GetProcAddress(
                module,
                windows::core::s!("nvmlDeviceGetUtilizationRates"),
            )?);
            let shutdown: Option<FnShutdown> =
                GetProcAddress(module, windows::core::s!("nvmlShutdown"))
                    .map(|p| std::mem::transmute(p));

            if init() != 0 {
                crate::log::log("NVML: init failed");
                return None;
            }

            let mut device: *mut c_void = std::ptr::null_mut();
            if get_handle(0, &mut device) != 0 || device.is_null() {
                crate::log::log("NVML: no device handle");
                if let Some(sd) = shutdown {
                    let _ = sd();
                }
                return None;
            }

            crate::log::log("NVML: GPU probe ready");
            Some(Self {
                get_util,
                shutdown,
                device,
                last: None,
            })
        }
    }

    /// 读一次 GPU 使用率（0-100）。读取失败时沿用上一次值。
    pub fn utilization(&mut self) -> Option<u32> {
        let mut util = NvmlUtilization::default();
        let rc = unsafe { (self.get_util)(self.device, &mut util) };
        if rc == 0 {
            let value = util.gpu.min(100);
            self.last = Some(value);
            self.last
        } else {
            self.last
        }
    }
}

impl Drop for GpuProbe {
    fn drop(&mut self) {
        if let Some(sd) = self.shutdown {
            unsafe {
                let _ = sd();
            }
        }
    }
}
