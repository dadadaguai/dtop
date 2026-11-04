use crate::app::{App, SortKey, WarnLevel};
use crate::collectors::gpu::GpuSample;
use crate::theme::{self, Theme};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use std::collections::VecDeque;

// ---- severity thresholds (match dcutop's theme.py) ----
const TEMP_RED_C: f64 = 75.0;
const TEMP_YELLOW_C: f64 = 65.0;
const POWER_RED_FRAC: f64 = 0.9;
const POWER_YELLOW_FRAC: f64 = 0.8;
const BAR_GREEN_BELOW: f64 = 60.0;
const BAR_YELLOW_BELOW: f64 = 85.0;

const NA: &str = "N/A";

fn sp(s: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(color))
}

fn spb(s: impl Into<String>, color: Color) -> Span<'static> {
    Span::styled(s.into(), Style::default().fg(color).add_modifier(Modifier::BOLD))
}

fn temp_style(t: Option<f64>, th: &Theme) -> Color {
    match t {
        Some(v) if v >= TEMP_RED_C => th.red,
        Some(v) if v >= TEMP_YELLOW_C => th.yellow,
        _ => th.text,
    }
}

fn power_style(p: Option<f64>, cap: Option<f64>, th: &Theme) -> Color {
    match (p, cap) {
        (Some(v), Some(c)) if c > 0.0 && v >= c * POWER_RED_FRAC => th.red,
        (Some(v), Some(c)) if c > 0.0 && v >= c * POWER_YELLOW_FRAC => th.yellow,
        _ => th.text,
    }
}

fn bar_style(pct: f64, th: &Theme) -> Color {
    if pct >= BAR_YELLOW_BELOW {
        th.red
    } else if pct >= BAR_GREEN_BELOW {
        th.yellow
    } else {
        th.green
    }
}

// ---- formatting (match formatter.py) ----
fn fmt_percent(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}%")).unwrap_or_else(|| NA.into())
}

fn fmt_temp(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}C")).unwrap_or_else(|| NA.into())
}

fn fmt_power(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.0}W")).unwrap_or_else(|| NA.into())
}

fn fmt_clock(v: Option<u64>) -> String {
    v.map(|x| format!("{x:.0}M")).unwrap_or_else(|| NA.into())
}

fn fmt_mem(mb: Option<f64>) -> String {
    mb.map(|v| {
        if v >= 1024.0 * 1024.0 {
            format!("{:.1}T", v / 1024.0 / 1024.0)
        } else if v >= 1024.0 {
            format!("{:.1}G", v / 1024.0)
        } else {
            format!("{:.0}M", v)
        }
    })
    .unwrap_or_else(|| NA.into())
}

