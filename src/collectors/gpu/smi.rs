use super::{GpuBackend, GpuProc, GpuSnapshot, GpuSample};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct SmiBackend {
    cmd: String,
    cache: Mutex<PidCache>,
}

struct PidCache {
    procs: Vec<GpuProc>,
    bus_map: HashMap<String, usize>,
    /// slowly-changing extras (Dec%/Enc%/Perf/Mode/Fan) from the default table
    extras: Vec<GpuSample>,
    counter: u64,
}

/// metrics args: --showpids is excluded because it does not support --json
/// on hy-smi (prints a warning and text), which would corrupt JSON output.
const SMI_ARGS: &[&str] = &[
    "--showuse",
    "--showmemuse",
    "--showtemp",
    "--showpower",
    "--showclocks",
    "--showproductname",
    "--showmeminfo",
    "vram",
];

/// how often (in collect cycles) to refresh the pid/bus tables
const PID_REFRESH_CYCLES: u64 = 5;

impl SmiBackend {
    pub fn new(cmd: &str) -> Self {
        Self {
            cmd: cmd.into(),
            cache: Mutex::new(PidCache {
                procs: Vec::new(),
                bus_map: HashMap::new(),
                extras: Vec::new(),
                counter: 0,
            }),
        }
    }

    fn run(&self, args: &[&str], json: bool) -> Result<String> {
        let mut full: Vec<&str> = args.to_vec();
        if json {
            full.push("--json");
        }
        run_cmd_timeout(&self.cmd, &full, Duration::from_secs(5))
    }

