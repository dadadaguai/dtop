use super::{GpuBackend, GpuProc, GpuSnapshot, GpuSample};
use anyhow::{Context, Result};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct SmiBackend {
    cmd: String,
}

const SMI_ARGS: &[&str] = &[
    "--showuse",
    "--showmemuse",
    "--showtemp",
    "--showpower",
    "--showclocks",
    "--showproductname",
    "--showmeminfo",
    "vram",
    "--showpids",
];

impl SmiBackend {
    pub fn new(cmd: &str) -> Self {
        Self { cmd: cmd.into() }
    }

    fn run(&self, json: bool) -> Result<String> {
        let mut args: Vec<&str> = SMI_ARGS.to_vec();
        if json {
            args.push("--json");
        }
        run_cmd_timeout(&self.cmd, &args, Duration::from_secs(5))
    }
}

impl GpuBackend for SmiBackend {
    fn name(&self) -> &str {
        &self.cmd
    }

    fn collect(&self) -> Result<GpuSnapshot> {
        // try JSON output first (much more robust to parse)
        if let Ok(out) = self.run(true) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) {
                let snap = parse_json(&v, &self.cmd);
                if !snap.gpus.is_empty() {
                    return Ok(snap);
                }
            }
            log::debug!("{}: --json output unparsable, falling back to text", self.cmd);
        }
        let out = match self.run(false) {
            Ok(out) => out,
            Err(e) => {
                let hint = if !super::dcu_device_present() {
                    " (hint: /dev/kfd not found - is the DCU driver loaded?)"
                } else {
                    ""
                };
                anyhow::bail!("{e}{hint}");
            }
        };
        let mut snap = parse_text(&out, &self.cmd);
        if snap.gpus.is_empty() && snap.error.is_none() {
            snap.error = Some(format!(
                "{} produced no GPU data (driver not loaded, no permission, or unsupported version)",
                self.cmd
            ));
        }
        Ok(snap)
    }
}

fn run_cmd_timeout(cmd: &str, args: &[&str], timeout: Duration) -> Result<String> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {cmd}"))?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let mut out = String::new();
            if let Some(mut so) = child.stdout.take() {
                let _ = so.read_to_string(&mut out);
            }
            if !out.is_empty() || status.success() {
                return Ok(out);
            }
            anyhow::bail!("{cmd} exited with {status}");
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("{cmd} timed out after {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn first_f64(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() || (bytes[i] == b'-' && i + 1 < bytes.len()) {
            let mut j = i;
            let mut dot = false;
            while j < bytes.len()
                && (bytes[j].is_ascii_digit() || (bytes[j] == b'.' && !dot && { dot = true; true }))
            {
                j += 1;
            }
            if let Ok(v) = s[i..j].parse::<f64>() {
                return Some(v);
            }
            i = j;
        }
        i += 1;
    }
    None
}

fn norm_mhz(v: f64) -> u64 {
    // smi may report Hz (e.g. 1700000000) or MHz (1700)
    if v > 100_000.0 {
        (v / 1_000_000.0) as u64
    } else {
        v as u64
    }
}

