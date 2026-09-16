use crate::app::{App, Focus, View};
use crate::collectors::gpu::GpuSnapshot;
use crate::collectors::mem::fmt_kb;
use crate::theme::{temp_color, themes, util_color, Theme};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Axis, Block, Borders, Cell as TCell, Chart, Clear, Dataset, GraphType, Paragraph,
    RenderDirection, Row, Sparkline, Table, Wrap,
};
use ratatui::Frame;

/// Number of columns for the per-card grid in the detail view.
fn gpu_detail_cols(n: usize, width: u16) -> usize {
    if n <= 2 {
        n.max(1)
    } else if n <= 4 {
        2
    } else if width >= 150 {
        4
    } else if width >= 90 {
        3
    } else {
        2
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let th = &themes()[app.theme_idx];
    let area = f.area();

    let warn_h: u16 = if app.warnings.is_empty() { 0 } else { 1 };
    let n_cards = app.gpu.gpus.len();

    // CPU panel: 8 rows is enough (total/load + core grid); DCU panel: 6 rows in
    // summary, a scrollable grid in detail. Everything else goes to processes.
    let want_cpu: u16 = 8;
    let want_gpu: u16 = match app.view {
        View::Summary => 6,
        View::Detail => {
            let cols = gpu_detail_cols(n_cards.max(1), area.width) as u16;
            let rows = ((n_cards.max(1) as u16) + cols - 1) / cols;
            (rows * 9 + 2).max(6)
        }
    };
    let gpu_cap: u16 = match app.view {
        View::Summary => 6,
        View::Detail => 20,
    };
    let proc_target: u16 = (app.max_procs as u16 + 3).max(6);

    let fixed = 2 + warn_h; // header + hints + warnings
    let avail = area.height.saturating_sub(fixed).max(6);
    let cpu_h = want_cpu.min(avail.saturating_sub(6)).max(6.min(avail));
    let remain = avail.saturating_sub(cpu_h);
    // processes first (they were the ones being squeezed), DCU takes the rest
    // up to its cap, and whatever the cap frees goes back to processes
    let proc_h = proc_target.min(remain.saturating_sub(want_gpu.min(6)));
    let gpu_h = remain.saturating_sub(proc_h).min(gpu_cap);
    let proc_h = remain.saturating_sub(gpu_h);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(cpu_h),
            Constraint::Length(gpu_h),
            Constraint::Length(proc_h),
            Constraint::Length(warn_h),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, chunks[0], app, th);
    // give the CPU panel as much width as possible: memory only needs ~26-46 cols
    let mem_w = (area.width / 4).clamp(26, 46);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(30), Constraint::Length(mem_w)])
        .split(chunks[1]);
    draw_cpu(f, top[0], app, th);
    draw_mem(f, top[1], app, th);
    draw_gpu(f, chunks[2], app, th);
    draw_procs(f, chunks[3], app, th);
    if warn_h > 0 {
        draw_warnings(f, chunks[4], app, th);
    }
    draw_hints(f, chunks[5], app, th);

    if app.show_help {
        draw_help(f, area, th);
    }
    if let Some(target) = &app.kill_target {
        draw_kill_confirm(f, area, th, target.pid, &target.cmd);
    }
}

fn block_title<'a>(title: Span<'a>, th: &Theme) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.border))
        .title(title)
        .title_style(Style::default().fg(th.title).add_modifier(Modifier::BOLD))
}

