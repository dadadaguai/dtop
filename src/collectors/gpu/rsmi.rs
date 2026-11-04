//! FFI backend over `librocm_smi64.so` — the same SMI library `hy-smi` calls
//! internally, invoked here directly (in-process, no subprocess).
//!
//! Ground truth is the driver header shipped on DCU machines plus dcutop's
//! verified bindings (8x HYGON DCU-3G / BW1100, librocm_smi64.so.2.8):
//!
//! - `rsmi_frequencies_t` has 33 frequency slots (32 normal + 1 sleep); a
//!   32-entry buffer would let the driver write past its end (280 bytes).
//! - `rsmi_process_info_v2_t.vramUsageSize` is BYTES on this build (the header
//!   comment claims MiB, wrongly).
//! - `rsmi_dev_memory_usage_get` is the only way to get used VRAM.
//! - Units: temperature m°C, power µW, clocks Hz. Temp sensors: EDGE=0,
//!   JUNCTION=1, MEMORY=2, CORE=11. Clocks: SYS=0, MEM=4.
//! - `rsmi_dev_hcu_util_get`/`rsmi_dev_cu_util_get` BLOCK for `duration_ms`,
//!   so they are round-robined across devices (a few per tick).

use super::{GpuBackend, GpuProc, GpuProcDevice, GpuSample, GpuSnapshot};
use anyhow::{Context, Result};
use std::ffi::{c_char, CStr};
use std::sync::Mutex;

const RSMI_MAX_NUM_FREQUENCIES: usize = 33;
const MEM_VRAM: u32 = 0;
const TEMP_CURRENT: u32 = 0;
const TEMP_EDGE: u32 = 0;
const CLK_SYS: u32 = 0;
const CLK_MEM: u32 = 4;

type Status = i32;

#[repr(C)]
struct RsmiFrequencies {
    has_deep_sleep: bool,
    num_supported: u32,
    current: u32,
    frequency: [u64; RSMI_MAX_NUM_FREQUENCIES],
}

#[repr(C)]
struct RsmiProcessInfo {
    process_id: u32,
    pasid: u32,
    vram_usage: u64,
    sdma_usage: u64,
    cu_occupancy: u32,
}

#[repr(C)]
struct RsmiProcessInfoV2 {
    process_id: u32,
    vram_usage_size: u64, // BYTES on this driver build
    vram_usage_rate: f32,
    used_gpus: i32,
    gpu_index: [i32; 16],
    gpu_usage_rate: [f32; 16],
}

struct RsmiApi {
    init: unsafe extern "C" fn(u64) -> Status,
    shutdown: unsafe extern "C" fn() -> Status,
    status_string: unsafe extern "C" fn(i32, *mut *mut c_char) -> Status,
    num_devices: unsafe extern "C" fn(*mut u32) -> Status,
    dev_name_get: unsafe extern "C" fn(u32, *mut c_char, usize) -> Status,
    dev_pci_id_get: unsafe extern "C" fn(u32, *mut u64) -> Status,
    numa_node: unsafe extern "C" fn(u32, *mut u32) -> Status,
    cu_num: unsafe extern "C" fn(u32, *mut i32) -> Status,
    mem_total: unsafe extern "C" fn(u32, u32, *mut u64) -> Status,
    mem_usage: unsafe extern "C" fn(u32, u32, *mut u64) -> Status,
    busy_percent: unsafe extern "C" fn(u32, *mut u32) -> Status,
    power_get: unsafe extern "C" fn(u32, *mut u64, *mut i32) -> Status,
    power_ave_get: unsafe extern "C" fn(u32, u32, *mut u64) -> Status,
    power_cap_get: unsafe extern "C" fn(u32, u32, *mut u64) -> Status,
    temp_metric: unsafe extern "C" fn(u32, u32, u32, *mut i64) -> Status,
    clk_freq: unsafe extern "C" fn(u32, u32, *mut RsmiFrequencies) -> Status,
    hcu_util: unsafe extern "C" fn(u32, u32, *mut f32) -> Status,
    cu_util: unsafe extern "C" fn(u32, u32, *mut f32) -> Status,
    dev_proc_usage: unsafe extern "C" fn(u32, u32, *mut f32) -> Status,
    compute_process_info_get: unsafe extern "C" fn(*mut RsmiProcessInfo, *mut u32) -> Status,
    compute_process_info_by_pid_v2: unsafe extern "C" fn(u32, *mut RsmiProcessInfoV2) -> Status,
    compute_process_info_by_device: unsafe extern "C" fn(u32, u32, *mut RsmiProcessInfo) -> Status,
}