fn fmt_mem_pair(used: Option<f64>, total: Option<f64>) -> String {
    match total {
        Some(t) => {
            let (u, unit) = if t >= 1024.0 * 1024.0 {
                (used.map(|v| v / 1024.0 / 1024.0), "T")
            } else if t >= 1024.0 {
                (used.map(|v| v / 1024.0), "G")
            } else {
                (used, "M")
            };
            let u = u.map(|v| format!("{v:.1}")).unwrap_or_else(|| NA.into());
            format!("{u}/{:.1}{unit}", t / if unit == "T" { 1024.0 * 1024.0 } else if unit == "G" { 1024.0 } else { 1.0 })
        }
        None => format!("{}/N/A", fmt_mem(used)),
    }
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

// ---- braille dot-matrix chart (match braille.py) ----
const BRAILLE_BASE: u32 = 0x2800;
// one char = one sample; 8 levels via two stacked text rows
const LEVEL_BITS: [u8; 4] = [0xC0, 0x30, 0x0C, 0x03];

fn sample_glyph(pct: f64) -> (u8, u8) {
    let filled = (((pct / 100.0) * 8.0).round() as i64).clamp(1, 8) as usize;
    let (mut bottom, mut top) = (0u8, 0u8);
    for level in 0..filled {
        if level < 4 {
            bottom |= LEVEL_BITS[level];
        } else {
            top |= LEVEL_BITS[level - 4];
        }
    }
    (bottom, top)
}

fn downsample(values: &[f64], width: usize) -> Vec<f64> {
    if width == 0 || values.len() <= width {
        return values.to_vec();
    }
    let bucket = values.len() as f64 / width as f64;
    let mut out = Vec::with_capacity(width);
    for i in 0..width {
        let start = (i as f64 * bucket) as usize;
        let end = ((start + 1).max(((i + 1) as f64 * bucket) as usize)).min(values.len());
        let s = &values[start..end];
        out.push(s.iter().sum::<f64>() / s.len() as f64);
    }
    out
}

/// (top_row, bottom_row) two-line braille waveform.
fn braille_chart(values: &[f64], width: usize) -> (String, String) {
    let mut sampled = downsample(values, width);
    if sampled.len() < width && !sampled.is_empty() {
        let first = sampled[0];
        let mut padded = vec![first; width - sampled.len()];
        padded.extend(sampled);
        sampled = padded;
    }
    let mut top = String::new();
    let mut bottom = String::new();
    for v in sampled {
        let (b, t) = sample_glyph(v);
        bottom.push(char::from_u32(BRAILLE_BASE + b as u32).unwrap_or(' '));
        top.push(char::from_u32(BRAILLE_BASE + t as u32).unwrap_or(' '));
    }
    (top, bottom)
}

fn axis_line(width: usize, interval_s: f64) -> String {
    let span_s = width as f64 * interval_s;
    let marks = [120u32, 60, 30];
    let mut pieces: Vec<(usize, String)> = Vec::new();
    for mark in marks {
        if (mark as f64) <= span_s {
            let col = width - (mark as f64 / interval_s) as usize;
            pieces.push((col, format!("|{mark}s")));
        }
    }
    if pieces.is_empty() {
        return String::new();
    }
    let mut line = String::new();
    let mut cursor = 0usize;
    for (col, label) in pieces {
        let start = cursor.max(col);
        line.push_str(&" ".repeat(start - cursor));
        line.push_str(&label);
        cursor = start + label.chars().count();
    }
    line
}

fn avg_series(hists: &[VecDeque<f64>]) -> Vec<f64> {
    let n = hists.first().map(|h| h.len()).unwrap_or(0);
    (0..n)
        .map(|i| hists.iter().filter_map(|h| h.get(i)).sum::<f64>() / hists.len() as f64)
        .collect()
}

// ---- box drawing (match panels.py _box*) ----
fn box_top(title: &str, width: usize, th: &Theme) -> Line<'static> {
    if title.is_empty() {
        Line::from(sp(format!("┌{}┐", "─".repeat(width.saturating_sub(2))), th.border))
    } else {
        let fill = width.saturating_sub(5 + title.chars().count());
        let mut spans = vec![sp("┌─ ", th.border)];
        spans.push(spb(title.to_string(), th.title));
        spans.push(sp(format!(" {}┐", "─".repeat(fill)), th.border));
        Line::from(spans)
    }
}

fn box_bottom(width: usize, th: &Theme) -> Line<'static> {
    Line::from(sp(format!("└{}┘", "─".repeat(width.saturating_sub(2))), th.border))
}

fn box_sep(width: usize, th: &Theme) -> Line<'static> {
    Line::from(sp(format!("├{}┤", "─".repeat(width.saturating_sub(2))), th.border))
}

fn box_row(line: Line<'static>, width: usize, th: &Theme) -> Line<'static> {
    let inner = width.saturating_sub(4);
    let text_len: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = inner.saturating_sub(text_len);
    let mut spans = vec![sp("│ ", th.border)];
    spans.extend(line.spans);
    spans.push(sp(format!("{} │", " ".repeat(pad)), th.border));
    Line::from(spans)
}

// ---- Devices panel ----
fn bar_cell(pct: Option<f64>, width: usize, suffix: String, th: &Theme) -> Span<'static> {
    let (filled, color) = match pct {
        Some(p) => (
            ((p / 100.0 * width as f64).round() as usize).min(width),
            bar_style(p, th),
        ),
        None => (0, th.dim),
    };
    let blocks = "█".repeat(filled);
    sp(format!("▏{blocks:<width$} {suffix}", width = width), color)
}

fn device_header(util_bar: usize, mem_bar: usize, th: &Theme) -> Vec<Span<'static>> {
    let h = th.text;
    vec![
        spb(format!("{:>3}  ", "HCU"), h),
        spb(format!("{:<10}  ", "Model"), h),
        spb(format!("{:>6}  ", "Temp"), h),
        spb(format!("{:>7}  ", "Power"), h),
        spb(format!("{:<width$}  ", "HCU%", width = util_bar + 8), h),
        spb(format!("{:>6}  ", "CU%"), h),
        spb(format!("{:<width$}  ", "VRAM", width = mem_bar + 14), h),
        spb(format!("{:>6}  ", "SCLK"), h),
        spb(format!("{:>6}", "MCLK"), h),
    ]
}