fn draw_header(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let snap = &app.gpu;
    let now = chrono::Local::now().format("%H:%M:%S").to_string();
    let line = Line::from(vec![
        Span::styled(
            " dtop ",
            Style::default()
                .fg(th.title)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("v{} ", env!("CARGO_PKG_VERSION")), Style::default().fg(th.dim)),
        Span::styled(
            format!("{} ", app.hostname),
            Style::default().fg(th.text),
        ),
        Span::styled(
            format!("up {} ", app.uptime),
            Style::default().fg(th.dim),
        ),
        Span::styled(
            format!("backend:{} ", snap.backend),
            Style::default().fg(th.green),
        ),
        Span::styled(
            format!("{} ", snap.gpus.len()),
            Style::default().fg(th.cyan),
        ),
        Span::styled("DCU ", Style::default().fg(th.dim)),
        Span::styled(
            format!("theme:{} ", th.name),
            Style::default().fg(th.dim),
        ),
        Span::styled(now, Style::default().fg(th.green)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn bar_spans(pct: f64, width: usize, color: Color, th: &Theme) -> Vec<Span<'static>> {
    let w = width.max(1);
    let filled = ((pct / 100.0) * w as f64).round().clamp(0.0, w as f64) as usize;
    vec![
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled(
            "░".repeat(w - filled),
            Style::default().fg(th.dim).add_modifier(Modifier::DIM),
        ),
    ]
}

fn draw_cpu(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let cores = &app.cpu.per_core;
    let n = cores.len();

    // core grid geometry (needed for the title before the block is built)
    let inner_w = area.width.saturating_sub(2);
    let core_area_w = inner_w.saturating_sub(17) as usize;
    let ncols = (core_area_w / 22).clamp(1, 8);
    let rows_vis = area.height.saturating_sub(2).max(1) as usize;
    let cells = ncols * rows_vis;
    let max_offset = n.saturating_sub(cells);
    app.cpu_scroll_max.set(max_offset);
    let offset = app.cpu_scroll.min(max_offset);

    let focused = app.focus == Focus::Cpu;
    let range = if max_offset > 0 {
        format!(
            "· {}-{}/{} ↑↓ ",
            offset + 1,
            (offset + cells).min(n),
            n
        )
    } else {
        String::new()
    };
    let title = Span::styled(
        format!(
            " {}CPU  {}{} ",
            if focused { "◉ " } else { "" },
            truncate(&app.cpu_col.model, 24),
            if range.is_empty() {
                String::new()
            } else {
                format!(" {range}")
            }
        ),
        Style::default(),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused { th.title } else { th.border }))
        .title(title)
        .title_style(Style::default().fg(th.title).add_modifier(Modifier::BOLD));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(17), Constraint::Min(20)])
        .split(inner);

    // left: total + loadavg + sparkline (compact: 3 text rows)
    let hist: Vec<u64> = app.cpu_hist.iter().map(|v| *v as u64).collect();
    let total_color = util_color(app.cpu.total, th);
    let left = Paragraph::new(Text::from(vec![
        Line::from(vec![
            Span::styled("TOTAL ", Style::default().fg(th.dim)),
            Span::styled(
                format!("{:.1}%", app.cpu.total),
                Style::default()
                    .fg(total_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            format!("load {:.2} {:.2}", app.cpu_col.load[0], app.cpu_col.load[1]),
            Style::default().fg(th.dim),
        )),
        Line::from(Span::styled(
            format!("15m  {:.2}  {}T", app.cpu_col.load[2], app.cpu_col.nproc),
            Style::default().fg(th.dim),
        )),
    ]));
    let left_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(cols[0]);
    f.render_widget(left, left_split[0]);
    f.render_widget(
        Sparkline::default()
            .data(&hist)
            .max(100)
            .style(Style::default().fg(th.green))
            .direction(RenderDirection::RightToLeft),
        left_split[1],
    );

    // right: individual cores in a scrollable grid (row-major).
    if n == 0 {
        return;
    }
    let mut col_constraints = Vec::new();
    for _ in 0..ncols {
        col_constraints.push(Constraint::Ratio(1, ncols as u32));
    }
    let core_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(col_constraints)
        .split(cols[1]);

    // fixed label ("C127 ") + percentage (" 100%") + 1 trailing gap
    let col_w_min = (0..ncols)
        .map(|ci| core_cols[ci].width as usize)
        .min()
        .unwrap_or(20);
    let bar_w = col_w_min.saturating_sub(11).clamp(2, 40);

    let mut col_lines: Vec<Vec<Line<'static>>> = vec![Vec::new(); ncols];
    'outer: for r in 0..rows_vis {
        for ci in 0..ncols {
            let idx = offset + r * ncols + ci;
            if idx >= n {
                break 'outer;
            }
            let v = cores[idx];
            let mut spans = vec![Span::styled(
                format!("C{idx:<3} "),
                Style::default().fg(th.dim),
            )];
            spans.extend(bar_spans(v, bar_w, util_color(v, th), th));
            spans.push(Span::styled(
                format!("{:>4.0}% ", v),
                Style::default().fg(th.text),
            ));
            col_lines[ci].push(Line::from(spans));
        }
    }
    for ci in 0..ncols {
        f.render_widget(Paragraph::new(std::mem::take(&mut col_lines[ci])), core_cols[ci]);
    }
}

fn seg_bar(parts: &[(f64, Color)], width: usize, th: &Theme) -> Line<'static> {
    let w = width.max(1) as f64;
    let mut spans = Vec::new();
    let mut used = 0.0f64;
    for (pct, color) in parts {
        let end = (used + pct).min(100.0);
        let len = ((end - used) / 100.0 * w).round() as usize;
        if len > 0 {
            spans.push(Span::styled("█".repeat(len), Style::default().fg(*color)));
        }
        used = end;
    }
    let rest = (w as usize).saturating_sub(spans.iter().map(|s| s.content.chars().count()).sum());
    if rest > 0 {
        spans.push(Span::styled(
            "░".repeat(rest),
            Style::default().fg(th.dim).add_modifier(Modifier::DIM),
        ));
    }
    Line::from(spans)
}