impl RsmiApi {
    fn load(lib: &libloading::Library) -> Result<Self> {
        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                let s: libloading::Symbol<$ty> =
                    unsafe { lib.get($name) }
                        .with_context(|| format!("missing symbol {}", String::from_utf8_lossy($name)))?;
                *s
            }};
        }
        Ok(Self {
            init: sym!(b"rsmi_init", unsafe extern "C" fn(u64) -> Status),
            shutdown: sym!(b"rsmi_shut_down", unsafe extern "C" fn() -> Status),
            status_string: sym!(b"rsmi_status_string", unsafe extern "C" fn(i32, *mut *mut c_char) -> Status),
            num_devices: sym!(b"rsmi_num_monitor_devices", unsafe extern "C" fn(*mut u32) -> Status),
            dev_name_get: sym!(b"rsmi_dev_name_get", unsafe extern "C" fn(u32, *mut c_char, usize) -> Status),
            dev_pci_id_get: sym!(b"rsmi_dev_pci_id_get", unsafe extern "C" fn(u32, *mut u64) -> Status),
            numa_node: sym!(b"rsmi_topo_get_numa_node_number", unsafe extern "C" fn(u32, *mut u32) -> Status),
            cu_num: sym!(b"rsmi_dev_cu_num_get", unsafe extern "C" fn(u32, *mut i32) -> Status),
            mem_total: sym!(b"rsmi_dev_memory_total_get", unsafe extern "C" fn(u32, u32, *mut u64) -> Status),
            mem_usage: sym!(b"rsmi_dev_memory_usage_get", unsafe extern "C" fn(u32, u32, *mut u64) -> Status),
            busy_percent: sym!(b"rsmi_dev_busy_percent_get", unsafe extern "C" fn(u32, *mut u32) -> Status),
            power_get: sym!(b"rsmi_dev_power_get", unsafe extern "C" fn(u32, *mut u64, *mut i32) -> Status),
            power_ave_get: sym!(b"rsmi_dev_power_ave_get", unsafe extern "C" fn(u32, u32, *mut u64) -> Status),
            power_cap_get: sym!(b"rsmi_dev_power_cap_get", unsafe extern "C" fn(u32, u32, *mut u64) -> Status),
            temp_metric: sym!(b"rsmi_dev_temp_metric_get", unsafe extern "C" fn(u32, u32, u32, *mut i64) -> Status),
            clk_freq: sym!(b"rsmi_dev_gpu_clk_freq_get", unsafe extern "C" fn(u32, u32, *mut RsmiFrequencies) -> Status),
            hcu_util: sym!(b"rsmi_dev_hcu_util_get", unsafe extern "C" fn(u32, u32, *mut f32) -> Status),
            cu_util: sym!(b"rsmi_dev_cu_util_get", unsafe extern "C" fn(u32, u32, *mut f32) -> Status),
            dev_proc_usage: sym!(b"rsmi_dev_proc_usage_get", unsafe extern "C" fn(u32, u32, *mut f32) -> Status),
            compute_process_info_get: sym!(b"rsmi_compute_process_info_get", unsafe extern "C" fn(*mut RsmiProcessInfo, *mut u32) -> Status),
            compute_process_info_by_pid_v2: sym!(b"rsmi_compute_process_info_by_pid_get_v2", unsafe extern "C" fn(u32, *mut RsmiProcessInfoV2) -> Status),
            compute_process_info_by_device: sym!(b"rsmi_compute_process_info_by_device_get", unsafe extern "C" fn(u32, u32, *mut RsmiProcessInfo) -> Status),
        })
    }

    fn status_string(&self, status: i32) -> String {
        let mut out: *mut c_char = std::ptr::null_mut();
        unsafe { (self.status_string)(status, &mut out) };
        if out.is_null() {
            return format!("unknown rsmi status {status}");
        }
        unsafe { CStr::from_ptr(out) }.to_string_lossy().into_owned()
    }

    fn init(&self) -> Result<()> {
        let s = unsafe { (self.init)(0) };
        if s != 0 {
            anyhow::bail!("rsmi_init: {}", self.status_string(s));
        }
        Ok(())
    }

    fn num_devices(&self) -> Result<usize> {
        let mut n: u32 = 0;
        let s = unsafe { (self.num_devices)(&mut n) };
        if s != 0 {
            anyhow::bail!("rsmi_num_monitor_devices: {}", self.status_string(s));
        }
        Ok(n as usize)
    }

    fn device_name(&self, idx: u32) -> Option<String> {
        let mut buf = vec![0u8; 256];
        let s = unsafe { (self.dev_name_get)(idx, buf.as_mut_ptr() as *mut c_char, 256) };
        if s != 0 {
            return None;
        }
        let c = unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) };
        Some(c.to_string_lossy().into_owned())
    }

    fn pci_bus_id(&self, idx: u32) -> Option<String> {
        let mut bdfid: u64 = 0;
        let s = unsafe { (self.dev_pci_id_get)(idx, &mut bdfid) };
        if s != 0 {
            return None;
        }
        let domain = (bdfid >> 32) & 0xFFFF_FFFF;
        let bus = (bdfid >> 8) & 0xFF;
        let device = (bdfid >> 3) & 0x1F;
        let function = bdfid & 0x7;
        Some(format!("{domain:04x}:{bus:02x}:{device:02x}.{function:x}"))
    }

    fn numa_node(&self, idx: u32) -> Option<u32> {
        let mut node: u32 = 0;
        let s = unsafe { (self.numa_node)(idx, &mut node) };
        if s != 0 {
            None
        } else {
            Some(node)
        }
    }

    fn cu_num(&self, idx: u32) -> Option<u32> {
        let mut n: i32 = 0;
        let s = unsafe { (self.cu_num)(idx, &mut n) };
        if s != 0 {
            None
        } else {
            Some(n as u32)
        }
    }

    fn memory_total(&self, idx: u32) -> Option<f64> {
        let mut v: u64 = 0;
        let s = unsafe { (self.mem_total)(idx, MEM_VRAM, &mut v) };
        if s != 0 {
            None
        } else {
            Some(v as f64 / 1024.0 / 1024.0)
        }
    }

    fn memory_used(&self, idx: u32) -> Option<f64> {
        let mut v: u64 = 0;
        let s = unsafe { (self.mem_usage)(idx, MEM_VRAM, &mut v) };
        if s != 0 {
            None
        } else {
            Some(v as f64 / 1024.0 / 1024.0)
        }
    }

    fn busy_percent(&self, idx: u32) -> Option<f64> {
        let mut v: u32 = 0;
        let s = unsafe { (self.busy_percent)(idx, &mut v) };
        if s != 0 {
            None
        } else {
            Some(v as f64)
        }
    }

    fn power(&self, idx: u32) -> Option<f64> {
        let mut watts: u64 = 0;
        let mut ptype: i32 = -1;
        let s = unsafe { (self.power_get)(idx, &mut watts, &mut ptype) };
        if s != 0 {
            // older path: average power only
            let mut w: u64 = 0;
            let s2 = unsafe { (self.power_ave_get)(idx, 0, &mut w) };
            if s2 != 0 {
                return None;
            }
            Some(w as f64 / 1_000_000.0)
        } else {
            Some(watts as f64 / 1_000_000.0)
        }
    }

    fn power_cap(&self, idx: u32) -> Option<f64> {
        let mut cap: u64 = 0;
        let s = unsafe { (self.power_cap_get)(idx, 0, &mut cap) };
        if s != 0 {
            None
        } else {
            Some(cap as f64 / 1_000_000.0)
        }
    }

    fn temperature(&self, idx: u32, sensor: u32) -> Option<f64> {
        let mut mdeg: i64 = 0;
        let s = unsafe { (self.temp_metric)(idx, sensor, TEMP_CURRENT, &mut mdeg) };
        if s != 0 {
            None
        } else {
            Some(mdeg as f64 / 1000.0)
        }
    }

    fn clk_mhz(&self, idx: u32, clk_type: u32) -> Option<u64> {
        let mut freqs = RsmiFrequencies {
            has_deep_sleep: false,
            num_supported: 0,
            current: 0,
            frequency: [0; RSMI_MAX_NUM_FREQUENCIES],
        };
        let s = unsafe { (self.clk_freq)(idx, clk_type, &mut freqs) };
        if s != 0 || freqs.has_deep_sleep || freqs.num_supported == 0 {
            return None;
        }
        let cur = freqs.current as usize;
        if cur >= freqs.num_supported as usize || cur >= freqs.frequency.len() {
            return None;
        }
        Some(freqs.frequency[cur] / 1_000_000)
    }

    /// Blocking: samples the window for `duration_ms` milliseconds.
    fn hcu_util(&self, idx: u32, duration_ms: u32) -> Option<f64> {
        let mut pct: f32 = 0.0;
        let s = unsafe { (self.hcu_util)(idx, duration_ms, &mut pct) };
        if s != 0 {
            None
        } else {
            Some(pct as f64)
        }
    }

    fn cu_util(&self, idx: u32, duration_ms: u32) -> Option<f64> {
        let mut pct: f32 = 0.0;
        let s = unsafe { (self.cu_util)(idx, duration_ms, &mut pct) };
        if s != 0 {
            None
        } else {
            Some(pct as f64)
        }
    }

    fn dev_proc_usage(&self, pid: u32, idx: u32) -> Option<f64> {
        let mut pct: f32 = 0.0;
        let s = unsafe { (self.dev_proc_usage)(pid, idx, &mut pct) };
        if s != 0 {
            None
        } else {
            Some(pct as f64)
        }
    }

    fn compute_process_pids(&self) -> Vec<u32> {
        let mut count: u32 = 0;
        let s = unsafe { (self.compute_process_info_get)(std::ptr::null_mut(), &mut count) };
        if s != 0 || count == 0 {
            return Vec::new();
        }
        let mut array: Vec<RsmiProcessInfo> = (0..count)
            .map(|_| RsmiProcessInfo {
                process_id: 0,
                pasid: 0,
                vram_usage: 0,
                sdma_usage: 0,
                cu_occupancy: 0,
            })
            .collect();
        let s = unsafe { (self.compute_process_info_get)(array.as_mut_ptr(), &mut count) };
        if s != 0 {
            return Vec::new();
        }
        array
            .iter()
            .take(count as usize)
            .map(|p| p.process_id)
            .collect()
    }

    fn compute_process_info_v2(&self, pid: u32) -> Option<(u64, Vec<(usize, f32)>)> {
        let mut info = RsmiProcessInfoV2 {
            process_id: 0,
            vram_usage_size: 0,
            vram_usage_rate: 0.0,
            used_gpus: 0,
            gpu_index: [0; 16],
            gpu_usage_rate: [0.0; 16],
        };
        let s = unsafe { (self.compute_process_info_by_pid_v2)(pid, &mut info) };
        if s != 0 {
            return None;
        }
        let used = info.used_gpus.max(0) as usize;
        let gpus: Vec<(usize, f32)> = (0..used.min(info.gpu_index.len()))
            .map(|i| (info.gpu_index[i].max(0) as usize, info.gpu_usage_rate[i]))
            .collect();
        Some((info.vram_usage_size, gpus))
    }

    fn process_info_by_device(&self, pid: u32, idx: u32) -> Option<(f64, f64)> {
        // returns (vram_bytes, cu_occupancy)
        let mut info = RsmiProcessInfo {
            process_id: 0,
            pasid: 0,
            vram_usage: 0,
            sdma_usage: 0,
            cu_occupancy: 0,
        };
        let s = unsafe { (self.compute_process_info_by_device)(pid, idx, &mut info) };
        if s != 0 {
            return None;
        }
        Some((
            info.vram_usage as f64 / 1024.0 / 1024.0,
            info.cu_occupancy as f64,
        ))
    }
}

