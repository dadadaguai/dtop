use crate::cli::Cli;
use crate::collectors::cpu::{CpuCollector, CpuSample};
use crate::collectors::gpu::{detect, GpuBackend, GpuSample, GpuSnapshot};
use crate::collectors::mem::{self, MemStats};
use crate::collectors::proc::{ProcCollector, ProcInfo};
use crate::record::{RecordEntry, Recorder};
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Process-table sort order (nvitop keys p/m/c/u).
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum SortKey {
    Pid,
    Vram,
    Cu,
    Cpu,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum WarnLevel {
    Warn,
    Crit,
}

#[derive(Debug)]
pub struct Warning {
    pub level: WarnLevel,
    pub text: String,
}

/// One row in the Processes table: a (process, device) pair plus host info.
#[derive(Debug, Clone)]
pub struct ProcRow {
    pub pid: u32,
    pub user: String,
    pub cpu_pct: f64,
    pub mem_pct: f64,
    pub cmd: String,
    pub pod: String,
    pub device: Option<usize>,
    pub vram_mb: Option<f64>,
    pub cu_pct: Option<f64>,
}

pub struct App {
    pub running: bool,
    pub theme_idx: usize,
    pub sort_key: SortKey,
    pub selected: usize,
    pub show_help: bool,
    pub kill_target: Option<ProcRow>,
    pub status_msg: Option<(String, Instant)>,
    pub filter: Option<HashSet<usize>>,

    pub cpu_col: CpuCollector,
    pub cpu: CpuSample,
    pub cpu_hist: VecDeque<f64>,
    pub mem: MemStats,
    pub mem_hist: VecDeque<f64>,
    pub proc_col: ProcCollector,
    pub proc_rows: Vec<ProcRow>,

    pub gpu_state: Arc<Mutex<GpuSnapshot>>,
    pub gpu: GpuSnapshot,
    pub gpu_util_hist: Vec<VecDeque<f64>>,
    pub gpu_vram_hist: Vec<VecDeque<f64>>,

    pub interval: f64,
    pub history_len: usize,
    pub hostname: String,

    pub warnings: Vec<Warning>,
    pub temp_warn: f64,
    pub temp_crit: f64,
    idle_warn_secs: u64,
    idle_streak: Vec<u32>,

    pub recorder: Option<Recorder>,
    _gpu_thread: Option<std::thread::JoinHandle<()>>,
}

impl App {
    pub fn new(cli: &Cli) -> anyhow::Result<Self> {
        let backend: Box<dyn GpuBackend> = detect(&cli.backend, cli.window_ms);
        let backend_name = backend.name().to_string();
        let shared = Arc::new(Mutex::new(GpuSnapshot {
            backend: backend_name,
            ..Default::default()
        }));

        let thread_state = shared.clone();
        let gi = Duration::from_secs_f64(cli.gpu_interval.max(0.2));
        let handle = std::thread::spawn(move || loop {
            let cycle_start = Instant::now();
            match backend.collect() {
                Ok(snap) => {
                    log::debug!(
                        "gpu snapshot: {} cards, {} procs, took {}ms",
                        snap.gpus.len(),
                        snap.procs.len(),
                        cycle_start.elapsed().as_millis()
                    );
                    for g in &snap.gpus {
                        log::trace!(
                            "card{} name={:?} util={:?} cu={:?} vram={:?}/{:?}MB temp={:?}C pwr={:?}W sclk={:?}",
                            g.index, g.name, g.util, g.cu_util, g.vram_used_mb,
                            g.vram_total_mb, g.temp_c, g.power_w, g.sclk_mhz
                        );
                    }
                    for p in &snap.procs {
                        log::trace!("proc pid={} devices={:?}", p.pid, p.devices);
                    }
                    *thread_state.lock().unwrap() = snap;
                }
                Err(e) => {
                    log::warn!("gpu collect failed: {e}");
                    let mut s = thread_state.lock().unwrap();
                    s.error = Some(e.to_string());
                }
            }
            let elapsed = cycle_start.elapsed();
            if elapsed < gi {
                std::thread::sleep(gi - elapsed);
            }
        });

        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .unwrap_or_else(|_| "localhost".into())
            .trim()
            .to_string();

        let recorder = match &cli.record {
            Some(path) => Some(Recorder::new(path, cli.record_interval)?),
            None => None,
        };

        let mut app = Self {
            running: true,
            theme_idx: theme::theme_index(&cli.theme),
            sort_key: SortKey::Pid,
            selected: 0,
            show_help: false,
            kill_target: None,
            status_msg: None,
            filter: cli.devices.as_ref().map(|d| d.iter().copied().collect()),
            cpu_col: CpuCollector::new(),
            cpu: CpuSample {
                total: 0.0,
                per_core: vec![],
            },
            cpu_hist: VecDeque::new(),
            mem: MemStats::default(),
            mem_hist: VecDeque::new(),
            proc_col: ProcCollector::new(),
            proc_rows: vec![],
            gpu_state: shared,
            gpu: GpuSnapshot::default(),
            gpu_util_hist: vec![],
            gpu_vram_hist: vec![],
            interval: cli.interval.clamp(0.2, 30.0),
            history_len: cli.history.max(10),
            hostname,
            warnings: Vec::new(),
            temp_warn: cli.temp_warn,
            temp_crit: cli.temp_crit,
            idle_warn_secs: cli.idle_warn_secs,
            idle_streak: Vec::new(),
            recorder,
            _gpu_thread: Some(handle),
        };
        app.tick();
        std::thread::sleep(Duration::from_millis(350));
        app.tick();
        Ok(app)
    }

    /// Host memory usage percentage (matches `top`/`free`: (total-avail)/total).
    fn mem_pct(&self) -> f64 {
        let total = self.mem.total_kb.max(1) as f64;
        (total - self.mem.avail_kb as f64) / total * 100.0
    }

    pub fn tick(&mut self) {
        if let Ok(s) = self.cpu_col.read() {
            self.cpu = s;
        }
        if let Ok(m) = mem::read() {
            self.mem = m;
        }
        self.gpu = self.gpu_state.lock().unwrap().clone();

        // host /proc processes -> map pid -> ProcInfo (cpu% and user filled)
        let host_procs = self.proc_col.scan();
        let mut host_map: HashMap<u32, ProcInfo> =
            host_procs.into_iter().map(|p| (p.pid, p)).collect();

        // fill display-only fields (rss/cmd/container) for the pids we care about
        let mut pids: Vec<u32> = self.gpu.procs.iter().map(|p| p.pid).collect();
        pids.sort_unstable();
        pids.dedup();
        for pid in pids {
            if let Some(hp) = host_map.get_mut(&pid) {
                self.proc_col.read_rss(hp);
                if hp.cmd.is_empty() {
                    hp.cmd = ProcCollector::read_cmd(pid);
                }
                let (pod, cid) = ProcCollector::read_container(pid);
                hp.pod = pod;
                hp.container = cid;
            }
        }

        // build one row per (process, device)
        let mut rows: Vec<ProcRow> = Vec::new();
        for gp in &self.gpu.procs {
            let (user, cpu_pct, mem_pct, cmd, pod) = match host_map.get(&gp.pid) {
                Some(h) => (
                    h.user.clone(),
                    h.cpu_pct,
                    h.mem_pct,
                    h.cmd.clone(),
                    if !h.pod.is_empty() {
                        h.pod.clone()
                    } else {
                        h.container.clone()
                    },
                ),
                None => ("?".to_string(), 0.0, 0.0, gp.name.clone(), String::new()),
            };
            if gp.devices.is_empty() {
                rows.push(ProcRow {
                    pid: gp.pid,
                    user: user.clone(),
                    cpu_pct,
                    mem_pct,
                    cmd: cmd.clone(),
                    pod: pod.clone(),
                    device: None,
                    vram_mb: None,
                    cu_pct: None,
                });
            }
            for d in &gp.devices {
                rows.push(ProcRow {
                    pid: gp.pid,
                    user: user.clone(),
                    cpu_pct,
                    mem_pct,
                    cmd: cmd.clone(),
                    pod: pod.clone(),
                    device: Some(d.device),
                    vram_mb: d.vram_mb,
                    cu_pct: d.cu_pct,
                });
            }
        }

        // filter + sort
        if let Some(f) = &self.filter {
            rows.retain(|r| r.device.map_or(false, |d| f.contains(&d)));
        }
        match self.sort_key {
            SortKey::Pid => rows.sort_by_key(|r| r.pid),
            SortKey::Vram => rows.sort_by(|a, b| {
                b.vram_mb
                    .partial_cmp(&a.vram_mb)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            SortKey::Cu => rows.sort_by(|a, b| {
                b.cu_pct
                    .partial_cmp(&a.cu_pct)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            SortKey::Cpu => rows.sort_by(|a, b| b.cpu_pct.partial_cmp(&a.cpu_pct).unwrap()),
        }
        self.proc_rows = rows;
        if self.selected >= self.proc_rows.len() {
            self.selected = self.proc_rows.len().saturating_sub(1);
        }

        // history: host cpu/mem + per-card util/vram
        let mem_pct = self.mem_pct();
        push_hist(&mut self.cpu_hist, self.cpu.total, self.history_len);
        push_hist(&mut self.mem_hist, mem_pct, self.history_len);
        let n = self.gpu.gpus.len();
        if self.gpu_util_hist.len() != n {
            self.gpu_util_hist = vec![VecDeque::new(); n];
            self.gpu_vram_hist = vec![VecDeque::new(); n];
        }
        for i in 0..n {
            let (util, vram_pct) = {
                let g = &self.gpu.gpus[i];
                (
                    g.util.unwrap_or(0.0),
                    match (g.vram_used_mb, g.vram_total_mb) {
                        (Some(u), Some(t)) if t > 0.0 => u / t * 100.0,
                        _ => 0.0,
                    },
                )
            };
            push_hist(&mut self.gpu_util_hist[i], util, self.history_len);
            push_hist(&mut self.gpu_vram_hist[i], vram_pct, self.history_len);
        }

        if let Some(rec) = self.recorder.as_mut() {
            let entry = RecordEntry {
                ts: chrono::Local::now().to_rfc3339(),
                host: self.hostname.clone(),
                cpu_total: self.cpu.total,
                load: self.cpu_col.load,
                mem_kb: self.mem.total_kb,
                mem_used_kb: self.mem.used_kb,
                mem_cache_kb: self.mem.cache_kb,
                mem_avail_kb: self.mem.avail_kb,
                swap_used_kb: self.mem.swap_used_kb(),
                swap_total_kb: self.mem.swap_total_kb,
                gpus: &self.gpu.gpus,
            };
            rec.maybe_write(&entry);
        }

        self.update_warnings();
    }

    fn update_warnings(&mut self) {
        self.warnings = Self::compute_warnings(
            &self.gpu.gpus,
            &mut self.idle_streak,
            self.temp_warn,
            self.temp_crit,
            self.idle_warn_secs,
        );
    }

    pub fn compute_warnings(
        gpus: &[GpuSample],
        idle_streak: &mut Vec<u32>,
        temp_warn: f64,
        temp_crit: f64,
        idle_warn_secs: u64,
    ) -> Vec<Warning> {
        let mut warnings = Vec::new();
        if idle_streak.len() != gpus.len() {
            *idle_streak = vec![0; gpus.len()];
        }
        for (i, g) in gpus.iter().enumerate() {
            if let Some(t) = g.temp_c {
                if t >= temp_crit {
                    warnings.push(Warning {
                        level: WarnLevel::Crit,
                        text: format!("DCU{} {:.0}C critical", g.index, t),
                    });
                } else if t >= temp_warn {
                    warnings.push(Warning {
                        level: WarnLevel::Warn,
                        text: format!("DCU{} {:.0}C hot", g.index, t),
                    });
                }
            }
            if let Some(m) = &g.mode {
                let ml = m.to_lowercase();
                if ml.contains("power saving") || ml.contains("throttl") || ml.contains("low") {
                    warnings.push(Warning {
                        level: WarnLevel::Warn,
                        text: format!("DCU{} throttled ({})", g.index, m),
                    });
                }
            }
            let util = g.util.unwrap_or(100.0);
            let vram_pct = match (g.vram_used_mb, g.vram_total_mb) {
                (Some(u), Some(t)) if t > 0.0 => u / t * 100.0,
                _ => 0.0,
            };
            if util < 5.0 && vram_pct > 30.0 {
                idle_streak[i] += 1;
                if idle_streak[i] as u64 == idle_warn_secs.max(1) {
                    warnings.push(Warning {
                        level: WarnLevel::Warn,
                        text: format!("DCU{} idle {}s but {:.0}% VRAM held", g.index, idle_streak[i], vram_pct),
                    });
                }
            } else {
                idle_streak[i] = 0;
            }
        }
        warnings
    }

    pub fn scroll(&mut self, delta: i64) {
        let len = self.proc_rows.len() as i64;
        if len == 0 {
            return;
        }
        self.selected = ((self.selected as i64 + delta).clamp(0, len - 1)) as usize;
    }

    fn toggle_filter(&mut self, index: usize) {
        let mut f = self.filter.take().unwrap_or_default();
        if !f.remove(&index) {
            f.insert(index);
        }
        self.filter = if f.is_empty() { None } else { Some(f) };
        let shown = self
            .filter
            .as_ref()
            .map(|f| {
                let mut v: Vec<_> = f.iter().copied().collect();
                v.sort_unstable();
                v.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
            })
            .unwrap_or_else(|| "all".into());
        self.set_status(format!("filter: {shown}"));
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if let Some(target) = self.kill_target.clone() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    match std::process::Command::new("kill")
                        .arg(target.pid.to_string())
                        .status()
                    {
                        Ok(_) => {
                            log::warn!("sent SIGTERM to pid {} ({})", target.pid, target.cmd);
                            self.set_status(format!("SIGTERM -> {} ({})", target.pid, target.user));
                        }
                        Err(e) => {
                            log::error!("kill {} failed: {e}", target.pid);
                            self.set_status(format!("kill failed: {e}"));
                        }
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {}
                _ => {}
            }
            self.kill_target = None;
            return;
        }

        if self.show_help {
            self.show_help = false;
            if key.code == KeyCode::Char('q') {
                self.running = false;
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.running = false,
            KeyCode::Char('h') | KeyCode::F(1) => self.show_help = true,
            KeyCode::Char('t') => {
                self.theme_idx = (self.theme_idx + 1) % theme::themes().len();
                self.set_status(format!("theme: {}", theme::themes()[self.theme_idx].name));
            }
            KeyCode::Char('r') => self.set_status("refreshing".into()),
            KeyCode::Up => self.scroll(-1),
            KeyCode::Down => self.scroll(1),
            KeyCode::PageUp => self.scroll(-10),
            KeyCode::PageDown => self.scroll(10),
            KeyCode::Char('p') => {
                self.sort_key = SortKey::Pid;
                self.set_status("sort: PID".into());
            }
            KeyCode::Char('m') => {
                self.sort_key = SortKey::Vram;
                self.set_status("sort: VRAM".into());
            }
            KeyCode::Char('c') => {
                self.sort_key = SortKey::Cu;
                self.set_status("sort: CU%".into());
            }
            KeyCode::Char('u') => {
                self.sort_key = SortKey::Cpu;
                self.set_status("sort: CPU%".into());
            }
            KeyCode::Char('a') => {
                self.filter = None;
                self.set_status("filter: all".into());
            }
            KeyCode::Char(d @ '1'..='9') => {
                self.toggle_filter((d as u8 - b'1') as usize);
            }
            KeyCode::Char('k') => {
                if let Some(p) = self.proc_rows.get(self.selected) {
                    if p.pid > 1 {
                        self.kill_target = Some(p.clone());
                    } else {
                        self.set_status("cannot kill pid 1".into());
                    }
                }
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.interval = (self.interval + 0.5).min(30.0);
                self.set_status(format!("interval {:.1}s", self.interval));
            }
            KeyCode::Char('-') => {
                self.interval = (self.interval - 0.5).max(0.5);
                self.set_status(format!("interval {:.1}s", self.interval));
            }
            _ => {}
        }
    }

    fn set_status(&mut self, msg: String) {
        self.status_msg = Some((msg, Instant::now()));
    }

    pub fn status_text(&self) -> Option<&str> {
        match &self.status_msg {
            Some((msg, t)) if t.elapsed() < Duration::from_secs(4) => Some(msg),
            _ => None,
        }
    }
}

fn push_hist(q: &mut VecDeque<f64>, v: f64, cap: usize) {
    q.push_back(v);
    while q.len() > cap {
        q.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(index: usize, temp: f64, mode: &str, util: f64, vram_pct: f64) -> GpuSample {
        GpuSample {
            index,
            temp_c: Some(temp),
            mode: Some(mode.to_string()),
            util: Some(util),
            vram_used_mb: Some(65520.0 * vram_pct / 100.0),
            vram_total_mb: Some(65520.0),
            ..Default::default()
        }
    }

    #[test]
    fn test_temp_and_throttle_warnings() {
        let gpus = vec![gpu(0, 70.0, "Normal", 93.0, 55.0), gpu(2, 88.0, "Power Saving", 93.0, 55.0)];
        let mut streak = Vec::new();
        let w = App::compute_warnings(&gpus, &mut streak, 75.0, 85.0, 10);
        let texts: Vec<&str> = w.iter().map(|w| w.text.as_str()).collect();
        assert!(texts.contains(&"DCU2 88C critical"), "got {texts:?}");
        assert!(texts.iter().any(|t| t.contains("throttled")), "missing throttle: {texts:?}");
    }

    #[test]
    fn test_idle_holding_memory_needs_sustained() {
        let gpus = vec![gpu(0, 50.0, "Normal", 1.0, 80.0)];
        let mut streak = Vec::new();
        for i in 1..=9 {
            let w = App::compute_warnings(&gpus, &mut streak, 75.0, 85.0, 10);
            assert!(w.is_empty(), "warned too early at {i}: {w:?}");
        }
        let w = App::compute_warnings(&gpus, &mut streak, 75.0, 85.0, 10);
        assert!(w.iter().any(|w| w.text.contains("idle")), "no idle warning");
    }
}