fn draw_mem(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let block = block_title(Span::styled(" MEMORY ", Style::default()), th);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let w = inner.width as usize;
    let m = &app.mem;
    let pct = |kb: u64| kb as f64 / m.total_kb.max(1) as f64 * 100.0;

    let snap = &app.gpu;
    let hbm_total: f64 = snap
        .gpus
        .iter()
        .filter_map(|g| g.vram_total_mb)
        .sum::<f64>()
        / 1024.0;
    let hbm_used: f64 = snap
        .gpus
        .iter()
        .filter_map(|g| g.vram_used_mb)
        .sum::<f64>()
        / 1024.0;

    let mut lines = vec![
        Line::from(vec![
            Span::styled("RAM   ", Style::default().fg(th.dim)),
            Span::styled(
                format!("{} / {}", fmt_kb(m.used_kb), fmt_kb(m.total_kb)),
                Style::default().fg(th.text),
            ),
        ]),
        seg_bar(
            &[(pct(m.used_kb), th.green), (pct(m.cache_kb), th.yellow)],
            w,
            th,
        ),
        Line::from(vec![
            Span::styled(
                format!("used {} ", fmt_kb(m.used_kb)),
                Style::default().fg(th.green),
            ),
            Span::styled(
                format!("cache {} ", fmt_kb(m.cache_kb)),
                Style::default().fg(th.yellow),
            ),
            Span::styled(
                format!("avail {}", fmt_kb(m.avail_kb)),
                Style::default().fg(th.dim),
            ),
        ]),
        Line::from(vec![
            Span::styled("SWAP  ", Style::default().fg(th.dim)),
            Span::styled(
                format!(
                    "{} / {}",
                    fmt_kb(m.swap_used_kb()),
                    fmt_kb(m.swap_total_kb)
                ),
                Style::default().fg(th.text),
            ),
        ]),
        seg_bar(
            &[(
                m.swap_used_kb() as f64 / m.swap_total_kb.max(1) as f64 * 100.0,
                th.orange,
            )],
            w,
            th,
        ),
        // HBM is text only (the DCU panel already shows the per-card detail)
        if snap.gpus.is_empty() {
            Line::from(Span::styled("HBM   N/A (no DCU)", Style::default().fg(th.dim)))
        } else {
            Line::from(vec![
                Span::styled("HBM   ", Style::default().fg(th.dim)),
                Span::styled(
                    format!("{:.1}G", hbm_used),
                    Style::default().fg(th.cyan),
                ),
                Span::styled(
                    format!(" / {:.0}G ({} cards)", hbm_total, snap.gpus.len()),
                    Style::default().fg(th.dim),
                ),
            ])
        },
    ];
    while lines.len() < inner.height as usize {
        lines.push(Line::from(""));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn stat_span(label: &str, value: String, color: Color, th: &Theme) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!("{label} "),
            Style::default().fg(th.dim),
        ),
        Span::styled(
            format!("{value}  "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]
}

fn draw_gpu(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let snap = &app.gpu;
    let focused = app.focus == Focus::Gpu;
    let mut title_spans = vec![
        Span::styled(
            if focused { " ◉ DCU " } else { " DCU " },
            Style::default().fg(th.title).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{} cards · backend:{} · {}s ",
                snap.gpus.len(),
                snap.backend,
                app.gpu_interval
            ),
            Style::default().fg(th.dim),
        ),
    ];
    if let Some(err) = &snap.error {
        let short = if err.len() > 40 { &err[..40] } else { err };
        title_spans.push(Span::styled(
            format!("· {short} "),
            Style::default().fg(th.red),
        ));
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused { th.title } else { th.border }))
        .title(Line::from(title_spans));
    if app.view == View::Detail {
        block = block.title(
            Line::from(Span::styled(
                "Tab: summary view ",
                Style::default().fg(th.dim),
            ))
            .alignment(Alignment::Right),
        );
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    if snap.gpus.is_empty() {
        let reason = snap
            .error
            .clone()
            .unwrap_or_else(|| "no DCU data (backend produced no cards)".into());
        let msg = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::from(Span::styled(
                "NO DCU AVAILABLE",
                Style::default()
                    .fg(th.dim)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(reason, Style::default().fg(th.yellow))),
            Line::from(""),
            Line::from(Span::styled(
                "  hy-smi / rocm-smi not found  ->  install Hygon DTK toolkit",
                Style::default().fg(th.dim),
            )),
            Line::from(Span::styled(
                "  /dev/kfd missing               ->  DCU driver not loaded",
                Style::default().fg(th.dim),
            )),
            Line::from(Span::styled(
                "  no DCU hardware                ->  dtop still works as CPU/MEM monitor",
                Style::default().fg(th.dim),
            )),
            Line::from(Span::styled(
                "  demo mode                      ->  dtop -b mock",
                Style::default().fg(th.dim),
            )),
        ]))
        .alignment(Alignment::Center);
        f.render_widget(msg, inner);
        return;
    }

    match app.view {
        View::Summary => draw_gpu_summary(f, inner, app, th, snap),
        View::Detail => {
            let cols = gpu_detail_cols(snap.gpus.len().max(1), area.width);
            draw_gpu_detail(f, inner, app, th, snap, cols)
        }
    }
}