struct RsmiInner {
    _lib: Option<libloading::Library>,
    api: Option<RsmiApi>,
    error: Option<String>,
    count: usize,
    static_info: Vec<GpuSample>,
    /// last windowed (hcu, cu) per device; refreshed round-robin
    window: Vec<Option<(f64, f64)>>,
    rotation: usize,
    window_ms: u32,
    window_per_tick: usize,
}

impl Default for RsmiInner {
    fn default() -> Self {
        Self {
            _lib: None,
            api: None,
            error: None,
            count: 0,
            static_info: Vec::new(),
            window: Vec::new(),
            rotation: 0,
            window_ms: 150,
            window_per_tick: 2,
        }
    }
}

impl Drop for RsmiInner {
    fn drop(&mut self) {
        if let Some(api) = &self.api {
            unsafe { (api.shutdown)() };
        }
    }
}

impl RsmiInner {
    fn load(window_ms: u32, window_per_tick: usize) -> Self {
        let mut inner = RsmiInner {
            _lib: None,
            api: None,
            error: None,
            count: 0,
            static_info: Vec::new(),
            window: Vec::new(),
            rotation: 0,
            window_ms,
            window_per_tick,
        };
        let result = (|| -> Result<(libloading::Library, RsmiApi)> {
            let lib = unsafe { libloading::Library::new(find_library()?) }
                .context("failed to load librocm_smi64.so")?;
            let api = RsmiApi::load(&lib)?;
            api.init()?;
            Ok((lib, api))
        })();
        match result {
            Ok((lib, api)) => {
                inner.count = api.num_devices().unwrap_or(0);
                inner.static_info = (0..inner.count)
                    .map(|i| read_static(&api, i))
                    .collect();
                inner.window = vec![None; inner.count];
                inner._lib = Some(lib);
                inner.api = Some(api);
            }
            Err(e) => {
                inner.error = Some(format!("DCU driver unavailable: {e:#}"));
            }
        }
        inner
    }

