use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
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

    /// Scan /proc and return per-process CPU stats (no RSS/cmdline yet).
    ///
    /// Only /proc/<pid>/stat and a stat() for the owner uid are read here, so
    /// cost stays low on nodes with thousands of processes. RSS is filled on
    /// demand for the rows that need it via `read_rss`, and the command line
    /// via `read_cmd`.
    pub fn scan(&mut self) -> Vec<ProcInfo> {
        if let Ok(m) = super::mem::read() {
            self.total_mem_kb = m.total_kb.max(1);
        }
        let now = Instant::now();
        let mut out = Vec::new();
        // HashSet: the old Vec+contains made the retain below O(n^2)
        let mut alive: HashSet<u32> = HashSet::with_capacity(self.prev.len() + 64);
        let mut path = String::with_capacity(64);

        let entries = match fs::read_dir("/proc") {
            Ok(e) => e,
            Err(_) => return out,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let pid: u32 = match name.to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            alive.insert(pid);
            if let Some(info) = self.read_proc(pid, now, &mut path) {
                out.push(info);
            }
        }
        self.prev.retain(|pid, _| alive.contains(pid));
        out
    }

    fn read_proc(&mut self, pid: u32, now: Instant, path: &mut String) -> Option<ProcInfo> {
        use std::fmt::Write as _;
        path.clear();
        let _ = write!(path, "/proc/{pid}/stat");
        let stat = fs::read_to_string(&*path).ok()?;
        let rp = stat.rfind(')')?;
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

        // uid of the process directory owner == process real uid. A stat() is
        // far cheaper than reading the whole /proc/<pid>/status file.
        path.clear();
        let _ = write!(path, "/proc/{pid}");
        let uid = fs::metadata(&*path).map(|m| m.uid()).unwrap_or(0);
        let user = self
            .users
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string());

        Some(ProcInfo {
            pid,
            user,
            cpu_pct,
            mem_pct: 0.0,
            rss_kb: 0,
            cmd: String::new(),
            gpu_ids: Vec::new(),
            gpu_vram_mb: 0.0,
        })
    }

    /// Read RSS and fill mem_pct (only needed for processes that are shown or
    /// sorted by memory).
    pub fn read_rss(&self, p: &mut ProcInfo) -> Option<()> {
        let rss_pages: u64 = fs::read_to_string(format!("/proc/{}/statm", p.pid))
            .ok()
            .and_then(|s| s.split_whitespace().nth(1).and_then(|v| v.parse().ok()))?;
        p.rss_kb = rss_pages * 4;
        p.mem_pct = p.rss_kb as f64 / self.total_mem_kb as f64 * 100.0;
        Some(())
    }

    /// Read the command line for a single pid (only called for visible rows).
    pub fn read_cmd(pid: u32) -> String {
        fs::read_to_string(format!("/proc/{pid}/cmdline"))
            .ok()
            .map(|s| s.replace('\0', " ").trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                fs::read_to_string(format!("/proc/{pid}/comm"))
                    .ok()
                    .map(|s| format!("[{}]", s.trim()))
            })
            .unwrap_or_else(|| "?".into())
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