fn apply_kv(g: &mut GpuSample, key: &str, val: &str) {
    let k = key.to_lowercase();
    let v = val.trim();
    if k.contains("gpu use") || (k.contains("gpu") && k.contains("activity") && !k.contains("memory"))
    {
        if let Some(f) = first_f64(v) {
            g.util = Some(f);
        }
    } else if k.contains("memory activity") || k.contains("mem use") {
        if let (Some(f), Some(total)) = (first_f64(v), g.vram_total_mb) {
            g.vram_used_mb = Some(total * f / 100.0);
        } else if let Some(f) = first_f64(v) {
            // remember pct in used field scaled later if total arrives
            if g.vram_used_mb.is_none() {
                g.vram_used_mb = Some(-f); // negative marker = percent
            }
        }
    } else if k.contains("total used memory") || (k.contains("used") && k.contains("memory") && k.contains("b")) {
        if let Some(f) = first_f64(v) {
            g.vram_used_mb = Some(f / 1024.0 / 1024.0);
        }
    } else if k.contains("total memory") || (k.contains("vram") && k.contains("total")) {
        if let Some(f) = first_f64(v) {
            g.vram_total_mb = Some(f / 1024.0 / 1024.0);
        }
    } else if k.contains("temperature") || k.contains("temp") {
        if let Some(f) = first_f64(v) {
            // prefer edge sensor if multiple; keep first seen otherwise
            if g.temp_c.is_none() || k.contains("edge") {
                g.temp_c = Some(f);
            }
        }
    } else if k.contains("power") && (k.contains("(w)") || k.contains("watt") || v.contains('W')) {
        if let Some(f) = first_f64(v) {
            if g.power_w.is_none() || k.contains("socket") || k.contains("avg") {
                g.power_w = Some(f);
            }
        }
    } else if k.contains("sclk") {
        if let Some(f) = first_f64(v) {
            g.sclk_mhz = Some(norm_mhz(f));
        }
    } else if k.contains("mclk") {
        if let Some(f) = first_f64(v) {
            g.mclk_mhz = Some(norm_mhz(f));
        }
    } else if k.contains("card series") || k.contains("card model") || k.contains("sku") {
        if g.name.is_empty() || g.name == "DCU" {
            g.name = v.trim_matches('"').to_string();
        }
    }
}

fn finalize(g: &mut GpuSample) {
    // resolve percent marker
    if let (Some(used), Some(total)) = (g.vram_used_mb, g.vram_total_mb) {
        if used < 0.0 {
            g.vram_used_mb = Some(total * (-used) / 100.0);
        }
    } else if g.vram_used_mb.is_some() && g.vram_used_mb.unwrap() < 0.0 {
        g.vram_used_mb = None;
    }
    if g.name.is_empty() {
        g.name = "DCU".into();
    }
}

fn json_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn parse_json(v: &serde_json::Value, backend: &str) -> GpuSnapshot {
    let mut snap = GpuSnapshot {
        backend: backend.into(),
        ..Default::default()
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return snap,
    };

    let mut cards: Vec<(usize, GpuSample)> = Vec::new();
    for (key, val) in obj {
        let kl = key.to_lowercase();
        if !(kl.starts_with("card") || kl.starts_with("gpu")) {
            continue;
        }
        let idx = first_f64(key).unwrap_or(0.0) as usize;
        let mut g = GpuSample {
            index: idx,
            ..Default::default()
        };
        if let Some(fields) = val.as_object() {
            // two-pass: totals first so percent can resolve
            for (fk, fv) in fields {
                let fkl = fk.to_lowercase();
                if fkl.contains("total memory") || fkl.contains("total") && fkl.contains("vram") {
                    apply_kv(&mut g, fk, &json_str(fv));
                }
            }
            for (fk, fv) in fields {
                if fk.to_lowercase().contains("pid") {
                    continue;
                }
                apply_kv(&mut g, fk, &json_str(fv));
            }
            // nested pid table inside card object
            for (fk, fv) in fields {
                if fk.to_lowercase().contains("pid") || fk.to_lowercase().contains("process") {
                    collect_procs_json(fv, idx, &mut snap.procs);
                }
            }
        }
        finalize(&mut g);
        cards.push((idx, g));
    }
    cards.sort_by_key(|(i, _)| *i);
    snap.gpus = cards.into_iter().map(|(_, g)| g).collect();

    // top-level pid info
    for (key, val) in obj {
        let kl = key.to_lowercase();
        if kl.contains("pid") || kl.contains("process") {
            collect_procs_json(val, usize::MAX, &mut snap.procs);
        }
    }
    dedup_procs(&mut snap.procs);
    snap
}