    fn collect(&mut self) -> GpuSnapshot {
        let backend = "rsmi".to_string();
        if let Some(err) = self.error.clone() {
            return GpuSnapshot {
                backend,
                gpus: vec![],
                procs: vec![],
                error: Some(err),
            };
        }
        let api = match &self.api {
            Some(a) => a,
            None => {
                return GpuSnapshot {
                    backend,
                    error: Some("rsmi backend not initialized".into()),
                    ..Default::default()
                }
            }
        };

        // instant metrics, one pass over all devices
        let mut gpus: Vec<GpuSample> = self
            .static_info
            .iter()
            .cloned()
            .map(|mut g| {
                let idx = g.index as u32;
                g.util = api.busy_percent(idx);
                g.cu_util = None; // filled from window below
                g.vram_used_mb = api.memory_used(idx);
                g.vram_total_mb = api.memory_total(idx).or(g.vram_total_mb);
                g.temp_c = api.temperature(idx, TEMP_EDGE);
                g.power_w = api.power(idx);
                g.power_cap_w = api.power_cap(idx);
                g.sclk_mhz = api.clk_mhz(idx, CLK_SYS);
                g.mclk_mhz = api.clk_mhz(idx, CLK_MEM);
                g
            })
            .collect();

        // rotate the blocking HCU%/CU% window across devices
        if self.count > 0 {
            for _ in 0..self.window_per_tick {
                let idx = self.rotation % self.count;
                self.rotation += 1;
                let hcu = api.hcu_util(idx as u32, self.window_ms);
                let cu = api.cu_util(idx as u32, self.window_ms);
                if hcu.is_some() || cu.is_some() {
                    self.window[idx] = Some((hcu.unwrap_or(0.0), cu.unwrap_or(0.0)));
                }
            }
        }
        for g in gpus.iter_mut() {
            if let Some((hcu, cu)) = self.window.get(g.index).copied().flatten() {
                g.util = Some(hcu);
                g.cu_util = Some(cu);
            }
        }

        let procs = collect_processes(api);
        GpuSnapshot {
            backend,
            gpus,
            procs,
            error: None,
        }
    }
}