fn draw_gpu_detail(
    f: &mut Frame,
    area: Rect,
    app: &App,
    th: &Theme,
    snap: &GpuSnapshot,
    cols: usize,
) {
    let n = snap.gpus.len();
    if n == 0 {
        return;
    }
    // cols is computed from the panel (outer) width so it matches the layout
    let rows = (n + cols - 1) / cols;
    // a card wants >= 6 rows (borders + 1 stat line + charts); if the panel
    // cannot fit every row, show a scrollable window of rows
    let vis_rows = ((area.height / 6).max(1) as usize).min(rows);
    let max_off = rows.saturating_sub(vis_rows);
    app.gpu_scroll_max.set(max_off);
    let off = app.gpu_scroll.min(max_off);
    let vis_rows = vis_rows.min(rows - off);

    let mut constraints = Vec::new();
    for _ in 0..vis_rows {
        constraints.push(Constraint::Ratio(1, vis_rows as u32));
    }
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (ri, row_area) in row_areas.iter().enumerate() {
        let start = (off + ri) * cols;
        let count = n.saturating_sub(start).min(cols);
        if count == 0 {
            break;
        }
        let mut cc = Vec::new();
        for _ in 0..count {
            cc.push(Constraint::Ratio(1, count as u32));
        }
        let cards = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(cc)
            .split(*row_area);
        for (ci, card_area) in cards.iter().enumerate() {
            let idx = start + ci;
            if idx >= n {
                break;
            }
            draw_gpu_card(f, *card_area, app, th, snap, idx);
        }
    }
}