fn collect_procs_json(v: &serde_json::Value, gpu_idx: usize, out: &mut Vec<GpuProc>) {
    match v {
        serde_json::Value::Object(map) => {
            let lower: std::collections::HashMap<String, &serde_json::Value> = map
                .iter()
                .map(|(k, val)| (k.to_lowercase(), val))
                .collect();
            if let Some(pid_v) = lower.get("pid") {
                if let Some(pid) = first_f64(&json_str(pid_v)) {
                    let name = lower
                        .get("process")
                        .or_else(|| lower.get("command"))
                        .map(|x| json_str(x))
                        .unwrap_or_default();
                    let vram_mb = lower
                        .get("vram usage")
                        .or_else(|| lower.get("vram"))
                        .or_else(|| lower.get("memory"))
                        .and_then(|x| parse_vram_str(&json_str(x)));
                    out.push(GpuProc {
                        pid: pid as u32,
                        name,
                        vram_mb,
                        gpu_ids: if gpu_idx == usize::MAX { vec![] } else { vec![gpu_idx] },
                    });
                    return;
                }
            }
            for val in map.values() {
                collect_procs_json(val, gpu_idx, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for val in arr {
                collect_procs_json(val, gpu_idx, out);
            }
        }
        _ => {}
    }
}

fn parse_vram_str(s: &str) -> Option<f64> {
    let f = first_f64(s)?;
    let sl = s.to_lowercase();
    if sl.contains("gb") || sl.contains(" g") {
        Some(f * 1024.0)
    } else if sl.contains("mb") || sl.contains(" m") {
        Some(f)
    } else if sl.contains("kb") {
        Some(f / 1024.0)
    } else if f > 1024.0 * 1024.0 {
        Some(f / 1024.0 / 1024.0) // bytes
    } else {
        Some(f)
    }
}

fn dedup_procs(procs: &mut Vec<GpuProc>) {
    let mut merged: Vec<GpuProc> = Vec::new();
    for p in procs.drain(..) {
        if let Some(m) = merged.iter_mut().find(|m| m.pid == p.pid) {
            for id in p.gpu_ids {
                if !m.gpu_ids.contains(&id) {
                    m.gpu_ids.push(id);
                }
            }
            if m.vram_mb.is_none() {
                m.vram_mb = p.vram_mb;
            }
            if m.name.is_empty() {
                m.name = p.name;
            }
        } else {
            merged.push(p);
        }
    }
    *procs = merged;
}

fn parse_text(out: &str, backend: &str) -> GpuSnapshot {
    let mut snap = GpuSnapshot {
        backend: backend.into(),
        ..Default::default()
    };
    let mut gpus: Vec<GpuSample> = Vec::new();
    let mut in_pids = false;

    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() || line.chars().all(|c| c == '=' || c == '-') {
            continue;
        }
        let ll = line.to_lowercase();
        if ll.contains("pid") && ll.contains("process") {
            in_pids = true;
            continue;
        }

        // GPU[i] : key: value
        if let Some(br) = line.find("GPU[") {
            let rest = &line[br + 4..];
            if let Some(cb) = rest.find(']') {
                if let Ok(idx) = rest[..cb].trim().parse::<usize>() {
                    while gpus.len() <= idx {
                        gpus.push(GpuSample {
                            index: gpus.len(),
                            ..Default::default()
                        });
                    }
                    let body = &rest[cb + 1..];
                    let body = body.trim_start_matches(|c| c == ' ' || c == ':');
                    if let Some((k, v)) = body.split_once(':') {
                        apply_kv(&mut gpus[idx], k, v);
                    }
                }
            }
            continue;
        }

        // pid table rows: "88213  python  31.2 GB"
        if in_pids {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() >= 2 {
                if let Ok(pid) = f[0].parse::<u32>() {
                    let vram = f[2..].join(" ");
                    snap.procs.push(GpuProc {
                        pid,
                        name: f[1].to_string(),
                        vram_mb: parse_vram_str(&vram),
                        gpu_ids: vec![],
                    });
                }
            }
        }
    }

    for g in gpus.iter_mut() {
        finalize(g);
    }
    dedup_procs(&mut snap.procs);
    snap.gpus = gpus;
    snap
}
