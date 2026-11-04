use super::{GpuBackend, GpuProc, GpuProcDevice, GpuSample, GpuSnapshot};
use anyhow::Result;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

struct MockGpu {
    util: f64,
    cu: f64,
    vram: f64,
    temp: f64,
    power: f64,
    sclk: f64,
}

pub struct MockBackend {
    state: Mutex<(Vec<MockGpu>, u64)>,
    n: usize,
}

impl MockBackend {
    pub fn new(n: usize) -> Self {
        let gpus = (0..n)
            .map(|_| MockGpu {
                util: 30.0,
                cu: 20.0,
                vram: 55.0,
                temp: 45.0,
                power: 140.0,
                sclk: 1500.0,
            })
            .collect();
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(42);
        Self {
            state: Mutex::new((gpus, seed)),
            n,
        }
    }
}

fn next_rand(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*seed >> 33) as f64) / (u32::MAX as f64)
}

fn walk(v: f64, min: f64, max: f64, step: f64, r: f64) -> f64 {
    (v + (r * 2.0 - 1.0) * step).clamp(min, max)
}

impl GpuBackend for MockBackend {
    fn name(&self) -> &str {
        "mock"
    }

    fn collect(&self) -> Result<GpuSnapshot> {
        let mut guard = self.state.lock().unwrap();
        let (gpus, seed) = &mut *guard;
        let mut out = Vec::with_capacity(self.n);
        for (i, g) in gpus.iter_mut().enumerate() {
            g.util = walk(g.util, 2.0, 99.0, 12.0, next_rand(seed));
            g.cu = walk(g.cu, 1.0, 95.0, 9.0, next_rand(seed));
            g.vram = walk(g.vram, 20.0, 95.0, 3.0, next_rand(seed));
            g.temp = walk(g.temp, 35.0, 80.0, 1.5, next_rand(seed));
            g.power = walk(g.power, 80.0, 300.0, 15.0, next_rand(seed));
            g.sclk = walk(g.sclk, 1000.0, 1800.0, 80.0, next_rand(seed));
            out.push(GpuSample {
                index: i,
                name: "DCU-3G (mock)".into(),
                util: Some(g.util),
                cu_util: Some(g.cu),
                vram_used_mb: Some(144.0 * 1024.0 * g.vram / 100.0),
                vram_total_mb: Some(144.0 * 1024.0),
                temp_c: Some(g.temp),
                power_w: Some(g.power),
                power_cap_w: Some(800.0),
                sclk_mhz: Some(g.sclk as u64),
                mclk_mhz: Some(875),
                pci_bus_id: format!("0000:{:02x}:00.0", 5 + i * 2),
                numa_node: Some((i % 8) as u32),
                cu_count: Some(64),
                ..Default::default()
            });
        }
        let procs = vec![
            GpuProc {
                pid: 999991,
                name: "mock-train".into(),
                devices: vec![
                    GpuProcDevice {
                        device: 0,
                        vram_mb: Some(31.2 * 1024.0),
                        cu_pct: Some(78.0),
                    },
                    GpuProcDevice {
                        device: 1,
                        vram_mb: Some(20.0 * 1024.0),
                        cu_pct: Some(65.0),
                    },
                ],
            },
            GpuProc {
                pid: 999992,
                name: "mock-infer".into(),
                devices: vec![GpuProcDevice {
                    device: 2,
                    vram_mb: Some(14.6 * 1024.0),
                    cu_pct: Some(40.0),
                }],
            },
        ];
        Ok(GpuSnapshot {
            backend: "mock".into(),
            gpus: out,
            procs,
            error: None,
        })
    }
}
