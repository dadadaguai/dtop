pub mod mock;
pub mod smi;

use anyhow::Result;
use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
pub struct GpuSample {
    pub index: usize,
    pub name: String,
    pub util: Option<f64>,
    pub vram_used_mb: Option<f64>,
    pub vram_total_mb: Option<f64>,
    pub temp_c: Option<f64>,
    pub power_w: Option<f64>,
    pub sclk_mhz: Option<u64>,
    pub mclk_mhz: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GpuProc {
    pub pid: u32,
    pub name: String,
    pub vram_mb: Option<f64>,
    pub gpu_ids: Vec<usize>,
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

fn which(cmd: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let full = dir.join(cmd);
                full.metadata()
                    .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

pub fn detect(pref: &str) -> Box<dyn GpuBackend> {
    match pref {
        "hy-smi" => Box::new(smi::SmiBackend::new("hy-smi")),
        "rocm-smi" => Box::new(smi::SmiBackend::new("rocm-smi")),
        "mock" => Box::new(mock::MockBackend::new(8)),
        _ => {
            if which("hy-smi") {
                log::info!("auto-detect: using hy-smi");
                Box::new(smi::SmiBackend::new("hy-smi"))
            } else if which("rocm-smi") {
                log::info!("auto-detect: using rocm-smi");
                Box::new(smi::SmiBackend::new("rocm-smi"))
            } else {
                log::warn!("auto-detect: no hy-smi/rocm-smi found, falling back to mock data");
                Box::new(mock::MockBackend::new(8))
            }
        }
    }
}
