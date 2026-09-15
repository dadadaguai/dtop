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
    /// short container id (docker/containerd/crio), empty when not containerized
    pub container: String,
    /// short kubernetes pod uid, empty when not in a pod
    pub pod: String,
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
            container: String::new(),
            pod: String::new(),
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

    /// Resolve the container id and kubernetes pod uid from the cgroup path.
    /// Returns (pod_short, container_short); both empty for non-containerized
    /// processes. Only called for visible rows.
    pub fn read_container(pid: u32) -> (String, String) {
        match fs::read_to_string(format!("/proc/{pid}/cgroup")) {
            Ok(s) => parse_cgroup(&s),
            Err(_) => (String::new(), String::new()),
        }
    }
}

/// Parse a /proc/<pid>/cgroup file for pod uid and container id.
fn parse_cgroup(cg: &str) -> (String, String) {
    let mut pod = String::new();
    let mut cid = String::new();
    for tok in cg.split(['/', ':']) {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        // kubernetes pod uid appears as "...pod<uid>.slice". Note that
        // "kubepods" itself contains "pod", so scan for the first occurrence
        // actually followed by a uid-length hex/dash run.
        if pod.is_empty() {
            if let Some(uid) = extract_pod_uid(t) {
                pod = uid;
            }
        }
        // container id: runtime-prefixed, or a bare long hex run
        for pfx in ["docker-", "cri-containerd-", "containerd-", "crio-"] {
            if let Some(rest) = t.strip_prefix(pfx) {
                let id: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                if id.len() >= 12 && cid.is_empty() {
                    cid = id[..12].to_string();
                }
            }
        }
        if cid.is_empty() {
            let run = longest_hex_run(t);
            if run.len() >= 32 {
                cid = run[..12].to_string();
            }
        }
    }
    (pod, cid)
}

/// Extract a pod uid short form from a token like
/// "kubepods-burstable-pod1a2b3c4d-1111-2222-3333-444455556666.slice".
/// "kubepods" itself contains "pod", so only accept an occurrence whose
/// following hex/dash run is long enough to be a uid.
fn extract_pod_uid(t: &str) -> Option<String> {
    let mut start = 0;
    while let Some(pos) = t[start..].find("pod") {
        let p = start + pos + 3;
        let uid: String = t[p..]
            .chars()
            .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
            .collect();
        if uid.len() >= 32 {
            return Some(uid[..8].to_string());
        }
        start = p;
    }
    None
}

fn longest_hex_run(s: &str) -> String {
    let mut best = String::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_ascii_hexdigit() {
            cur.push(c);
        } else if cur.len() > best.len() {
            best = std::mem::take(&mut cur);
        } else {
            cur.clear();
        }
    }
    if cur.len() > best.len() {
        best = cur;
    }
    best
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cgroup_k8s_containerd() {
        let cg = "0::/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod1a2b3c4d-1111-2222-3333-444455556666.slice/cri-containerd-abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789.scope\n";
        let (pod, cid) = parse_cgroup(cg);
        assert_eq!(pod, "1a2b3c4d");
        assert_eq!(cid, "abcdef012345");
    }

    #[test]
    fn test_parse_cgroup_docker() {
        let cg = "12:pids:/docker/9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0\n";
        let (pod, cid) = parse_cgroup(cg);
        assert!(pod.is_empty());
        assert_eq!(cid, "9f8e7d6c5b4a");
    }

    #[test]
    fn test_parse_cgroup_systemd_docker_scope() {
        let cg = "0::/system.slice/docker-1122334455667788990011223344556677889900112233445566778899001122.scope";
        let (_, cid) = parse_cgroup(cg);
        assert_eq!(cid, "112233445566");
    }

    #[test]
    fn test_parse_cgroup_bare_systemd() {
        let cg = "0::/system.slice/sshd.service\n1:name=systemd:/user.slice/user-1000.slice/session-1.scope\n";
        let (pod, cid) = parse_cgroup(cg);
        assert!(pod.is_empty());
        assert!(cid.is_empty());
    }
}