fn device_row(g: &GpuSample, util_bar: usize, mem_bar: usize, th: &Theme) -> Vec<Span<'static>> {
    let mem_pct = match (g.vram_used_mb, g.vram_total_mb) {
        (Some(u), Some(t)) if t > 0.0 => Some(u / t * 100.0),
        _ => None,
    };
    vec![
        sp(format!("{:>3}  ", g.index), th.text),
        sp(format!("{:<10}  ", short_model(&g.name)), th.text),
        sp(format!("{:>6}  ", fmt_temp(g.temp_c)), temp_style(g.temp_c, th)),
        sp(format!("{:>7}  ", fmt_power(g.power_w)), power_style(g.power_w, g.power_cap_w, th)),
        bar_cell(g.util, util_bar, format!("{:>6}", fmt_percent(g.util)), th),
        sp("  ", th.text),
        sp(format!("{:>6}  ", fmt_percent(g.cu_util)), th.text),
        bar_cell(mem_pct, mem_bar, format!("{:>12}", fmt_mem_pair(g.vram_used_mb, g.vram_total_mb)), th),
        sp("  ", th.text),
        sp(format!("{:>6}  ", fmt_clock(g.sclk_mhz)), th.text),
        sp(format!("{:>6}", fmt_clock(g.mclk_mhz)), th.dim),
    ]
}

fn device_lines(app: &App, th: &Theme, inner_w: usize) -> Vec<Line<'static>> {
    let cards: Vec<&GpuSample> = match &app.filter {
        Some(f) => app.gpu.gpus.iter().filter(|g| f.contains(&g.index)).collect(),
        None => app.gpu.gpus.iter().collect(),
    };

    // size the two bar columns to fill the inner width (match DeviceLayout.for_width)
    let (mut util_bar, mut mem_bar) = (16usize, 14usize);
    let base = Line::from(device_header(util_bar, mem_bar, th))
        .spans
        .iter()
        .map(|s| s.content.chars().count())
        .sum::<usize>();
    let extra = inner_w as isize - base as isize;
    if extra >= 0 {
        util_bar += (extra / 2) as usize;
        mem_bar += (extra - extra / 2) as usize;
    } else {
        util_bar = 6usize.max((16isize + extra / 2) as usize);
        mem_bar = 6usize.max((14isize + extra - extra / 2) as usize);
    }

    let mut lines = vec![Line::from(device_header(util_bar, mem_bar, th))];
    for g in cards {
        lines.push(Line::from(device_row(g, util_bar, mem_bar, th)));
    }
    lines
}

// ---- Utilization panel ----
fn chart_block(caption: &str, caption_color: Color, series: &[f64], width: usize, color: Color, th: &Theme) -> Vec<Line<'static>> {
    let cur = series.last().copied().unwrap_or(0.0);
    let (top, bottom) = braille_chart(series, width);
    vec![
        Line::from(vec![
            spb(format!("{caption}: "), th.text),
            sp(format!("{cur:.1}%"), caption_color),
        ]),
        Line::from(sp(top, color)),
        Line::from(sp(bottom, color)),
    ]
}

fn chart_lines(app: &App, th: &Theme, inner_w: usize) -> Vec<Line<'static>> {
    let gap = 4usize;
    let half = 12usize.max((inner_w.saturating_sub(4 + gap)) / 2);

    let cpu: Vec<f64> = app.cpu_hist.iter().copied().collect();
    let mem: Vec<f64> = app.mem_hist.iter().copied().collect();
    let gpu_util = avg_series(&app.gpu_util_hist);
    let gpu_mem = avg_series(&app.gpu_vram_hist);

    let mut left: Vec<Line<'static>> = Vec::new();
    left.extend(chart_block("CPU", th.cyan, &cpu, half, th.cyan, th));
    left.extend(chart_block("MEM", th.green, &mem, half, th.green, th));
    left.push(Line::from(sp(axis_line(half, app.interval), th.dim)));

    let mut right: Vec<Line<'static>> = Vec::new();
    right.extend(chart_block("AVG GPU UTL", th.yellow, &gpu_util, half, th.yellow, th));
    right.extend(chart_block("AVG GPU MEM", th.magenta, &gpu_mem, half, th.magenta, th));
    right.push(Line::from(sp(axis_line(half, app.interval), th.dim)));

    let mut merged: Vec<Line<'static>> = Vec::new();
    for i in 0..left.len().max(right.len()) {
        let l = left.get(i).cloned().unwrap_or_else(|| Line::from(""));
        let r = right.get(i).cloned().unwrap_or_else(|| Line::from(""));
        let l_text = l
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        let pad = half.saturating_sub(l_text.chars().count());
        let mut spans = l.spans;
        spans.push(sp(format!("{}{}", " ".repeat(pad), " ".repeat(gap)), th.text));
        spans.extend(r.spans);
        merged.push(Line::from(spans));
    }
    merged
}