fn draw_gpu_card(
    f: &mut Frame,
    area: Rect,
    app: &App,
    th: &Theme,
    snap: &GpuSnapshot,
    idx: usize,
) {
    let g = &snap.gpus[idx];
    let numa_s = app
        .topology
        .numa_node
        .get(g.index)
        .and_then(|n| *n)
        .map(|n| format!(" · N{n}"))
        .unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.border))
        .title(Span::styled(
            format!(" DCU{} {}{} ", g.index, truncate(&g.name, 18), numa_s),
            Style::default()
                .fg(th.title)
                .add_modifier(Modifier::BOLD),
        ))
        .title_style(Style::default());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 1 || inner.width < 16 {
        return;
    }

    let util_s = g
        .util
        .map(|u| format!("{u:.0}%"))
        .unwrap_or_else(|| "N/A".into());
    let util_c = g.util.map(|u| util_color(u, th)).unwrap_or(th.dim);
    let temp = g.temp_c.unwrap_or(0.0);
    let vram_s = match (g.vram_used_mb, g.vram_total_mb) {
        (Some(u), Some(t)) => format!("{:.1}/{:.0}G", u / 1024.0, t / 1024.0),
        _ => "N/A".into(),
    };

    let cat = |groups: &[&Vec<Span<'static>>]| -> Vec<Span<'static>> {
        groups.iter().flat_map(|g| (*g).iter().cloned()).collect()
    };

    let sp_util = stat_span("UTIL", util_s, util_c, th);
    let sp_temp = stat_span(
        "TEMP",
        g.temp_c.map(|t| format!("{t:.0}C")).unwrap_or_else(|| "N/A".into()),
        temp_color(temp, th),
        th,
    );
    let sp_pwr = stat_span(
        "PWR",
        g.power_w.map(|p| format!("{p:.0}W")).unwrap_or_else(|| "N/A".into()),
        th.orange,
        th,
    );
    let sp_hbm = stat_span("HBM", vram_s, th.cyan, th);
    let sp_sclk = stat_span(
        "SCLK",
        g.sclk_mhz.map(|c| format!("{c}M")).unwrap_or_else(|| "N/A".into()),
        th.text,
        th,
    );
    let sp_mclk = stat_span(
        "MCLK",
        g.mclk_mhz.map(|c| format!("{c}M")).unwrap_or_else(|| "N/A".into()),
        th.dim,
        th,
    );

    // extras: video engines / fan / perf / mode
    let mut extras: Vec<Span<'static>> = Vec::new();
    if let Some(d) = g.dec_pct {
        extras.extend(stat_span("DEC", format!("{d:.0}%"), th.cyan, th));
    }
    if let Some(e) = g.enc_pct {
        extras.extend(stat_span("ENC", format!("{e:.0}%"), th.green, th));
    }
    if let Some(f) = g.fan_pct {
        extras.extend(stat_span("FAN", format!("{f:.0}%"), th.dim, th));
    } else if let Some(r) = g.fan_rpm {
        extras.extend(stat_span("FAN", format!("{r}rpm"), th.dim, th));
    }
    if let Some(p) = &g.perf_level {
        if p.to_lowercase() != "auto" {
            extras.extend(stat_span("PERF", p.clone(), th.dim, th));
        }
    }
    if let Some(m) = &g.mode {
        let ml = m.to_lowercase();
        if !ml.is_empty() && ml != "normal" {
            let mc = if ml.contains("power saving") || ml.contains("throttl") || ml.contains("low") {
                th.red
            } else {
                th.yellow
            };
            extras.extend(stat_span("MODE", m.clone(), mc, th));
        }
    }

    // Always keep room for the braille charts (they are the point of this
    // view). Only drop to a text-only card when there is genuinely no space.
    let stat_lines: Vec<Line<'static>> = if inner.height >= 9 && !extras.is_empty() {
        vec![
            Line::from(cat(&[&sp_util, &sp_temp, &sp_pwr])),
            Line::from(cat(&[&sp_hbm, &sp_sclk, &sp_mclk])),
            Line::from(extras.clone()),
        ]
    } else if inner.height >= 7 {
        let line2 = if extras.is_empty() {
            cat(&[&sp_hbm, &sp_sclk, &sp_mclk])
        } else {
            cat(&[&sp_hbm, &sp_sclk, &extras])
        };
        vec![
            Line::from(cat(&[&sp_util, &sp_temp, &sp_pwr])),
            Line::from(line2),
        ]
    } else if inner.height >= 5 {
        vec![Line::from(cat(&[&sp_util, &sp_temp, &sp_pwr, &sp_hbm]))]
    } else {
        let mut s = cat(&[&sp_util, &sp_temp, &sp_pwr]);
        if inner.height >= 2 {
            s.extend(sp_hbm.clone());
        }
        if inner.height >= 3 {
            s.extend(sp_sclk.clone());
        }
        f.render_widget(Paragraph::new(Line::from(s)), inner);
        return;
    };
    let stats_h = stat_lines.len() as u16;

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(stats_h), Constraint::Min(3)])
        .split(inner);
    f.render_widget(Paragraph::new(stat_lines), split[0]);

    // a single full-width VRAM(HBM%) curve is enough; UTIL stays as text
    let vram_data: Vec<f64> = app
        .gpu_vram_hist
        .get(idx)
        .map(|h| h.iter().copied().collect())
        .unwrap_or_default();
    render_curve(f, split[1], "HBM%", &vram_data, th.orange, th);
}

