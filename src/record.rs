use anyhow::{Context, Result};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Recorder {
    file: File,
    interval: Duration,
    last: Instant,
}

impl Recorder {
    pub fn new(path: &str, interval: f64) -> Result<Self> {
        let pb = PathBuf::from(path);
        if let Some(parent) = pb.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create dir {}", parent.display()))?;
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&pb)
            .with_context(|| format!("open record file {}", pb.display()))?;
        log::info!("recording metrics to {} every {}s", pb.display(), interval);
        Ok(Self {
            file,
            interval: Duration::from_secs_f64(interval.max(1.0)),
            last: Instant::now() - Duration::from_secs_f64(interval.max(1.0)),
        })
    }

    /// Write a record if the interval has elapsed. Errors are logged, never fatal.
    pub fn maybe_write(&mut self, entry: &impl Serialize) {
        if self.last.elapsed() < self.interval {
            return;
        }
        self.last = Instant::now();
        match serde_json::to_string(entry) {
            Ok(line) => {
                if let Err(e) = writeln!(self.file, "{line}") {
                    log::error!("record write failed: {e}");
                }
            }
            Err(e) => log::error!("record serialize failed: {e}"),
        }
    }
}

#[derive(Serialize)]
pub struct RecordEntry<'a> {
    pub ts: String,
    pub host: String,
    pub cpu_total: f64,
    pub load: [f64; 3],
    pub mem_kb: u64,
    pub mem_used_kb: u64,
    pub mem_cache_kb: u64,
    pub mem_avail_kb: u64,
    pub swap_used_kb: u64,
    pub swap_total_kb: u64,
    pub gpus: &'a [super::collectors::gpu::GpuSample],
}
