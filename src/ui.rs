use crate::app::{App, ProcRow, WarnLevel};
use crate::collectors::gpu::GpuSample;
use crate::theme::{self, temp_color, Theme};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table, TableState, Wrap};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App) {
    let th = &theme::themes()[app.theme_idx];
    let area = f.area();

    let cards: Vec<&GpuSample> = match &app.filter {
        Some(f) => app.gpu.gpus.iter().filter(|g| f.contains(&g.index)).collect(),
        None => app.gpu.gpus.iter().collect(),
    };
    let n_cards = cards.len();
    let has_gpu = !app.gpu.gpus.is_empty();

    let devices_h: u16 = if has_gpu { (3 + n_cards.max(1)) as u16 } else { 5 };
    let util_h: u16 = if has_gpu { 7 } else { 0 };
    let warn_h: u16 = if app.warnings.is_empty() { 0 } else { 1 };
    let err_h: u16 = if app.gpu.error.is_some() { 1 } else { 0 };
    let fixed = 1 + devices_h + util_h + warn_h + err_h + 1; // header + devices + util + warn + err + help
    let proc_h = area.height.saturating_sub(fixed).max(5);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(devices_h),
            Constraint::Length(util_h),
            Constraint::Length(proc_h),
            Constraint::Length(warn_h),
            Constraint::Length(err_h),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, chunks[0], app, th);
    if has_gpu {
        draw_devices(f, chunks[1], app, th, &cards);
        draw_utilization(f, chunks[2], app, th);
    } else {
        draw_no_gpu(f, chunks[1], app, th);
    }
    draw_processes(f, chunks[3], app, th);
    if warn_h > 0 {
        draw_warnings(f, chunks[4], app, th);
    }
    if err_h > 0 {
        draw_error(f, chunks[5], app, th);
    }
    draw_help(f, chunks[6], app, th);

    if app.show_help {
        draw_help_popup(f, area, th);
    }
    if let Some(target) = &app.kill_target {
        draw_kill_confirm(f, area, th, target.pid, &target.cmd);
    }
}

fn block<'a>(title: &'a str, th: &Theme) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.border))
        .title(Span::styled(title, Style::default().fg(th.title).add_modifier(Modifier::BOLD)))
}

