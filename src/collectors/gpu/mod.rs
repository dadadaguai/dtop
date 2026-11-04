pub mod mock;
pub mod rsmi;

use anyhow::Result;
use serde::Serialize;

/// Per-device instantaneous metrics. All optional: a failed driver read
/// becomes `None` (rendered "N/A") instead of crashing the monitor.
#[derive(Clone, Debug, Default, Serialize)]
pub struct GpuSample {
    pub index: usize,
    pub name: String,
    /// whole-card utilization (HCU%), 0..100
    pub util: Option<f64>,
    /// compute-unit utilization (CU%), 0..100
    pub cu_util: Option<f64>,
    pub vram_used_mb: Option<f64>,
    pub vram_total_mb: Option<f64>,
    pub temp_c: Option<f64>,
    pub power_w: Option<f64>,
    pub power_cap_w: Option<f64>,
    pub sclk_mhz: Option<u64>,
    pub mclk_mhz: Option<u64>,
    /// video decode engine utilization (%)
    pub dec_pct: Option<f64>,
    /// video encode engine utilization (%)
    pub enc_pct: Option<f64>,
    pub perf_level: Option<String>,
    pub mode: Option<String>,
    pub fan_pct: Option<f64>,
    pub fan_rpm: Option<u64>,
    pub pcie_tx_mbps: Option<f64>,
    pub pcie_rx_mbps: Option<f64>,
    /// static identity (from the driver, fetched once)
    pub pci_bus_id: String,
    pub numa_node: Option<u32>,
    pub cu_count: Option<u32>,
}

/// One process's footprint on one device.
#[derive(Clone, Debug, Default, Serialize)]
pub struct GpuProcDevice {
    pub device: usize,
    pub vram_mb: Option<f64>,
    pub cu_pct: Option<f64>,
}

/// A process known to the driver. A process may use several devices.
#[derive(Clone, Debug, Default, Serialize)]
pub struct GpuProc {
    pub pid: u32,
    pub name: String,
    pub devices: Vec<GpuProcDevice>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GpuSnapshot {
    pub backend: String,
    pub gpus: Vec<GpuSample>,
    pub procs: Vec<GpuProc>,
    pub error: Option<String>,
}

pub trait GpuBackend: Send + Sync {
    fn name(&self) -> &str;
    fn collect(&self) -> Result<GpuSnapshot>;
}

/// Select the backend. The only real data source is the librocm_smi64 FFI
/// (same library `hy-smi` calls internally); `mock` is for machines without
/// hardware and for development.
pub fn detect(pref: &str, window_ms: u32) -> Box<dyn GpuBackend> {
    match pref {
        "mock" => Box::new(mock::MockBackend::new(8)),
        _ => Box::new(rsmi::RsmiBackend::new(window_ms)),
    }
}