fn render_curve(
    f: &mut Frame,
    area: Rect,
    label: &str,
    data: &[f64],
    color: Color,
    th: &Theme,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.border))
        .title(Span::styled(
            format!(" {label} "),
            Style::default().fg(th.dim),
        ));
    let points: Vec<(f64, f64)> = data
        .iter()
        .enumerate()
        .map(|(i, v)| (i as f64, *v))
        .collect();
    let datasets = vec![Dataset::default()
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(color))
        .data(&points)];
    let xmax = (points.len().saturating_sub(1)).max(1) as f64;
    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(Axis::default().bounds([0.0, xmax]))
        .y_axis(
            Axis::default()
                .bounds([0.0, 100.0])
                .style(Style::default().fg(th.dim))
                .labels(vec!["0", "100"]),
        );
    f.render_widget(chart, area);
}

fn draw_gpu_summary(
    f: &mut Frame,
    area: Rect,
    app: &App,
    th: &Theme,
    snap: &GpuSnapshot,
) {
    let bar_w = 14usize;
    // add DEC/ENC + FAN columns only when the smi backend reports them
    let has_decenc = snap.gpus.iter().any(|g| g.dec_pct.is_some() || g.enc_pct.is_some());
    let has_fan = snap.gpus.iter().any(|g| g.fan_pct.is_some() || g.fan_rpm.is_some());
    let has_numa = app.topology.numa_node.iter().any(|n| n.is_some());

    // scrollable window: header takes 1 row
    let vis = (area.height.saturating_sub(1) as usize).max(1);
    let max_off = snap.gpus.len().saturating_sub(vis);
    app.gpu_scroll_max.set(max_off);
    let off = app.gpu_scroll.min(max_off);

    let rows: Vec<Row> = snap
        .gpus
        .iter()
        .skip(off)
        .take(vis)
        .map(|g| {
            let util = g.util.unwrap_or(0.0);
            let temp = g.temp_c.unwrap_or(0.0);
            let bar = Line::from(bar_spans(util, bar_w, util_color(util, th), th));
            let vram = match (g.vram_used_mb, g.vram_total_mb) {
                (Some(u), Some(t)) => format!("{:.1}/{:.0}G", u / 1024.0, t / 1024.0),
                _ => "N/A".into(),
            };
            let mut cells = vec![
                cell(format!("DCU{}", g.index), th.title),
                cell(truncate(&g.name, 16), th.dim),
                TCell::from(bar),
                cell(format!("{util:3.0}%"), util_color(util, th)),
                cell(vram, th.cyan),
                cell(
                    g.temp_c.map(|t| format!("{t:.0}C")).unwrap_or_else(|| "N/A".into()),
                    temp_color(temp, th),
                ),
                cell(
                    g.power_w.map(|p| format!("{p:.0}W")).unwrap_or_else(|| "N/A".into()),
                    th.orange,
                ),
                cell(
                    g.sclk_mhz.map(|c| format!("{c}M")).unwrap_or_else(|| "N/A".into()),
                    th.text,
                ),
            ];
            if has_decenc {
                cells.push(cell(
                    format!(
                        "{}/{}",
                        g.dec_pct.map(|d| format!("{d:.0}")).unwrap_or_else(|| "-".into()),
                        g.enc_pct.map(|e| format!("{e:.0}")).unwrap_or_else(|| "-".into())
                    ),
                    th.cyan,
                ));
            }
            if has_fan {
                let fan = g
                    .fan_pct
                    .map(|f| format!("{f:.0}%"))
                    .or_else(|| g.fan_rpm.map(|r| format!("{r}rpm")))
                    .unwrap_or_else(|| "-".into());
                cells.push(cell(fan, th.dim));
            }
            if has_numa {
                let numa = app
                    .topology
                    .numa_node
                    .get(g.index)
                    .and_then(|n| *n)
                    .map(|n| format!("N{n}"))
                    .unwrap_or_else(|| "-".into());
                cells.push(cell(numa, th.green));
            }
            Row::new(cells)
        })
        .collect();

    let mut headers = vec!["CARD", "MODEL", "GPU-UTIL", "UTIL", "HBM", "TEMP", "POWER", "SCLK"];
    if has_decenc {
        headers.push("DEC/ENC");
    }
    if has_fan {
        headers.push("FAN");
    }
    if has_numa {
        headers.push("NUMA");
    }
    let header = Row::new(
        headers
            .iter()
            .map(|h| Span::styled(*h, Style::default().fg(th.dim)))
            .collect::<Vec<_>>(),
    );

    let mut widths = vec![
        Constraint::Length(6),
        Constraint::Length(18),
        Constraint::Length(bar_w as u16 + 1),
        Constraint::Length(5),
        Constraint::Length(10),
        Constraint::Length(5),
        Constraint::Length(6),
        Constraint::Length(6),
    ];
    if has_decenc {
        widths.push(Constraint::Length(9));
    }
    if has_fan {
        widths.push(Constraint::Length(7));
    }
    if has_numa {
        widths.push(Constraint::Length(5));
    }

    let table = Table::new(rows, widths).header(header);
    f.render_widget(table, area);
}

