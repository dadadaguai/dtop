use anyhow::Result;
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone, Default)]
pub struct MemStats {
    pub total_kb: u64,
    pub avail_kb: u64,
    pub used_kb: u64,
    pub cache_kb: u64,
    pub swap_total_kb: u64,
    pub swap_free_kb: u64,
}

impl MemStats {
    pub fn swap_used_kb(&self) -> u64 {
        self.swap_total_kb.saturating_sub(self.swap_free_kb)
    }
}

pub fn read() -> Result<MemStats> {
    let content = fs::read_to_string("/proc/meminfo")?;
    let mut map: HashMap<String, u64> = HashMap::new();
    for line in content.lines() {
        if let Some((k, rest)) = line.split_once(':') {
            if let Some(v) = rest.split_whitespace().next() {
                map.insert(k.trim().to_string(), v.parse().unwrap_or(0));
            }
        }
    }
    let g = |k: &str| map.get(k).copied().unwrap_or(0);
    let total = g("MemTotal");
    let avail = if g("MemAvailable") > 0 {
        g("MemAvailable")
    } else {
        g("MemFree") + g("Buffers") + g("Cached")
    };
    Ok(MemStats {
        total_kb: total,
        avail_kb: avail,
        used_kb: total.saturating_sub(avail),
        cache_kb: g("Buffers") + g("Cached") + g("SReclaimable"),
        swap_total_kb: g("SwapTotal"),
        swap_free_kb: g("SwapFree"),
    })
}

pub fn fmt_kb(kb: u64) -> String {
    let gb = kb as f64 / 1024.0 / 1024.0;
    if gb >= 1.0 {
        format!("{gb:.1}G")
    } else {
        format!("{:.0}M", kb as f64 / 1024.0)
    }
}