fn draw_header(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let now = chrono::Local::now().format("%H:%M:%S").to_string();
    let mut spans = vec![
        Span::styled("dtop ", Style::default().fg(th.title).add_modifier(Modifier::BOLD)),
        Span::styled(env!("CARGO_PKG_VERSION"), Style::default().fg(th.dim)),
        Span::styled("  host: ", Style::default().fg(th.dim)),
        Span::styled(app.hostname.clone(), Style::default().fg(th.text)),
        Span::styled("  devices: ", Style::default().fg(th.dim)),
        Span::styled(format!("{}", app.gpu.gpus.len()), Style::default().fg(th.text)),
        Span::styled("  up ", Style::default().fg(th.dim)),
        Span::styled(app.uptime.clone(), Style::default().fg(th.dim)),
        Span::styled("  ", Style::default().fg(th.dim)),
        Span::styled(now, Style::default().fg(th.green)),
        Span::styled("  host cpu ", Style::default().fg(th.dim)),
        Span::styled(format!("{:.1}%", app.cpu.total), Style::default().fg(th.cyan)),
        Span::styled("  mem ", Style::default().fg(th.dim)),
        Span::styled(format!("{:.1}%", mem_pct(app)), Style::default().fg(th.cyan)),
    ];
    if let Some(status) = app.status_text() {
        spans.push(Span::styled(
            format!("  [{status}]"),
            Style::default().fg(th.yellow),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn mem_pct(app: &App) -> f64 {
    let total = app.mem.total_kb.max(1) as f64;
    (total - app.mem.avail_kb as f64) / total * 100.0
}

fn fmt_pct(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}%")).unwrap_or_else(|| "N/A".into())
}

fn fmt_g(mb: Option<f64>) -> String {
    mb.map(|m| format!("{:.1}G", m / 1024.0)).unwrap_or_else(|| "N/A".into())
}

fn fmt_clk(v: Option<u64>) -> String {
    v.map(|c| format!("{c}M")).unwrap_or_else(|| "N/A".into())
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

fn bar_cell(pct: Option<f64>, width: usize, suffix: String, th: &Theme) -> Line<'static> {
    let color = pct.map(|p| theme::util_color(p, th)).unwrap_or(th.dim);
    let filled = pct
        .map(|p| ((p / 100.0) * width as f64).round() as usize)
        .unwrap_or(0)
        .min(width);
    Line::from(vec![
        Span::styled("▏", Style::default().fg(th.dim)),
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled("░".repeat(width - filled), Style::default().fg(th.dim)),
        Span::styled(format!(" {suffix}"), Style::default().fg(color)),
    ])
}

fn draw_devices(f: &mut Frame, area: Rect, _app: &App, th: &Theme, cards: &[&GpuSample]) {
    let block = block("Devices", th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let util_bar = 12usize;
    let mem_bar = 8usize;

    let header = Row::new(vec![
        Cell::from(Span::styled("HCU", Style::default().fg(th.dim))),
        Cell::from(Span::styled("Model", Style::default().fg(th.dim))),
        Cell::from(Span::styled("Temp", Style::default().fg(th.dim))),
        Cell::from(Span::styled("Power", Style::default().fg(th.dim))),
        Cell::from(Span::styled("HCU%", Style::default().fg(th.dim))),
        Cell::from(Span::styled("CU%", Style::default().fg(th.dim))),
        Cell::from(Span::styled("VRAM", Style::default().fg(th.dim))),
        Cell::from(Span::styled("SCLK", Style::default().fg(th.dim))),
        Cell::from(Span::styled("MCLK", Style::default().fg(th.dim))),
    ]);

    let rows: Vec<Row> = cards
        .iter()
        .map(|g| {
            let mem_pct = match (g.vram_used_mb, g.vram_total_mb) {
                (Some(u), Some(t)) if t > 0.0 => Some(u / t * 100.0),
                _ => None,
            };
            Row::new(vec![
                Cell::from(Span::styled(
                    format!("{:>3}", g.index),
                    Style::default().fg(th.text),
                )),
                Cell::from(Span::styled(short_model(&g.name), Style::default().fg(th.title))),
                Cell::from(Span::styled(
                    g.temp_c.map(|t| format!("{t:.1}C")).unwrap_or_else(|| "N/A".into()),
                    Style::default().fg(g.temp_c.map(|t| temp_color(t, th)).unwrap_or(th.dim)),
                )),
                Cell::from(Span::styled(
                    g.power_w.map(|p| format!("{p:.0}W")).unwrap_or_else(|| "N/A".into()),
                    Style::default().fg(power_color(g, th)),
                )),
                Cell::from(bar_cell(g.util, util_bar, fmt_pct(g.util), th)),
                Cell::from(Span::styled(fmt_pct(g.cu_util), Style::default().fg(th.text))),
                Cell::from(bar_cell(
                    mem_pct,
                    mem_bar,
                    format!("{}", fmt_g(g.vram_used_mb)),
                    th,
                )),
                Cell::from(Span::styled(fmt_clk(g.sclk_mhz), Style::default().fg(th.text))),
                Cell::from(Span::styled(fmt_clk(g.mclk_mhz), Style::default().fg(th.dim))),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Length(12),
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Length(util_bar as u16 + 8),
        Constraint::Length(6),
        Constraint::Length(mem_bar as u16 + 9),
        Constraint::Length(6),
        Constraint::Length(6),
    ];
    let table = Table::new(rows, widths).header(header);
    f.render_widget(table, inner);
}

fn power_color(g: &GpuSample, th: &Theme) -> Color {
    match (g.power_w, g.power_cap_w) {
        (Some(p), Some(c)) if c > 0.0 => {
            let f = p / c;
            if f >= 0.9 {
                th.red
            } else if f >= 0.8 {
                th.yellow
            } else {
                th.text
            }
        }
        _ => th.text,
    }
}

fn avg_series(hists: &[std::collections::VecDeque<f64>]) -> Vec<f64> {
    let n = hists.first().map(|h| h.len()).unwrap_or(0);
    if n == 0 {
        return Vec::new();
    }
    (0..n)
        .map(|i| hists.iter().filter_map(|h| h.get(i)).sum::<f64>() / hists.len() as f64)
        .collect()
}

fn to_u64(data: &[f64]) -> Vec<u64> {
    data.iter().map(|v| *v as u64).collect()
}

fn spark_caption(f: &mut Frame, area: Rect, label: &str, color: Color, th: &Theme, value: String) {
    let line = Line::from(vec![
        Span::styled(format!("{label}: "), Style::default().fg(th.text).add_modifier(Modifier::BOLD)),
        Span::styled(value, Style::default().fg(color)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn spark(f: &mut Frame, area: Rect, data: &[f64], color: Color) {
    let d = to_u64(data);
    f.render_widget(
        Sparkline::default().data(&d).max(100).style(Style::default().fg(color)),
        area,
    );
}

fn draw_utilization(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let block = block("Utilization", th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .split(inner);

    // left: host CPU / MEM
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(cols[0]);
    spark_caption(f, left[0], "CPU", th.cyan, th, format!("{:.1}%", app.cpu.total));
    spark(f, left[1], &app.cpu_hist.iter().copied().collect::<Vec<_>>(), th.cyan);
    spark_caption(f, left[2], "MEM", th.green, th, format!("{:.1}%", mem_pct(app)));
    spark(f, left[3], &app.mem_hist.iter().copied().collect::<Vec<_>>(), th.green);

    // right: avg GPU util / mem
    let gpu_util_avg = avg_series(&app.gpu_util_hist);
    let gpu_mem_avg = avg_series(&app.gpu_vram_hist);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(cols[1]);
    let last = |v: &[f64]| v.last().copied().unwrap_or(0.0);
    spark_caption(f, right[0], "AVG GPU UTL", th.yellow, th, format!("{:.1}%", last(&gpu_util_avg)));
    spark(f, right[1], &gpu_util_avg, th.yellow);
    spark_caption(f, right[2], "AVG GPU MEM", th.magenta, th, format!("{:.1}%", last(&gpu_mem_avg)));
    spark(f, right[3], &gpu_mem_avg, th.magenta);

    // time axis on the remaining row
    let axis_row = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(1)])
        .split(inner)[1];
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("|{:<12}|{:<12}|0s  (right = now)", "60s", "30s"),
            Style::default().fg(th.dim),
        )),
        axis_row,
    );
}

fn draw_no_gpu(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let block = block("Devices", th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let reason = app
        .gpu
        .error
        .clone()
        .unwrap_or_else(|| "no DCU detected".into());
    f.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(Span::styled(reason, Style::default().fg(th.red))),
            Line::from(Span::styled(
                "  check: librocm_smi64.so in /opt/hyhal/lib · /dev/kfd · demo: dtop -b mock",
                Style::default().fg(th.dim),
            )),
        ])),
        inner,
    );
}

fn draw_processes(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let title = format!("Processes  ({})", app.proc_rows.len());
    let block = block(&title, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let header = Row::new(vec![
        Cell::from(Span::styled("PID", Style::default().fg(th.dim))),
        Cell::from(Span::styled("USER", Style::default().fg(th.dim))),
        Cell::from(Span::styled("HCU", Style::default().fg(th.dim))),
        Cell::from(Span::styled("VRAM", Style::default().fg(th.dim))),
        Cell::from(Span::styled("CU%", Style::default().fg(th.dim))),
        Cell::from(Span::styled("CPU%", Style::default().fg(th.dim))),
        Cell::from(Span::styled("MEM%", Style::default().fg(th.dim))),
        Cell::from(Span::styled("COMMAND", Style::default().fg(th.dim))),
    ]);

    let rows: Vec<Row> = app
        .proc_rows
        .iter()
        .map(|p| proc_row(p, th))
        .collect();

    let widths = [
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(3),
        Constraint::Length(7),
        Constraint::Length(5),
        Constraint::Length(6),
        Constraint::Length(5),
        Constraint::Min(12),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .highlight_symbol("> ")
        .row_highlight_style(Style::default().bg(th.sel));
    let mut state = TableState::default().with_selected(Some(app.selected));
    f.render_stateful_widget(table, inner, &mut state);
}

fn proc_row(p: &ProcRow, th: &Theme) -> Row<'static> {
    let dev = p
        .device
        .map(|d| d.to_string())
        .unwrap_or_else(|| "-".into());
    Row::new(vec![
        Cell::from(Span::styled(p.pid.to_string(), Style::default().fg(th.text))),
        Cell::from(Span::styled(truncate(&p.user, 8), Style::default().fg(th.dim))),
        Cell::from(Span::styled(dev, Style::default().fg(th.cyan))),
        Cell::from(Span::styled(
            p.vram_mb.map(|v| format!("{:.1}G", v / 1024.0)).unwrap_or_else(|| "-".into()),
            Style::default().fg(th.magenta),
        )),
        Cell::from(Span::styled(
            p.cu_pct.map(|c| format!("{c:.1}")).unwrap_or_else(|| "N/A".into()),
            Style::default().fg(th.cyan),
        )),
        Cell::from(Span::styled(
            format!("{:.1}", p.cpu_pct),
            Style::default().fg(theme::util_color(p.cpu_pct, th)),
        )),
        Cell::from(Span::styled(format!("{:.1}", p.mem_pct), Style::default().fg(th.text))),
        Cell::from(Span::styled(
            if p.pod.is_empty() {
                p.cmd.clone()
            } else {
                format!("{}  [{}]", p.cmd, p.pod)
            },
            Style::default().fg(th.text),
        )),
    ])
}

fn draw_warnings(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let mut spans = vec![Span::styled(" ⚠ ", Style::default().fg(th.red).add_modifier(Modifier::BOLD))];
    for (i, w) in app.warnings.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(th.dim)));
        }
        let color = match w.level {
            WarnLevel::Crit => th.red,
            WarnLevel::Warn => th.yellow,
        };
        spans.push(Span::styled(w.text.clone(), Style::default().fg(color)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_error(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    if let Some(err) = &app.gpu.error {
        f.render_widget(
            Paragraph::new(Span::styled(format!(" ! {err}"), Style::default().fg(th.red))),
            area,
        );
    }
}

fn draw_help(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let key = Style::default().fg(th.title).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(th.dim);
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
        spans.push(Span::styled(k.to_string(), key));
        spans.push(Span::styled(d.to_string(), dim));
        spans.push(Span::styled(" |".to_string(), dim));
    }
    spans.pop();
    if app.recorder.is_some() {
        spans.push(Span::styled("  rec:●".to_string(), dim));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
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
    let mut lines = vec![Line::from(Span::styled(
        "dtop key bindings",
        Style::default().fg(th.title).add_modifier(Modifier::BOLD),
    ))];
    lines.push(Line::from(""));
    for (k, d) in keys {
        lines.push(Line::from(vec![
            Span::styled(format!("{k:<20}"), Style::default().fg(th.green)),
            Span::styled(d, Style::default().fg(th.text)),
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
            Span::styled("SIGTERM pid ", Style::default().fg(th.text)),
            Span::styled(pid.to_string(), Style::default().fg(th.red).add_modifier(Modifier::BOLD)),
            Span::styled("?", Style::default().fg(th.text)),
        ]),
        Line::from(Span::styled(truncate(cmd, 44), Style::default().fg(th.dim))),
        Line::from(vec![
            Span::styled(" [y]", Style::default().fg(th.green)),
            Span::styled(" confirm   ", Style::default().fg(th.dim)),
            Span::styled("[n/Esc]", Style::default().fg(th.red)),
            Span::styled(" cancel", Style::default().fg(th.dim)),
        ]),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.red))
        .title(Span::styled(" CONFIRM ", Style::default().fg(th.red)));
    f.render_widget(Paragraph::new(text).block(block).alignment(Alignment::Center), rect);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}
