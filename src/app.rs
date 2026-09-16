use crate::cli::Cli;
use crate::collectors::cpu::{CpuCollector, CpuSample};
use crate::collectors::gpu::{detect, GpuBackend, GpuSample, GpuSnapshot, GpuTopology};
use crate::collectors::mem::{self, MemStats};
use crate::collectors::proc::{ProcCollector, ProcInfo};
use crate::record::{RecordEntry, Recorder};
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(PartialEq, Clone, Copy)]
pub enum View {
    Detail,
    Summary,
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

pub struct App {
    pub running: bool,
    pub view: View,
    pub theme_idx: usize,
    pub sort_by_mem: bool,
    pub selected: usize,
    pub show_help: bool,
    pub kill_target: Option<ProcInfo>,
    pub status_msg: Option<(String, Instant)>,

    pub cpu_col: CpuCollector,
    pub cpu: CpuSample,
    pub cpu_hist: VecDeque<f64>,
    pub mem: MemStats,
    pub proc_col: ProcCollector,
    pub procs: Vec<ProcInfo>,
    pub total_procs: usize,
    pub max_procs: usize,

    pub gpu_state: Arc<Mutex<GpuSnapshot>>,
    /// snapshot cached once per tick so the UI does not lock+clone per frame
    pub gpu: GpuSnapshot,
    /// static topology info (NUMA affinity, link matrix), fetched once
    pub topology: GpuTopology,
    pub gpu_util_hist: Vec<VecDeque<f64>>,
    pub gpu_vram_hist: Vec<VecDeque<f64>>,