fn cell(s: impl Into<String>, color: Color) -> TCell<'static> {
    TCell::from(Span::styled(s.into(), Style::default().fg(color)))
}

fn draw_procs(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let snap = &app.gpu;
    let focused = app.focus == Focus::Procs;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if focused { th.title } else { th.border }))
        .title(Span::styled(
            format!(
                " {}PROCESSES  {} total · {} on DCU · sort:{} ",
                if focused { "◉ " } else { "" },
                app.total_procs,
                snap.procs.len(),
                if app.sort_by_mem { "MEM" } else { "CPU" }
            ),
            Style::default(),
        ))
        .title_style(Style::default().fg(th.title).add_modifier(Modifier::BOLD));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // show the container/pod column only when something is containerized
    let show_pod = app
        .procs
        .iter()
        .take(app.max_procs)
        .any(|p| !p.pod.is_empty() || !p.container.is_empty());

    let rows: Vec<Row> = app
        .procs
        .iter()
        .take(app.max_procs)
        .map(|p| {
            let dcu = if p.gpu_ids.is_empty() {
                "-".to_string()
            } else {
                p.gpu_ids
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let hbm = if p.gpu_vram_mb > 0.0 {
                format!("{:.1}G", p.gpu_vram_mb / 1024.0)
            } else {
                "-".into()
            };
            let pod = if !p.pod.is_empty() {
                p.pod.clone()
            } else if !p.container.is_empty() {
                p.container.clone()
            } else {
                "-".into()
            };
            let mut cells = vec![
                Span::styled(p.pid.to_string(), Style::default().fg(th.text)),
                Span::styled(truncate(&p.user, 9), Style::default().fg(th.dim)),
                Span::styled(
                    format!("{:5.1}", p.cpu_pct),
                    Style::default().fg(util_color(p.cpu_pct, th)),
                ),
                Span::styled(format!("{:5.1}", p.mem_pct), Style::default().fg(th.text)),
                Span::styled(
                    dcu,
                    Style::default()
                        .fg(th.cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(hbm, Style::default().fg(th.orange)),
            ];
            if show_pod {
                cells.push(Span::styled(pod, Style::default().fg(th.green)));
            }
            cells.push(Span::styled(p.cmd.clone(), Style::default().fg(th.text)));
            Row::new(cells)
        })
        .collect();

    let mut headers = vec!["PID", "USER", "CPU%", "MEM%", "DCU", "HBM"];
    if show_pod {
        headers.push("POD");
    }
    headers.push("COMMAND");
    let header = Row::new(
        headers
            .into_iter()
            .map(|h| Span::styled(h, Style::default().fg(th.dim)))
            .collect::<Vec<_>>(),
    )
    .style(Style::default());

    let mut widths = vec![
        Constraint::Length(7),
        Constraint::Length(10),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(7),
    ];
    if show_pod {
        widths.push(Constraint::Length(13));
    }
    widths.push(Constraint::Min(20));

    let table = Table::new(rows, widths)
        .header(header)
        .highlight_symbol("▸ ")
        .row_highlight_style(Style::default().bg(th.sel).add_modifier(Modifier::BOLD));

    // manual selection highlight via state
    let mut state = ratatui::widgets::TableState::default().with_selected(Some(app.selected));
    f.render_stateful_widget(table, inner, &mut state);
}

fn draw_warnings(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let mut spans = vec![Span::styled(
        " ⚠ ",
        Style::default().fg(th.red).add_modifier(Modifier::BOLD),
    )];
    for (i, w) in app.warnings.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(th.dim)));
        }
        let color = match w.level {
            crate::app::WarnLevel::Crit => th.red,
            crate::app::WarnLevel::Warn => th.yellow,
        };
        spans.push(Span::styled(w.text.clone(), Style::default().fg(color)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_hints(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let status = app.status_text().map(|s| format!(" [{s}]")).unwrap_or_default();
    let record = if app.recorder.is_some() { " rec:●" } else { "" };
    let key = |_: &'static str| Style::default().fg(th.title).add_modifier(Modifier::BOLD);
    let dim = || Style::default().fg(th.dim);
    // highlight the active focus target
    let fstyle = |focus: Focus| {
        if app.focus == focus {
            Style::default().fg(th.green).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.title).add_modifier(Modifier::BOLD)
        }
    };
    let line = Line::from(vec![
        Span::styled(" q", key("q")),
        Span::styled(" quit  ", dim()),
        Span::styled("h", key("h")),
        Span::styled(" help  ", dim()),
        Span::styled("Tab", key("Tab")),
        Span::styled(" view  ", dim()),
        Span::styled("t", key("t")),
        Span::styled(" theme  ", dim()),
        Span::styled("c", fstyle(Focus::Cpu)),
        Span::styled("/", dim()),
        Span::styled("g", fstyle(Focus::Gpu)),
        Span::styled("/", dim()),
        Span::styled("p", fstyle(Focus::Procs)),
        Span::styled(
            format!(" focus({}) scroll  ", focus_name(app.focus)),
            dim(),
        ),
        Span::styled("k", key("k")),
        Span::styled(" kill  ", dim()),
        Span::styled("s", key("s")),
        Span::styled(" sort  ", dim()),
        Span::styled("+/-", key("+/-")),
        Span::styled(
            format!(" interval{record}{status}"),
            dim(),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn focus_name(f: Focus) -> &'static str {
    match f {
        Focus::Cpu => "CPU",
        Focus::Gpu => "DCU",
        Focus::Procs => "proc",
    }
}

fn centered_rect(width: u16, height: u16, r: Rect) -> Rect {
    let x = r.x + r.width.saturating_sub(width) / 2;
    let y = r.y + r.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(r.width), height.min(r.height))
}

fn draw_help(f: &mut Frame, area: Rect, th: &Theme) {
    let rect = centered_rect(56, 18, area);
    f.render_widget(Clear, rect);
    let keys = [
        ("q / Esc", "quit dtop"),
        ("h / F1", "toggle this help"),
        ("Tab / v", "switch detail / summary view"),
        ("t", "cycle theme (dark / nord / light)"),
        ("c", "focus CPU panel"),
        ("g", "focus DCU panel"),
        ("p", "focus process list"),
        ("↑ ↓ / PgUp PgDn", "scroll the focused panel"),
        ("k", "send SIGTERM to selected process"),
        ("s", "toggle sort by CPU% / MEM%"),
        ("+ / -", "increase / decrease refresh interval"),
    ];
    let mut lines = vec![Line::from(Span::styled(
        "dtop key bindings",
        Style::default()
            .fg(th.title)
            .add_modifier(Modifier::BOLD),
    ))];
    lines.push(Line::from(""));
    for (k, d) in keys {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{k:<18}"),
                Style::default().fg(th.green),
            ),
            Span::styled(d, Style::default().fg(th.text)),
        ]));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.title))
        .title(Span::styled(" HELP ", Style::default().fg(th.title)));
    f.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        rect,
    );
}

fn draw_kill_confirm(f: &mut Frame, area: Rect, th: &Theme, pid: u32, cmd: &str) {
    let rect = centered_rect(52, 7, area);
    f.render_widget(Clear, rect);
    let text = Text::from(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Send SIGTERM to ", Style::default().fg(th.text)),
            Span::styled(
                format!("PID {pid}"),
                Style::default().fg(th.red).add_modifier(Modifier::BOLD),
            ),
            Span::styled("?", Style::default().fg(th.text)),
        ]),
        Line::from(Span::styled(
            truncate(cmd, 44),
            Style::default().fg(th.dim),
        )),
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
        .title(Span::styled(" CONFIRM KILL ", Style::default().fg(th.red)));
    f.render_widget(Paragraph::new(text).block(block), rect);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}
