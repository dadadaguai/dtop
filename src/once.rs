use crate::cli::Cli;
use crate::collectors::cpu::CpuCollector;
use crate::collectors::gpu::{GpuBackend, GpuSnapshot};
use crate::collectors::{mem, proc};
use anyhow::Result;

/// One-shot snapshot mode (like gpustat): print and exit.
pub fn run(cli: &Cli, backend: &dyn GpuBackend) -> Result<()> {
    let mut cpu = CpuCollector::new();
    let _ = cpu.read();
    let mut pc = proc::ProcCollector::new();
    let _ = pc.scan();
    std::thread::sleep(std::time::Duration::from_millis(400));
    let cs = cpu.read()?;
    let ms = mem::read()?;
    let mut procs = pc.scan();
    let snap: GpuSnapshot = backend.collect().unwrap_or_else(|e| GpuSnapshot {
        backend: backend.name().to_string(),
        error: Some(e.to_string()),
        ..Default::default()
    });

    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_else(|_| "localhost".into())
        .trim()
        .to_string();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");

    println!("dtop {}  {}  {}  [backend: {}]", env!("CARGO_PKG_VERSION"), host, now, snap.backend);
    println!(
        "CPU  {:5.1}%  ({} cores)  load {:.2} {:.2} {:.2}",
        cs.total,
        cs.per_core.len(),
        cpu.load[0],
        cpu.load[1],
        cpu.load[2]
    );
    println!(
        "MEM  used {} / {}  cache {}  avail {}  swap {} / {}",
        mem::fmt_kb(ms.used_kb),
        mem::fmt_kb(ms.total_kb),
        mem::fmt_kb(ms.cache_kb),
        mem::fmt_kb(ms.avail_kb),
        mem::fmt_kb(ms.swap_used_kb()),
        mem::fmt_kb(ms.swap_total_kb)
    );

    if snap.gpus.is_empty() {
        let reason = snap
            .error
            .as_deref()
            .unwrap_or("no DCU data (backend produced no cards)");
        println!("DCU  none  [{reason}]");
        println!("     hints: install DTK (hy-smi) / check driver (ls /dev/kfd) / demo: dtop -b mock");
    }
    if let Some(err) = &snap.error {
        if !snap.gpus.is_empty() {
            println!("DCU  backend error: {err}");
        }
    }
    let gpu_by_pid: std::collections::HashMap<u32, &_> =
        snap.procs.iter().map(|p| (p.pid, p)).collect();
    for g in &snap.gpus {
        let util_s = g
            .util
            .map(|u| format!("{u:>3.0}%"))
            .unwrap_or_else(|| "N/A ".into());
        println!(
            "DCU{} {}  util {}  hbm {}/{}  {}°C  {}W  sclk {}MHz",
            g.index,
            g.name,
            util_s,
            g.vram_used_mb
                .map(|v| format!("{:.1}G", v / 1024.0))
                .unwrap_or_else(|| "N/A".into()),
            g.vram_total_mb
                .map(|v| format!("{:.0}G", v / 1024.0))
                .unwrap_or_else(|| "N/A".into()),
            g.temp_c.map(|t| format!("{t:.0}")).unwrap_or_else(|| "N/A".into()),
            g.power_w.map(|p| format!("{p:.0}")).unwrap_or_else(|| "N/A".into()),
            g.sclk_mhz
                .map(|s| s.to_string())
                .unwrap_or_else(|| "N/A".into()),
        );
    }

    if cli.procs > 0 {
        println!();
        println!(
            "{:<8}{:<12}{:>7}{:>7}  {:<8}{:>8}  {}",
            "PID", "USER", "CPU%", "MEM%", "DCU", "HBM", "COMMAND"
        );
        if cli.sort == "mem" {
            for p in procs.iter_mut() {
                pc.read_rss(p);
            }
            procs.sort_by(|a, b| b.mem_pct.partial_cmp(&a.mem_pct).unwrap());
        } else {
            procs.sort_by(|a, b| b.cpu_pct.partial_cmp(&a.cpu_pct).unwrap());
        }
        procs.truncate(cli.procs);
        for p in procs.iter_mut() {
            pc.read_rss(p);
        }
        for p in procs.iter() {
            let (dcu, hbm) = match gpu_by_pid.get(&p.pid) {
                Some(gp) => (
                    if gp.gpu_ids.is_empty() {
                        "-".to_string()
                    } else {
                        gp.gpu_ids
                            .iter()
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    },
                    gp.vram_mb
                        .map(|v| format!("{:.1}G", v / 1024.0))
                        .unwrap_or_else(|| "-".into()),
                ),
                None => ("-".into(), "-".into()),
            };
            let cmd: String = proc::ProcCollector::read_cmd(p.pid)
                .chars()
                .take(60)
                .collect();
            println!(
                "{:<8}{:<12}{:>6.1}{:>7.1}  {:<8}{:>8}  {}",
                p.pid, p.user, p.cpu_pct, p.mem_pct, dcu, hbm, cmd
            );
        }
    }
    Ok(())
}