// ---- Processes panel ----
fn process_lines(app: &App, th: &Theme, inner_w: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        spb(format!("{:>7}  ", "PID"), th.text),
        spb(format!("{:<8} ", "USER"), th.text),
        spb(format!("{:>3} ", "HCU"), th.text),
        spb(format!("{:>7} ", "VRAM"), th.text),
        spb(format!("{:>5} ", "CU%"), th.text),
        spb(format!("{:>6} ", "CPU%"), th.text),
        spb(format!("{:>5}  ", "MEM%"), th.text),
        spb("COMMAND", th.text),
    ])];

    for (pos, p) in app.proc_rows.iter().enumerate() {
        let marker = if pos == app.selected { ">" } else { " " };
        let cmd_width = inner_w.saturating_sub(50).max(8);
        let mut row = vec![
            sp(format!("{marker}{:>7}  ", p.pid), th.text),
            sp(format!("{:<8} ", truncate(&p.user, 8)), th.text),
            sp(format!("{:>3} ", p.device.map(|d| d.to_string()).unwrap_or_else(|| "-".into())), th.text),
            sp(format!("{:>7} ", fmt_mem(p.vram_mb)), th.text),
            sp(format!("{:>5} ", p.cu_pct.map(|c| format!("{c:.1}")).unwrap_or_else(|| NA.into())), th.text),
            sp(format!("{:>6} ", format!("{:.1}", p.cpu_pct)), th.text),
            sp(format!("{:>5}  ", format!("{:.1}", p.mem_pct)), th.text),
            sp(
                if p.pod.is_empty() {
                    truncate(&p.cmd, cmd_width)
                } else {
                    truncate(&format!("{}  [{}]", p.cmd, p.pod), cmd_width)
                },
                th.text,
            ),
        ];
        if pos == app.selected {
            for s in row.iter_mut() {
                *s = s.clone().patch_style(Style::default().add_modifier(Modifier::BOLD));
            }
        }
        lines.push(Line::from(row));
    }
    lines
}

fn sort_name(sk: SortKey) -> &'static str {
    match sk {
        SortKey::Pid => "PID",
        SortKey::Vram => "VRAM",
        SortKey::Cu => "CU",
        SortKey::Cpu => "CPU",
    }
}

fn render_frame(app: &App, th: &Theme, width: usize) -> Vec<Line<'static>> {
    let inner_w = width.saturating_sub(4); // boxes eat "│ " and " │"
    let has_gpu = !app.gpu.gpus.is_empty();

    // title
    let stamp = chrono::Local::now().format("%b %d %H:%M:%S").to_string();
    let mut title = format!(
        "dtop {}  host: {}  devices: {}  {}",
        env!("CARGO_PKG_VERSION"),
        app.hostname,
        app.gpu.gpus.len(),
        stamp
    );
    title.push_str(&format!("  host cpu {:.1}%  mem {:.1}%", app.cpu.total, mem_pct(app)));
    title.push_str(&format!("  sort: {}", sort_name(app.sort_key)));
    if let Some(f) = &app.filter {
        let mut v: Vec<_> = f.iter().copied().collect();
        v.sort_unstable();
        let list = v.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        title.push_str(&format!("  filter: {list}"));
    }
    if let Some(status) = app.status_text() {
        title.push_str(&format!("  [{status}]"));
    }
    if app.gpu.error.is_some() {
        title.push_str("  errors: 1");
    }

    let mut frame: Vec<Line<'static>> = Vec::new();
    frame.push(box_top("", width, th));
    frame.push(box_row(Line::from(spb(title, th.text)), width, th));
    frame.push(box_bottom(width, th));

    // devices
    frame.push(box_top("Devices", width, th));
    let dev_rows = if has_gpu {
        device_lines(app, th, inner_w)
    } else {
        vec![Line::from(sp("  no DCU detected".to_string(), th.red))]
    };
    for (pos, row) in dev_rows.iter().enumerate() {
        frame.push(box_row(row.clone(), width, th));
        if pos < dev_rows.len() - 1 {
            frame.push(box_sep(width, th));
        }
    }
    frame.push(box_bottom(width, th));

    // utilization
    if has_gpu {
        frame.push(box_top("Utilization", width, th));
        for row in chart_lines(app, th, inner_w) {
            frame.push(box_row(row, width, th));
        }
        frame.push(box_bottom(width, th));
    }

    // processes
    frame.push(box_top("Processes", width, th));
    let proc_rows = process_lines(app, th, inner_w);
    for row in proc_rows {
        frame.push(box_row(row, width, th));
    }
    frame.push(box_bottom(width, th));

    // warnings / errors
    for w in &app.warnings {
        let color = match w.level {
            WarnLevel::Crit => th.red,
            WarnLevel::Warn => th.yellow,
        };
        frame.push(Line::from(vec![
            sp("⚠ ".to_string(), th.red),
            sp(w.text.clone(), color),
        ]));
    }
    if let Some(err) = &app.gpu.error {
        frame.push(Line::from(sp(format!("! {err}"), th.red)));
    }

    // help
    frame.push(Line::from(help_line(app, th)));
    frame
}

