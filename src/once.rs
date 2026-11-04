use crate::collectors::cpu::{CpuCollector, CpuSample};
use crate::collectors::gpu::{GpuBackend, GpuSample, GpuSnapshot};
use crate::collectors::{mem, proc};
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;

pub fn host_cpu_mem(cpu: &mut CpuCollector, proc_col: &mut proc::ProcCollector) -> (f64, f64) {
    let _ = cpu.read();
    let _ = proc_col.scan();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let cs = cpu
        .read()
        .unwrap_or(CpuSample { total: 0.0, per_core: vec![] });
    let ms = mem::read().unwrap_or_default();
    let mem_pct = (ms.total_kb as f64 - ms.avail_kb as f64) / ms.total_kb.max(1) as f64 * 100.0;
    (cs.total, mem_pct)
}

fn collect(backend: &dyn GpuBackend) -> GpuSnapshot {
    backend.collect().unwrap_or_else(|e| GpuSnapshot {
        backend: backend.name().to_string(),
        error: Some(e.to_string()),
        ..Default::default()
    })
}

/// Merge host info (user/cpu%/mem%/command) into GPU processes.
fn merge_processes(snap: &GpuSnapshot, proc_col: &mut proc::ProcCollector) -> Vec<ProcessRow> {
    let host = proc_col.scan();
    let mut map: std::collections::HashMap<u32, proc::ProcInfo> =
        host.into_iter().map(|p| (p.pid, p)).collect();
    let mut pids: Vec<u32> = snap.procs.iter().map(|p| p.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    for pid in pids {
        if let Some(hp) = map.get_mut(&pid) {
            proc_col.read_rss(hp);
            if hp.cmd.is_empty() {
                hp.cmd = proc::ProcCollector::read_cmd(pid);
            }
        }
    }
    let mut rows = Vec::new();
    for gp in &snap.procs {
        let host = map.get(&gp.pid);
        let user = host.map(|h| h.user.clone()).unwrap_or_else(|| "?".into());
        let cpu_pct = host.map(|h| h.cpu_pct).unwrap_or(0.0);
        let mem_pct = host.map(|h| h.mem_pct).unwrap_or(0.0);
        let cmd = host
            .map(|h| h.cmd.clone())
            .unwrap_or_else(|| gp.name.clone());
        for d in &gp.devices {
            rows.push(ProcessRow {
                pid: gp.pid,
                user: user.clone(),
                cpu_pct,
                mem_pct,
                cmd: cmd.clone(),
                device: Some(d.device),
                vram_mb: d.vram_mb,
                cu_pct: d.cu_pct,
            });
        }
    }
    rows
}

struct ProcessRow {
    pid: u32,
    user: String,
    cpu_pct: f64,
    mem_pct: f64,
    cmd: String,
    device: Option<usize>,
    vram_mb: Option<f64>,
    cu_pct: Option<f64>,
}

/// One-shot text snapshot (nvitop-like).
pub fn run_once(backend: &dyn GpuBackend) -> Result<()> {
    let mut cpu = CpuCollector::new();
    let mut pc = proc::ProcCollector::new();
    let (cpu_pct, mem_pct) = host_cpu_mem(&mut cpu, &mut pc);
    let snap = collect(backend);
    let rows = merge_processes(&snap, &mut pc);

    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_else(|_| "localhost".into())
        .trim()
        .to_string();
    println!(
        "dtop {}  host: {}  devices: {}  [backend: {}]  host cpu {:.1}%  mem {:.1}%",
        env!("CARGO_PKG_VERSION"),
        host,
        snap.gpus.len(),
        snap.backend,
        cpu_pct,
        mem_pct
    );
    if let Some(err) = &snap.error {
        println!("! {err}");
    }
    println!();
    println!(
        "{:>3}  {:<10}  {:>5}  {:>7}  {:>7}  {:>6}  {:>11}  {:>6}  {:>6}",
        "HCU", "Model", "Temp", "Power", "HCU%", "CU%", "VRAM", "SCLK", "MCLK"
    );
    for g in &snap.gpus {
        println!(
            "{:>3}  {:<10}  {:>5}  {:>7}  {:>7}  {:>6}  {:>11}  {:>6}  {:>6}",
            g.index,
            short_model(&g.name),
            g.temp_c.map(|t| format!("{t:.1}C")).unwrap_or_else(|| "N/A".into()),
            g.power_w.map(|p| format!("{p:.0}W")).unwrap_or_else(|| "N/A".into()),
            fmt_pct(g.util),
            fmt_pct(g.cu_util),
            fmt_vram(g.vram_used_mb, g.vram_total_mb),
            fmt_clk(g.sclk_mhz),
            fmt_clk(g.mclk_mhz),
        );
    }
    if !rows.is_empty() {
        println!();
        println!(
            "{:>8}  {:<8}  {:>3}  {:>8}  {:>5}  {:>6}  {:>5}  {}",
            "PID", "USER", "HCU", "VRAM", "CU%", "CPU%", "MEM%", "COMMAND"
        );
        for r in rows {
            println!(
                "{:>8}  {:<8}  {:>3}  {:>8}  {:>5}  {:>6.1}  {:>5.1}  {}",
                r.pid,
                r.user,
                r.device.map(|d| d.to_string()).unwrap_or_else(|| "-".into()),
                r.vram_mb.map(|v| format!("{:.1}G", v / 1024.0)).unwrap_or_else(|| "-".into()),
                r.cu_pct.map(|c| format!("{c:.1}")).unwrap_or_else(|| "N/A".into()),
                r.cpu_pct,
                r.mem_pct,
                r.cmd
            );
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct DevJson {
    vram_used_mb: Option<f64>,
    cu_pct: Option<f64>,
}

#[derive(Serialize)]
struct ProcJson {
    pid: u32,
    username: String,
    command: String,
    cpu_percent: f64,
    host_memory_percent: f64,
    devices: BTreeMap<String, DevJson>,
}

#[derive(Serialize)]
struct SnapshotJson {
    dtop_version: String,
    timestamp: f64,
    host: HostJson,
    devices: Vec<GpuSample>,
    processes: Vec<ProcJson>,
    errors: Vec<String>,
}

#[derive(Serialize)]
struct HostJson {
    cpu_percent: f64,
    memory_percent: f64,
}

/// One-shot JSON snapshot with a stable schema (null = unsupported metric).
pub fn run_json(backend: &dyn GpuBackend) -> Result<()> {
    let mut cpu = CpuCollector::new();
    let mut pc = proc::ProcCollector::new();
    let (cpu_pct, mem_pct) = host_cpu_mem(&mut cpu, &mut pc);
    let snap = collect(backend);
    let rows = merge_processes(&snap, &mut pc);

    let mut procs: Vec<ProcJson> = Vec::new();
    let mut by_pid: BTreeMap<u32, usize> = BTreeMap::new();
    for r in rows {
        let idx = *by_pid.entry(r.pid).or_insert_with(|| {
            procs.push(ProcJson {
                pid: r.pid,
                username: r.user.clone(),
                command: r.cmd.clone(),
                cpu_percent: r.cpu_pct,
                host_memory_percent: r.mem_pct,
                devices: BTreeMap::new(),
            });
            procs.len() - 1
        });
        if let Some(d) = r.device {
            procs[idx].devices.insert(
                d.to_string(),
                DevJson {
                    vram_used_mb: r.vram_mb,
                    cu_pct: r.cu_pct,
                },
            );
        }
    }

    let errors = snap.error.clone().into_iter().collect();
    let out = SnapshotJson {
        dtop_version: env!("CARGO_PKG_VERSION").into(),
        timestamp: chrono::Local::now().timestamp_millis() as f64 / 1000.0,
        host: HostJson {
            cpu_percent: cpu_pct,
            memory_percent: mem_pct,
        },
        devices: snap.gpus,
        processes: procs,
        errors,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn short_model(name: &str) -> String {
    let mut n = name;
    for p in ["HYGON ", "MOCK ", "AMD "] {
        if n.starts_with(p) {
            n = &n[p.len()..];
        }
    }
    let t: String = n.chars().take(10).collect();
    t
}

fn fmt_pct(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}%")).unwrap_or_else(|| "N/A".into())
}

fn fmt_vram(used: Option<f64>, total: Option<f64>) -> String {
    match (used, total) {
        (Some(u), Some(t)) => format!("{:.1}/{:.0}G", u / 1024.0, t / 1024.0),
        _ => "N/A".into(),
    }
}

fn fmt_clk(v: Option<u64>) -> String {
    v.map(|c| format!("{c}M")).unwrap_or_else(|| "N/A".into())
}