fn find_library() -> Result<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("DCUTOP_LIBRARY_PATH") {
        dirs.extend(p.split(':').filter(|s| !s.is_empty()).map(Into::into));
    }
    dirs.push("/opt/hyhal/lib".into());
    dirs.push("/opt/dtk/lib".into());
    let names = ["librocm_smi64.so"];
    for d in &dirs {
        for n in names {
            let p = d.join(n);
            if p.exists() {
                return Ok(p);
            }
        }
    }
    // last resort: bare soname via the dynamic linker
    for n in names {
        let bare = std::path::PathBuf::from(n);
        if unsafe { libloading::Library::new(&bare) }.is_ok() {
            return Ok(bare);
        }
    }
    anyhow::bail!(
        "librocm_smi64.so not found; searched {}; \
         check the hyhal/DTK driver install, or set DCUTOP_LIBRARY_PATH",
        dirs
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn read_static(api: &RsmiApi, index: usize) -> GpuSample {
    let idx = index as u32;
    GpuSample {
        index,
        name: api.device_name(idx).unwrap_or_default(),
        pci_bus_id: api.pci_bus_id(idx).unwrap_or_default(),
        numa_node: api.numa_node(idx),
        cu_count: api.cu_num(idx),
        vram_total_mb: api.memory_total(idx),
        ..Default::default()
    }
}

fn collect_processes(api: &RsmiApi) -> Vec<GpuProc> {
    let mut out = Vec::new();
    for pid in api.compute_process_pids() {
        let mut proc = GpuProc {
            pid,
            name: String::new(),
            devices: Vec::new(),
        };
        if let Some((_vram_bytes, gpus)) = api.compute_process_info_v2(pid) {
            for (dev, _rate) in gpus {
                let mut d = GpuProcDevice {
                    device: dev,
                    vram_mb: None,
                    cu_pct: None,
                };
                if let Some((vram_mb, cu)) = api.process_info_by_device(pid, dev as u32) {
                    d.vram_mb = Some(vram_mb);
                    d.cu_pct = Some(cu);
                }
                if let Some(cu) = api.dev_proc_usage(pid, dev as u32) {
                    d.cu_pct = Some(cu);
                }
                proc.devices.push(d);
            }
        }
        out.push(proc);
    }
    out
}

pub struct RsmiBackend {
    inner: Mutex<RsmiInner>,
}

impl RsmiBackend {
    pub fn new(window_ms: u32) -> Self {
        let inner = RsmiInner::load(window_ms, 2);
        Self {
            inner: Mutex::new(inner),
        }
    }
}

impl GpuBackend for RsmiBackend {
    fn name(&self) -> &str {
        "rsmi"
    }

    fn collect(&self) -> Result<GpuSnapshot> {
        let mut inner = self.inner.lock().unwrap();
        Ok(inner.collect())
    }
}