fn mem_pct(app: &App) -> f64 {
    let total = app.mem.total_kb.max(1) as f64;
    (total - app.mem.avail_kb as f64) / total * 100.0
}

fn help_line(app: &App, th: &Theme) -> Vec<Span<'static>> {
    let mut v = Vec::new();
    for (k, d) in [
        ("q", " quit"),
        (" r", " refresh"),
        ("  ↑/↓", " select"),
        ("  p", " sort PID"),
        (" m", " VRAM"),
        (" c", " CU"),
        (" u", " CPU"),
        ("  1-9", " filter"),
        (" a", " all"),
        ("  t", " theme"),
        (" k", " kill"),
    ] {
        v.push(spb(k.to_string(), th.title));
        v.push(sp(d.to_string(), th.dim));
        v.push(sp(" |".to_string(), th.dim));
    }
    v.pop();
    if app.recorder.is_some() {
        v.push(sp("  rec:●".to_string(), th.dim));
    }
    v
}

pub fn draw(f: &mut Frame, app: &App) {
    let th = &theme::themes()[app.theme_idx];
    let area = f.area();
    let width = area.width as usize;

    let mut lines = render_frame(app, th, width);
    lines.truncate(area.height as usize);
    f.render_widget(Paragraph::new(Text::from(lines)), area);

    if app.show_help {
        draw_help_popup(f, area, th);
    }
    if let Some(target) = &app.kill_target {
        draw_kill_confirm(f, area, th, target.pid, &target.cmd);
    }
}

fn centered_rect(width: u16, height: u16, r: Rect) -> Rect {
    let x = r.x + r.width.saturating_sub(width) / 2;
    let y = r.y + r.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(r.width), height.min(r.height))
}

fn draw_help_popup(f: &mut Frame, area: Rect, th: &Theme) {
    let rect = centered_rect(52, 15, area);
    f.render_widget(Clear, rect);
    let keys = [
        ("q / Esc", "quit"),
        ("h / F1", "toggle this help"),
        ("r", "refresh now"),
        ("↑ ↓ / PgUp PgDn", "select process"),
        ("p / m / c / u", "sort by PID / VRAM / CU% / CPU%"),
        ("1-9", "filter device (toggle)"),
        ("a", "show all devices"),
        ("t", "cycle theme"),
        ("k", "SIGTERM selected process"),
        ("+ / -", "adjust refresh interval"),
    ];
    let mut lines = vec![Line::from(spb(
        "dtop key bindings",
        th.title,
    ))];
    lines.push(Line::from(""));
    for (k, d) in keys {
        lines.push(Line::from(vec![
            sp(format!("{k:<20}"), th.green),
            sp(d.to_string(), th.text),
        ]));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.title))
        .title(Span::styled(" HELP ", Style::default().fg(th.title)));
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: true }), rect);
}

fn draw_kill_confirm(f: &mut Frame, area: Rect, th: &Theme, pid: u32, cmd: &str) {
    let rect = centered_rect(52, 6, area);
    f.render_widget(Clear, rect);
    let text = Text::from(vec![
        Line::from(""),
        Line::from(vec![
            sp("SIGTERM pid ", th.text),
            sp(pid.to_string(), th.red),
            sp("?", th.text),
        ]),
        Line::from(sp(truncate(cmd, 44), th.dim)),
        Line::from(vec![
            sp(" [y]", th.green),
            sp(" confirm   ", th.dim),
            sp("[n/Esc]", th.red),
            sp(" cancel", th.dim),
        ]),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.red))
        .title(Span::styled(" CONFIRM ", Style::default().fg(th.red)));
    f.render_widget(Paragraph::new(text).block(block).alignment(Alignment::Center), rect);
}