    pub interval: f64,
    pub gpu_interval: f64,
    pub history_len: usize,
    pub hostname: String,
    pub uptime: String,

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
        let backend: Box<dyn GpuBackend> = detect(&cli.backend);
        let backend_name = backend.name().to_string();
        // static topology (NUMA affinity / link matrix): query once up front
        let topology = backend.topology().unwrap_or_default();
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
                    // trace level: full per-card values, useful for remote parser debugging
                    for g in &snap.gpus {
                        log::trace!(
                            "card{} name={:?} util={:?} hbm={:?}/{:?}MB temp={:?}C pwr={:?}W sclk={:?}",
                            g.index, g.name, g.util, g.vram_used_mb, g.vram_total_mb,
                            g.temp_c, g.power_w, g.sclk_mhz
                        );
                    }
                    for p in &snap.procs {
                        log::trace!(
                            "proc pid={} gpus={:?} vram={:?}MB",
                            p.pid, p.gpu_ids, p.vram_mb
                        );
                    }
                    *thread_state.lock().unwrap() = snap;
                }
                Err(e) => {
                    log::warn!("gpu collect failed: {e}");
                    let mut s = thread_state.lock().unwrap();
                    s.error = Some(e.to_string());
                }
            }
            // keep the sampling period accurate: subtract the time the smi
            // subprocess itself took, otherwise the interval drifts long
            let elapsed = cycle_start.elapsed();
            if elapsed < gi {
                std::thread::sleep(gi - elapsed);
            }
        });

        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .unwrap_or_else(|_| "localhost".into())
            .trim()
            .to_string();
        let uptime = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
            .map(|secs| {
                let d = (secs / 86400.0) as u64;
                let h = ((secs % 86400.0) / 3600.0) as u64;
                let m = ((secs % 3600.0) / 60.0) as u64;
                if d > 0 {
                    format!("{d}d {h}h")
                } else {
                    format!("{h}h {m}m")
                }
            })
            .unwrap_or_else(|| "?".into());

        let recorder = match &cli.record {
            Some(path) => Some(Recorder::new(path, cli.record_interval)?),
            None => None,
        };

        let mut app = Self {
            running: true,
            view: if cli.view == "summary" {
                View::Summary
            } else {
                View::Detail
            },
            theme_idx: theme::theme_index(&cli.theme),
            sort_by_mem: cli.sort == "mem",
            selected: 0,
            show_help: false,
            kill_target: None,
            status_msg: None,
            cpu_col: CpuCollector::new(),
            cpu: CpuSample {
                total: 0.0,
                per_core: vec![],
            },
            cpu_hist: VecDeque::new(),
            mem: MemStats::default(),
            proc_col: ProcCollector::new(),
            procs: vec![],
            total_procs: 0,
            max_procs: cli.procs,
            gpu_state: shared,
            gpu: GpuSnapshot::default(),
            topology,
            gpu_util_hist: vec![],
            gpu_vram_hist: vec![],
            interval: cli.interval.clamp(0.2, 30.0),
            gpu_interval: cli.gpu_interval,
            history_len: cli.history.max(10),
            hostname,
            uptime,
            warnings: Vec::new(),
            temp_warn: cli.temp_warn,
            temp_crit: cli.temp_crit,
            idle_warn_secs: cli.idle_warn_secs,
            idle_streak: Vec::new(),
            recorder,
            _gpu_thread: Some(handle),
        };
        // prime collectors so the first frame has data
        app.tick();
        std::thread::sleep(Duration::from_millis(350));
        app.tick();
        Ok(app)
    }

    pub fn tick(&mut self) {
        if let Ok(s) = self.cpu_col.read() {
            self.cpu = s;
        }
        if let Ok(m) = mem::read() {
            self.mem = m;
        }

        // grab the latest GPU snapshot once per tick
        self.gpu = self.gpu_state.lock().unwrap().clone();
        let mut procs = self.proc_col.scan();
        for p in procs.iter_mut() {
            if let Some(gp) = self.gpu.procs.iter().find(|g| g.pid == p.pid) {
                p.gpu_ids = gp.gpu_ids.clone();
                p.gpu_vram_mb = gp.vram_mb.unwrap_or(0.0);
            }
        }
        if self.sort_by_mem {
            // memory sort needs RSS for every process
            for p in procs.iter_mut() {
                self.proc_col.read_rss(p);
            }
            procs.sort_by(|a, b| b.mem_pct.partial_cmp(&a.mem_pct).unwrap());
        } else {
            procs.sort_by(|a, b| b.cpu_pct.partial_cmp(&a.cpu_pct).unwrap());
        }
        self.total_procs = procs.len();
        procs.truncate(self.max_procs);
        // RSS + cmdline + container attribution only for displayed rows
        for p in procs.iter_mut() {
            self.proc_col.read_rss(p);
            p.cmd = ProcCollector::read_cmd(p.pid);
            let (pod, cid) = ProcCollector::read_container(p.pid);
            p.pod = pod;
            p.container = cid;
        }
        self.procs = procs;
        if self.selected >= self.procs.len() {
            self.selected = self.procs.len().saturating_sub(1);
        }

        // history
        push_hist(&mut self.cpu_hist, self.cpu.total, self.history_len);
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

        // persistence
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

    /// Derive anomaly warnings from the latest snapshot. Kept intentionally
    /// conservative: only sustained (not momentary) conditions warn.
    fn update_warnings(&mut self) {
        self.warnings = Self::compute_warnings(
            &self.gpu.gpus,
            &mut self.idle_streak,
            self.temp_warn,
            self.temp_crit,
            self.idle_warn_secs,
        );
    }

    /// Pure warning computation so it can be unit tested.
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
        // idle yet holding memory: usually a stalled/idle job
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
                    text: format!(
                        "DCU{} idle {}s but {:.0}% HBM held",
                        g.index, idle_streak[i], vram_pct
                    ),
                });
            }
        } else {
            idle_streak[i] = 0;
        }
    }
    warnings
}

    pub fn scroll(&mut self, delta: i64) {
        let len = self.procs.len().min(self.max_procs) as i64;
        if len == 0 {
            return;
        }
        self.selected = ((self.selected as i64 + delta).clamp(0, len - 1)) as usize;
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        // kill confirmation popup eats keys first
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
            KeyCode::Tab | KeyCode::Char('v') => {
                self.view = match self.view {
                    View::Detail => View::Summary,
                    View::Summary => View::Detail,
                };
            }
            KeyCode::Char('t') => {
                self.theme_idx = (self.theme_idx + 1) % theme::themes().len();
                self.set_status(format!("theme: {}", theme::themes()[self.theme_idx].name));
            }
            KeyCode::Up => self.scroll(-1),
            KeyCode::Down => self.scroll(1),
            KeyCode::PageUp => self.scroll(-5),
            KeyCode::PageDown => self.scroll(5),
            KeyCode::Char('s') => {
                self.sort_by_mem = !self.sort_by_mem;
                self.set_status(format!(
                    "sort by {}",
                    if self.sort_by_mem { "MEM%" } else { "CPU%" }
                ));
            }
            KeyCode::Char('k') => {
                if let Some(p) = self.procs.get(self.selected) {
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
        let gpus = vec![
            gpu(0, 70.0, "Normal", 93.0, 55.0),
            gpu(2, 88.0, "Power Saving", 93.0, 55.0),
        ];
        let mut streak = Vec::new();
        let w = App::compute_warnings(&gpus, &mut streak, 75.0, 85.0, 10);
        let texts: Vec<&str> = w.iter().map(|w| w.text.as_str()).collect();
        assert!(texts.contains(&"DCU2 88C critical"), "got {texts:?}");
        assert!(
            texts.iter().any(|t| t.contains("throttled")),
            "throttle warning missing: {texts:?}"
        );
        assert!(!texts.iter().any(|t| t.contains("DCU0")), "unexpected DCU0 warn: {texts:?}");
    }

    #[test]
    fn test_idle_holding_memory_needs_sustained() {
        let gpus = vec![gpu(0, 50.0, "Normal", 1.0, 80.0)];
        let mut streak = Vec::new();
        for i in 1..=9 {
            let w = App::compute_warnings(&gpus, &mut streak, 75.0, 85.0, 10);
            assert!(w.is_empty(), "warned too early at tick {i}: {w:?}");
        }
        let w = App::compute_warnings(&gpus, &mut streak, 75.0, 85.0, 10);
        assert!(w.iter().any(|w| w.text.contains("idle")), "no idle warning: {:?}", w.iter().map(|w| &w.text).collect::<Vec<_>>());
        // becoming busy resets the streak
        let busy = vec![gpu(0, 50.0, "Normal", 50.0, 80.0)];
        let w = App::compute_warnings(&busy, &mut streak, 75.0, 85.0, 10);
        assert!(w.is_empty());
        assert_eq!(streak[0], 0);
    }
}
