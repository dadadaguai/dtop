use std::collections::HashMap;
use std::fs;
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct ProcInfo {
    pub pid: u32,
    pub user: String,
    pub cpu_pct: f64,
    pub mem_pct: f64,
    #[allow(dead_code)]
    pub rss_kb: u64,
    pub cmd: String,
    pub gpu_ids: Vec<usize>,
    pub gpu_vram_mb: f64,
}

pub struct ProcCollector {
    prev: HashMap<u32, (u64, Instant)>,
    users: HashMap<u32, String>,
    total_mem_kb: u64,
}

impl ProcCollector {
    pub fn new() -> Self {
        Self {
            prev: HashMap::new(),
            users: load_users(),
            total_mem_kb: 1,
        }
    }

    pub fn scan(&mut self) -> Vec<ProcInfo> {
        if let Ok(m) = super::mem::read() {
            self.total_mem_kb = m.total_kb.max(1);
        }
        let now = Instant::now();
        let mut out = Vec::new();
        let mut alive = Vec::new();

        let entries = match fs::read_dir("/proc") {
            Ok(e) => e,
            Err(_) => return out,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let pid: u32 = match name.parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            alive.push(pid);
            if let Some(info) = self.read_proc(pid, now) {
                out.push(info);
            }
        }
        self.prev.retain(|pid, _| alive.contains(pid));
        out
    }

    fn read_proc(&mut self, pid: u32, now: Instant) -> Option<ProcInfo> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let lp = stat.find('(')?;
        let rp = stat.rfind(')')?;
        let comm = stat[lp + 1..rp].to_string();
        let fields: Vec<&str> = stat[rp + 2..].split_whitespace().collect();
        // fields[0] = state (#3); utime = #14 -> idx 11, stime = #15 -> idx 12
        let utime: u64 = fields.get(11)?.parse().ok()?;
        let stime: u64 = fields.get(12)?.parse().ok()?;
        let ticks = utime + stime;

        let cpu_pct = match self.prev.get(&pid) {
            Some((pt, pi)) => {
                let dt = now.duration_since(*pi).as_secs_f64();
                if dt > 0.01 {
                    ((ticks.saturating_sub(*pt) as f64) / 100.0 / dt * 100.0).max(0.0)
                } else {
                    0.0
                }
            }
            None => 0.0,
        };
        self.prev.insert(pid, (ticks, now));

        let rss_pages: u64 = fs::read_to_string(format!("/proc/{pid}/statm"))
            .ok()
            .and_then(|s| s.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
            .unwrap_or(0);
        let rss_kb = rss_pages * 4;

        let uid: u32 = fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Uid:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(0);
        let user = self
            .users
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string());

        let cmd = fs::read_to_string(format!("/proc/{pid}/cmdline"))
            .ok()
            .map(|s| s.replace('\0', " ").trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("[{comm}]"));

        Some(ProcInfo {
            pid,
            user,
            cpu_pct,
            mem_pct: rss_kb as f64 / self.total_mem_kb as f64 * 100.0,
            rss_kb,
            cmd,
            gpu_ids: Vec::new(),
            gpu_vram_mb: 0.0,
        })
    }
}

fn load_users() -> HashMap<u32, String> {
    let mut m = HashMap::new();
    if let Ok(content) = fs::read_to_string("/etc/passwd") {
        for line in content.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() >= 3 {
                if let Ok(uid) = f[2].parse::<u32>() {
                    m.insert(uid, f[0].to_string());
                }
            }
        }
    }
    m
}
