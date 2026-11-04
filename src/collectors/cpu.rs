use anyhow::Result;
use std::fs;

pub struct CpuSample {
    pub total: f64,
    #[allow(dead_code)]
    pub per_core: Vec<f64>,
}

pub struct CpuCollector {
    prev_total: Option<Vec<u64>>,
    prev_cores: Option<Vec<Vec<u64>>>,
    #[allow(dead_code)]
    pub model: String,
    #[allow(dead_code)]
    pub nproc: usize,
    pub load: [f64; 3],
}

fn pct(prev: &[u64], cur: &[u64]) -> f64 {
    let idle = |v: &[u64]| v[3] + v.get(4).copied().unwrap_or(0);
    let sum = |v: &[u64]| v.iter().sum::<u64>();
    let dt = sum(cur).saturating_sub(sum(prev)) as f64;
    let di = idle(cur).saturating_sub(idle(prev)) as f64;
    if dt <= 0.0 {
        0.0
    } else {
        ((dt - di) / dt * 100.0).clamp(0.0, 100.0)
    }
}

impl CpuCollector {
    pub fn new() -> Self {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let model = cpuinfo
            .lines()
            .find(|l| l.starts_with("model name"))
            .and_then(|l| l.splitn(2, ':').nth(1))
            .unwrap_or("CPU")
            .trim()
            .to_string();
        let nproc = cpuinfo
            .lines()
            .filter(|l| l.starts_with("processor"))
            .count()
            .max(1);
        Self {
            prev_total: None,
            prev_cores: None,
            model,
            nproc,
            load: [0.0; 3],
        }
    }

    pub fn read(&mut self) -> Result<CpuSample> {
        let stat = fs::read_to_string("/proc/stat")?;
        let mut total: Vec<u64> = Vec::new();
        let mut cores: Vec<Vec<u64>> = Vec::new();
        for line in stat.lines() {
            if let Some(rest) = line.strip_prefix("cpu") {
                let nums: Vec<u64> = rest
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .collect();
                if nums.len() < 4 {
                    continue;
                }
                if rest.starts_with(char::is_whitespace) {
                    total = nums;
                } else {
                    cores.push(nums);
                }
            }
        }

        let sample = match (&self.prev_total, &self.prev_cores) {
            (Some(pt), Some(pc)) => CpuSample {
                total: pct(pt, &total),
                per_core: pc
                    .iter()
                    .zip(cores.iter())
                    .map(|(p, c)| pct(p, c))
                    .collect(),
            },
            _ => CpuSample {
                total: 0.0,
                per_core: vec![0.0; cores.len()],
            },
        };

        self.prev_total = Some(total);
        self.prev_cores = Some(cores);

        if let Ok(l) = fs::read_to_string("/proc/loadavg") {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() >= 3 {
                self.load = [
                    f[0].parse().unwrap_or(0.0),
                    f[1].parse().unwrap_or(0.0),
                    f[2].parse().unwrap_or(0.0),
                ];
            }
        }
        Ok(sample)
    }
}