    fn collect_metrics(&self) -> Result<GpuSnapshot> {
        // try JSON output first (much more robust to parse)
        if let Ok(out) = self.run(SMI_ARGS, true) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&out) {
                let snap = parse_json(&v, &self.cmd);
                if !snap.gpus.is_empty() {
                    return Ok(snap);
                }
            }
            log::debug!("{}: --json output unparsable, falling back to text", self.cmd);
        }
        let out = match self.run(SMI_ARGS, false) {
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

impl GpuBackend for SmiBackend {
    fn name(&self) -> &str {
        &self.cmd
    }

    fn collect(&self) -> Result<GpuSnapshot> {
        let mut snap = self.collect_metrics()?;

        let mut cache = self.cache.lock().unwrap();
        cache.counter += 1;
        if cache.counter % PID_REFRESH_CYCLES == 1 {
            // pid table (text only; hy-smi --showpids does not support --json)
            match self.run(&["--showpids"], false) {
                Ok(out) => cache.procs = parse_pids(&out),
                Err(e) => log::debug!("{} --showpids failed: {e}", self.cmd),
            }
            // per-card PCI bus map, used to resolve process -> card index
            cache.bus_map = match self.run(&["--showbus"], false) {
                Ok(out) => parse_bus_map(&out),
                Err(e) => {
                    log::debug!("{} --showbus failed: {e}", self.cmd);
                    HashMap::new()
                }
            };
            // bare `hy-smi` concise table carries Dec%/Enc%/Perf/Mode which the
            // --show* flags above do not expose
            cache.extras = match self.run(&[], false) {
                Ok(out) => parse_default_table(&out),
                Err(e) => {
                    log::debug!("{} default table failed: {e}", self.cmd);
                    Vec::new()
                }
            };
        }
        // resolve card index via PCI bus when available (HCU Index in showpids
        // lists all visible cards, not the ones actually used).
        // Destructure so procs (mut), bus_map and extras (shared) are disjoint.
        let PidCache {
            procs,
            bus_map,
            extras,
            ..
        } = &mut *cache;
        for p in procs.iter_mut() {
            if !p.pci_bus.is_empty() {
                if let Some(idx) = bus_map.get(&p.pci_bus) {
                    p.gpu_ids = vec![*idx];
                }
            }
        }
        snap.procs = procs.clone();
        merge_extras(&mut snap.gpus, extras);
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

/// Last numeric token in a string (used to grab the frequency just before "Mhz").
fn last_f64(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    let mut result = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let mut j = i;
            let mut dot = false;
            while j < bytes.len()
                && (bytes[j].is_ascii_digit() || (bytes[j] == b'.' && !dot && { dot = true; true }))
            {
                j += 1;
            }
            if let Ok(v) = s[i..j].parse::<f64>() {
                result = Some(v);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    result
}

/// Parse a clock value into MHz. Handles three shapes:
///   "1500Mhz"            (json: sclk clock speed)
///   "10 (1500Mhz)"       (text: level + freq in parens)
///   "10"                 (json: sclk clock level -> reject, it's a level not a freq)
fn parse_clock_mhz(v: &str) -> Option<u64> {
    let vl = v.to_lowercase();
    if let Some(pos) = vl.find("mhz") {
        if let Some(f) = last_f64(&v[..pos]) {
            return Some(norm_mhz(f));
        }
    }
    // bare number: only accept if it looks like a frequency, not a small level index
    if let Some(f) = first_f64(v) {
        if f >= 100.0 {
            return Some(norm_mhz(f));
        }
    }
    None
}

/// Parse a memory value into MB, honoring unit hints in key or value.
fn parse_mem_mb(key: &str, val: &str) -> Option<f64> {
    let f = first_f64(val)?;
    let vl = val.to_lowercase();
    let kl = key.to_lowercase();
    if vl.contains("gb") || kl.contains("(gb)") {
        return Some(f * 1024.0);
    }
    if vl.contains("mb") || kl.contains("(mb)") {
        return Some(f);
    }
    if vl.contains("kb") || kl.contains("(kb)") {
        return Some(f / 1024.0);
    }
    if vl.contains('b') || kl.contains("(b)") {
        return Some(f / 1024.0 / 1024.0);
    }
    // no unit hint: guess by magnitude (bytes vs MB)
    if f >= 1_000_000.0 {
        Some(f / 1024.0 / 1024.0)
    } else {
        Some(f)
    }
}

fn is_na(v: &str) -> bool {
    let vl = v.to_lowercase();
    vl == "n/a" || vl == "na" || vl.contains("not supported") || vl.contains("not available") || vl == "-"
}

fn apply_kv(g: &mut GpuSample, key: &str, val: &str) {
    let k = key.to_lowercase();
    let v = val.trim().trim_matches('"');
    if v.is_empty() {
        return;
    }
    let na = is_na(v);

    // NOTE: order matters. "total used memory" contains both "used" and "total",
    // so check used/usage before total. "memory activity" must be checked before
    // generic "gpu activity".
    if !na && (k.contains("used") || k.contains("usage")) && (k.contains("memory") || k.contains("vram")) {
        if let Some(mb) = parse_mem_mb(&k, v) {
            g.vram_used_mb = Some(mb);
        }
    } else if !na && k.contains("total") && (k.contains("memory") || k.contains("vram")) {
        if let Some(mb) = parse_mem_mb(&k, v) {
            g.vram_total_mb = Some(mb);
        }
    } else if !na
        && (k.contains("memory activity")
            || k.contains("mem use")
            || (k.contains("vram") && k.contains('%'))
            || (k.contains("memory") && k.contains("%") && !k.contains("total")))
    {
        if let Some(f) = first_f64(v) {
            g.vram_pct = Some(f);
        }
    } else if !na
        && (k.contains("gpu use")
            || k.contains("gpu util")
            || k.contains("gpu activity")
            || k.contains("hcu use")
            || k.contains("hcu util")
            || k.contains("hcu activity")
            || k.contains("dcu use")
            || k.contains("dcu util")
            || k.contains("utilization")
            || (k.contains("use (%)") && !k.contains("memory") && !k.contains("mem")))
    {
        if let Some(f) = first_f64(v) {
            g.util = Some(f);
        }
    } else if !na && (k.contains("temperature") || k.contains("temp")) {
        if let Some(f) = first_f64(v) {
            // prefer edge sensor if multiple; keep first seen otherwise
            if g.temp_c.is_none() || k.contains("edge") {
                g.temp_c = Some(f);
            }
        }
    } else if !na
        && k.contains("power")
        && !k.contains("cap")
        && !k.contains("limit")
        && !k.contains("max")
        && (k.contains("(w)") || k.contains("watt") || v.to_lowercase().contains('w'))
    {
        if let Some(f) = first_f64(v) {
            // prefer total package/socket power over per-rail (GFX core / memory) power
            let better = k.contains("package") || k.contains("socket") || k.contains("tdp");
            if g.power_w.is_none() || better {
                g.power_w = Some(f);
            }
        }
    } else if !na && k.contains("sclk") {
        if let Some(mhz) = parse_clock_mhz(v) {
            g.sclk_mhz = Some(mhz);
        }
    } else if !na && k.contains("mclk") {
        if let Some(mhz) = parse_clock_mhz(v) {
            g.mclk_mhz = Some(mhz);
        }
    } else if !na && (k == "dec%" || k.contains("decoder") || k.contains("decode")) {
        if let Some(f) = first_f64(v) {
            g.dec_pct = Some(f);
        }
    } else if !na && (k == "enc%" || k.contains("encoder") || k.contains("encode")) {
        if let Some(f) = first_f64(v) {
            g.enc_pct = Some(f);
        }
    } else if !na && k.contains("perf") {
        g.perf_level = Some(v.to_string());
    } else if !na && k.contains("mode") && !k.contains("model") {
        g.mode = Some(v.to_string());
    } else if !na && k.contains("fan") {
        // "45%" -> percent, "1500 RPM" -> rpm
        let vl = v.to_lowercase();
        if vl.contains("rpm") {
            if let Some(f) = first_f64(v) {
                g.fan_rpm = Some(f as u64);
            }
        } else if let Some(f) = first_f64(v) {
            g.fan_pct = Some(f);
        }
    } else if !na
        && k.contains("pcie")
        && !k.contains("clock")
        && (k.contains("mb/s") || k.contains("mbps") || k.contains("bandwidth") || k.contains("throughput"))
    {
        // PCIe link throughput, e.g. "PCIe Tx (MB/s)" / "PCIe Bandwidth"
        if k.contains("tx") || k.contains("txn") || k.contains("transmit") {
            if let Some(f) = first_f64(v) {
                g.pcie_tx_mbps = Some(f);
            }
        } else if k.contains("rx") || k.contains("receive") {
            if let Some(f) = first_f64(v) {
                g.pcie_rx_mbps = Some(f);
            }
        } else if let Some(f) = first_f64(v) {
            // single bandwidth value: record on the tx side
            if g.pcie_tx_mbps.is_none() {
                g.pcie_tx_mbps = Some(f);
            }
        }
    } else if !na && (k.contains("card model") || k.contains("model")) {
        g.name_model = v.to_string();
    } else if !na && (k.contains("card series") || k.contains("series")) {
        g.name_series = v.to_string();
    } else if !na && k.contains("sku") {
        g.name_sku = v.to_string();
    }
}

fn finalize(g: &mut GpuSample) {
    // resolve vram percent into absolute MB when total is known
    if g.vram_used_mb.is_none() {
        if let (Some(pct), Some(total)) = (g.vram_pct, g.vram_total_mb) {
            g.vram_used_mb = Some(total * pct / 100.0);
        }
    }
    // name resolution: model > series > sku, ignore placeholder junk
    let pick = [(&g.name_model, 3), (&g.name_series, 2), (&g.name_sku, 1)]
        .into_iter()
        .filter(|(s, _)| !s.is_empty() && !is_na(s))
        .max_by_key(|(_, prio)| *prio)
        .map(|(s, _)| s.clone());
    g.name = pick.unwrap_or_else(|| "DCU".into());
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
                        ..Default::default()
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

/// Find a card-index prefix like "GPU[0]", "HCU[2]", "DCU[1]" in a line.
/// Returns (index, byte offset just after the ']').
fn parse_card_idx(line: &str) -> Option<(usize, usize)> {
    for tag in ["GPU[", "HCU[", "DCU["] {
        if let Some(p) = line.find(tag) {
            let rest = &line[p + tag.len()..];
            if let Some(cb) = rest.find(']') {
                if let Ok(idx) = rest[..cb].trim().parse::<usize>() {
                    return Some((idx, p + tag.len() + cb + 1));
                }
            }
        }
    }
    None
}

/// Extract a PCI bus id token like "0000:09:00.0" and normalize it
/// (strip the all-zero domain so "0000:09:00.0" -> "09:00.0").
fn extract_pci(s: &str) -> Option<String> {
    for tok in s.split(|c: char| !c.is_ascii_hexdigit() && c != ':' && c != '.') {
        let parts: Vec<&str> = tok.split(':').collect();
        if parts.len() == 3 && parts[2].contains('.') && !parts[0].is_empty() {
            let lower = tok.to_lowercase();
            return Some(
                lower
                    .strip_prefix("0000:")
                    .unwrap_or(&lower)
                    .to_string(),
            );
        }
    }
    None
}

/// Extract all bracketed numbers from a string like "['0'] ['1'] ['7']".
fn extract_bracket_nums(s: &str) -> Vec<usize> {
    s.split('[')
        .skip(1)
        .filter_map(|p| {
            let end = p.find(']')?;
            p[..end].trim().trim_matches('\'').parse::<usize>().ok()
        })
        .collect()
}

/// Parse hy-smi --showpids block format:
///
/// PID: 3195716
///     PASID: 32769
///     HCU Index: ['0'] ['1'] ...
///     PCI BUS: ['0000:09:00.0']
///     VRAM USED(MiB): 442
///
/// Also tolerates the rocm-smi table format ("PID PROCESS VRAM USAGE" header
/// followed by "88213 python 31.2 GB" rows).
fn parse_pids(out: &str) -> Vec<GpuProc> {
    let mut procs: Vec<GpuProc> = Vec::new();
    let mut cur: Option<GpuProc> = None;
    let mut table_mode = false;

    for line in out.lines() {
        let t = line.trim();
        if t.is_empty() || t.chars().all(|c| c == '=' || c == '-') {
            continue;
        }
        let tl = t.to_lowercase();

        if let Some(rest) = t.strip_prefix("PID:") {
            if let Some(c) = cur.take() {
                procs.push(c);
            }
            if let Some(pid) = first_f64(rest) {
                cur = Some(GpuProc {
                    pid: pid as u32,
                    ..Default::default()
                });
            }
            continue;
        }
        if tl.contains("pid") && tl.contains("process") {
            table_mode = true;
            continue;
        }

        if let Some(c) = cur.as_mut() {
            if tl.starts_with("vram used(mib)") || (tl.contains("vram used") && tl.contains("mib")) {
                c.vram_mb = first_f64(t);
            } else if tl.starts_with("vram used(%)") || (tl.contains("vram used") && tl.contains("(%)")) {
                // percent only; absolute MiB preferred, keep if absent
            } else if tl.starts_with("pci bus") {
                if let Some(b) = extract_pci(t) {
                    c.pci_bus = b;
                }
            } else if tl.starts_with("hcu index") || tl.starts_with("gpu index") || tl.starts_with("dcu index") {
                c.gpu_ids = extract_bracket_nums(t);
            }
            continue;
        }

        // rocm-smi table rows: "88213  python  31.2 GB"
        if table_mode {
            let f: Vec<&str> = t.split_whitespace().collect();
            if f.len() >= 2 {
                if let Ok(pid) = f[0].parse::<u32>() {
                    let vram = f[2..].join(" ");
                    procs.push(GpuProc {
                        pid,
                        name: f[1].to_string(),
                        vram_mb: parse_vram_str(&vram),
                        ..Default::default()
                    });
                }
            }
        }
    }
    if let Some(c) = cur.take() {
        procs.push(c);
    }
    dedup_procs(&mut procs);
    procs
}

/// Parse `--showbus` output into normalized-PCI -> card index map.
fn parse_bus_map(out: &str) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for line in out.lines() {
        if let Some((idx, start)) = parse_card_idx(line) {
            if let Some(pci) = extract_pci(&line[start..]) {
                m.insert(pci, idx);
            }
        }
    }
    m
}

/// Parse the concise table produced by bare `hy-smi`:
///
/// HCU   Temp   AvgPwr   Perf   PwrCap   VRAM%   HCU%   Dec%   Enc%   Mode
/// 0     52.0C  89.0W    manual 1000.0W  2%      0.0%   0.0%   0.0%   Normal
///
/// Carries Dec%/Enc%/Perf/Mode, which the --show* flags dtop normally calls
/// do not expose.
fn parse_default_table(out: &str) -> Vec<GpuSample> {
    let mut header: Option<Vec<String>> = None;
    let mut gpus: Vec<GpuSample> = Vec::new();

    for line in out.lines() {
        let t = line.trim();
        if t.is_empty() || t.chars().all(|c| c == '=' || c == '-') {
            continue;
        }
        let cols: Vec<&str> = t.split_whitespace().collect();
        let lower = t.to_lowercase();

        if header.is_none() {
            if (lower.contains("hcu") || lower.contains("gpu") || lower.contains("dcu"))
                && lower.contains("temp")
                && cols.len() >= 4
            {
                header = Some(cols.iter().map(|c| c.to_lowercase()).collect());
            }
            continue;
        }
        let h = header.as_ref().unwrap();
        if cols.len() < h.len() {
            continue;
        }
        let idx: usize = match cols[0].parse() {
            Ok(i) => i,
            Err(_) => continue,
        };
        let mut g = GpuSample {
            index: idx,
            ..Default::default()
        };
        for (i, key) in h.iter().enumerate() {
            if i == 0 || i > cols.len() {
                continue; // column 0 is the card index
            }
            // the last column can contain spaces (e.g. Mode = "Power Saving")
            let val_owned;
            let val: &str = if i == h.len() - 1 {
                val_owned = cols[i..].join(" ");
                &val_owned
            } else {
                cols[i]
            };
            match key.as_str() {
                "hcu%" | "gpu%" | "dcu%" | "util%" => g.util = first_f64(val),
                "vram%" | "mem%" => g.vram_pct = first_f64(val),
                "dec%" => g.dec_pct = first_f64(val),
                "enc%" => g.enc_pct = first_f64(val),
                "temp" | "temp(c)" => g.temp_c = first_f64(val),
                "avgpwr" | "avgpwr(w)" | "power" | "power(w)" => g.power_w = first_f64(val),
                "perf" => g.perf_level = Some(val.to_string()),
                "mode" => g.mode = Some(val.to_string()),
                "fan" | "fan%" => {
                    let vl = val.to_lowercase();
                    if vl.contains("rpm") {
                        g.fan_rpm = first_f64(val).map(|f| f as u64);
                    } else {
                        g.fan_pct = first_f64(val);
                    }
                }
                _ => apply_kv(&mut g, key, val),
            }
        }
        finalize(&mut g);
        gpus.push(g);
    }
    gpus
}

/// Copy slowly-changing extras (Dec/Enc/Perf/Mode/Fan) from the default-table
/// snapshot into the main samples, matched by card index.
fn merge_extras(gpus: &mut [GpuSample], extras: &[GpuSample]) {
    for g in gpus.iter_mut() {
        if let Some(e) = extras.iter().find(|e| e.index == g.index) {
            if g.dec_pct.is_none() {
                g.dec_pct = e.dec_pct;
            }
            if g.enc_pct.is_none() {
                g.enc_pct = e.enc_pct;
            }
            if g.perf_level.is_none() {
                g.perf_level = e.perf_level.clone();
            }
            if g.mode.is_none() {
                g.mode = e.mode.clone();
            }
            if g.fan_pct.is_none() {
                g.fan_pct = e.fan_pct;
            }
            if g.fan_rpm.is_none() {
                g.fan_rpm = e.fan_rpm;
            }
        }
    }
}

fn parse_text(out: &str, backend: &str) -> GpuSnapshot {
    let mut snap = GpuSnapshot {
        backend: backend.into(),
        ..Default::default()
    };
    let mut gpus: Vec<GpuSample> = Vec::new();

    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() || line.chars().all(|c| c == '=' || c == '-') {
            continue;
        }

        // GPU[i]/HCU[i]/DCU[i] : key: value
        if let Some((idx, start)) = parse_card_idx(line) {
            while gpus.len() <= idx {
                gpus.push(GpuSample {
                    index: gpus.len(),
                    ..Default::default()
                });
            }
            let body = &line[start..];
            let body = body.trim_start_matches(|c: char| c.is_whitespace() || c == ':' || c == '\t');
            if let Some((k, v)) = body.split_once(':') {
                apply_kv(&mut gpus[idx], k, v);
            }
            continue;
        }

        // concise table fallback (default hy-smi output):
        // HCU  Temp  AvgPwr  Perf  PwrCap  VRAM%  HCU% ...
        // 0    52.0C 89.0W   ...   1000.0W 2%     0.0%
    }

    for g in gpus.iter_mut() {
        finalize(g);
    }
    snap.gpus = gpus;
    snap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_clock_mhz() {
        assert_eq!(parse_clock_mhz("1500Mhz"), Some(1500));
        assert_eq!(parse_clock_mhz("10 (1500Mhz)"), Some(1500));
        assert_eq!(parse_clock_mhz("0 (1800Mhz)"), Some(1800));
        assert_eq!(parse_clock_mhz("10"), None); // bare level index, not a freq
        assert_eq!(parse_clock_mhz("1500000000"), Some(1500)); // Hz -> MHz
    }

    #[test]
    fn test_parse_mem_mb() {
        // MiB key, bare number value -> treated as MB by magnitude heuristic
        assert_eq!(parse_mem_mb("vram Total Memory (MiB)", "65520"), Some(65520.0));
        assert_eq!(parse_mem_mb("vram Total Used Memory (MiB)", "998"), Some(998.0));
        // explicit GB
        assert_eq!(parse_mem_mb("Total Memory (GB)", "64"), Some(64.0 * 1024.0));
        // bytes
        assert_eq!(parse_mem_mb("Total Memory (B)", "68719476736"), Some(65536.0));
    }

    #[test]
    fn test_extract_pci_and_normalize() {
        assert_eq!(extract_pci("['0000:09:00.0']").as_deref(), Some("09:00.0"));
        assert_eq!(extract_pci("PCI Bus: 0000:b5:00.0").as_deref(), Some("b5:00.0"));
        assert_eq!(extract_pci("no bus here"), None);
    }

    #[test]
    fn test_extract_bracket_nums() {
        assert_eq!(extract_bracket_nums("['0'] ['1'] ['7']"), vec![0, 1, 7]);
        assert_eq!(extract_bracket_nums("['8']"), vec![8]);
    }

    #[test]
    fn test_parse_pids_block_format() {
        let out = "\
PIDs for KFD processes:

PID: 3195716
	PASID: 32769
	HCU Index: ['0'] ['1'] ['2'] ['3'] ['4'] ['5'] ['6'] ['7'] 
	PCI BUS: ['0000:09:00.0'] 
	VRAM USED(MiB): 442
	VRAM USED(%): 1

PID: 2988878
	PCI BUS: ['0000:b5:00.0'] 
	VRAM USED(MiB): 484
";
        let procs = parse_pids(out);
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0].pid, 3195716);
        assert_eq!(procs[0].vram_mb, Some(442.0));
        assert_eq!(procs[0].pci_bus, "09:00.0");
        assert_eq!(procs[0].gpu_ids, vec![0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(procs[1].pid, 2988878);
        assert_eq!(procs[1].pci_bus, "b5:00.0");
    }

    #[test]
    fn test_parse_bus_map() {
        let out = "HCU[0]\t\t: PCI Bus: 0000:09:00.0\nHCU[7]\t\t: PCI Bus: 0000:b5:00.0\n";
        let m = parse_bus_map(out);
        assert_eq!(m.get("09:00.0"), Some(&0));
        assert_eq!(m.get("b5:00.0"), Some(&7));
    }

    #[test]
    fn test_apply_kv_hcu_use_and_power_priority() {
        let mut g = GpuSample::default();
        // power: package power must win over per-rail GFX/memory power
        apply_kv(&mut g, "Average Graphics Package Power (W)", "89.0");
        apply_kv(&mut g, "Average GFX Core Power (W)", "22.0");
        apply_kv(&mut g, "Average Memory Power (W)", "36.0");
        assert_eq!(g.power_w, Some(89.0));
        // util from HCU use (%)
        apply_kv(&mut g, "HCU use (%)", "42.5");
        assert_eq!(g.util, Some(42.5));
        // vram pct from HCU memory use (%)
        apply_kv(&mut g, "HCU memory use (%)", "2");
        assert_eq!(g.vram_pct, Some(2.0));
        // temp prefers edge sensor
        apply_kv(&mut g, "Temperature (Sensor junction) (C)", "56.0");
        apply_kv(&mut g, "Temperature (Sensor edge) (C)", "53.0");
        assert_eq!(g.temp_c, Some(53.0));
    }

    #[test]
    fn test_apply_kv_power_cap_excluded() {
        let mut g = GpuSample::default();
        apply_kv(&mut g, "Power Cap (W)", "1000.0");
        assert_eq!(g.power_w, None); // cap must not be treated as draw
        apply_kv(&mut g, "Average Graphics Package Power (W)", "89.0");
        assert_eq!(g.power_w, Some(89.0));
    }

    #[test]
    fn test_finalize_vram_pct_and_name() {
        let mut g = GpuSample::default();
        g.vram_total_mb = Some(65520.0);
        g.vram_pct = Some(2.0);
        g.name_series = "BW".into();
        finalize(&mut g);
        assert!((g.vram_used_mb.unwrap() - 1310.4).abs() < 0.1);
        assert_eq!(g.name, "BW");
    }

    #[test]
    fn test_parse_text_hcu_prefix() {
        let out = "\
HCU[0]\t\t: HCU use (%): 42.0
HCU[0]\t\t: vram Total Memory (MiB): 65520
HCU[0]\t\t: vram Total Used Memory (MiB): 998
HCU[0]\t\t: Temperature (Sensor edge) (C): 53.0
HCU[0]\t\t: Average Graphics Package Power (W): 89.0
HCU[0]\t\t: sclk clock level: 10 (1500Mhz)
HCU[0]\t\t: Card Series:\t\t BW
";
        let snap = parse_text(out, "hy-smi");
        assert_eq!(snap.gpus.len(), 1);
        let g = &snap.gpus[0];
        assert_eq!(g.util, Some(42.0));
        assert_eq!(g.vram_total_mb, Some(65520.0));
        assert_eq!(g.vram_used_mb, Some(998.0));
        assert_eq!(g.temp_c, Some(53.0));
        assert_eq!(g.power_w, Some(89.0));
        assert_eq!(g.sclk_mhz, Some(1500));
        assert_eq!(g.name, "BW");
    }
}

#[cfg(test)]
mod tests_extras {
    use super::*;

    #[test]
    fn test_parse_default_table() {
        let out = "\
================================= System Management Interface ==================================
HCU     Temp     AvgPwr     Perf     PwrCap     VRAM%      HCU%      Dec%      Enc%      Mode     
0       52.0C    89.0W      manual   1000.0W    2%         7.5%      12.0%     3.0%      Normal   
1       77.0C    210.0W     auto     1000.0W    55%        91.0%     0.0%      0.0%      Power Saving
================================================================================================
";
        let gpus = parse_default_table(out);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].index, 0);
        assert_eq!(gpus[0].util, Some(7.5));
        assert_eq!(gpus[0].vram_pct, Some(2.0));
        assert_eq!(gpus[0].dec_pct, Some(12.0));
        assert_eq!(gpus[0].enc_pct, Some(3.0));
        assert_eq!(gpus[0].temp_c, Some(52.0));
        assert_eq!(gpus[0].power_w, Some(89.0));
        assert_eq!(gpus[0].perf_level.as_deref(), Some("manual"));
        assert_eq!(gpus[0].mode.as_deref(), Some("Normal"));
        assert_eq!(gpus[1].mode.as_deref(), Some("Power Saving"));
        assert_eq!(gpus[1].util, Some(91.0));
    }

    #[test]
    fn test_apply_kv_dec_enc_perf_mode_fan() {
        let mut g = GpuSample::default();
        apply_kv(&mut g, "Dec%", "12.0");
        apply_kv(&mut g, "Enc%", "3.5");
        apply_kv(&mut g, "Perf Level", "manual");
        apply_kv(&mut g, "Mode", "Normal");
        apply_kv(&mut g, "Fan Speed", "45%");
        assert_eq!(g.dec_pct, Some(12.0));
        assert_eq!(g.enc_pct, Some(3.5));
        assert_eq!(g.perf_level.as_deref(), Some("manual"));
        assert_eq!(g.mode.as_deref(), Some("Normal"));
        assert_eq!(g.fan_pct, Some(45.0));
    }

    #[test]
    fn test_mode_does_not_clobber_model() {
        let mut g = GpuSample::default();
        apply_kv(&mut g, "Card Model", "K100-AI");
        apply_kv(&mut g, "Mode", "Power Saving");
        finalize(&mut g);
        assert_eq!(g.name, "K100-AI");
        assert_eq!(g.mode.as_deref(), Some("Power Saving"));
    }

    #[test]
    fn test_pcie_clock_not_mistaken_for_bandwidth() {
        let mut g = GpuSample::default();
        apply_kv(&mut g, "pcie clock level", "2");
        apply_kv(&mut g, "pcie clock speed", "32.0GT/s, x16 1100Mhz");
        assert_eq!(g.pcie_tx_mbps, None);
        assert_eq!(g.pcie_rx_mbps, None);
        // a real bandwidth key is still accepted
        apply_kv(&mut g, "PCIe Tx (MB/s)", "1536");
        assert_eq!(g.pcie_tx_mbps, Some(1536.0));
    }

    #[test]
    fn test_merge_extras() {
        let mut gpus = vec![GpuSample { index: 0, ..Default::default() }];
        let extras = vec![GpuSample {
            index: 0,
            dec_pct: Some(1.0),
            enc_pct: Some(2.0),
            mode: Some("Normal".into()),
            ..Default::default()
        }];
        merge_extras(&mut gpus, &extras);
        assert_eq!(gpus[0].dec_pct, Some(1.0));
        assert_eq!(gpus[0].enc_pct, Some(2.0));
    }
}
