use crate::app::{App, View};
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

pub fn draw(f: &mut Frame, app: &App) {
    let th = &themes()[app.theme_idx];
    let area = f.area();

    let proc_h = (app.max_procs as u16 + 3).clamp(5, area.height.saturating_sub(24).max(5));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(14),
            Constraint::Min(8),
            Constraint::Length(proc_h),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, chunks[0], app, th);
    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(3, 5), Constraint::Ratio(2, 5)])
        .split(chunks[1]);
    draw_cpu(f, top[0], app, th);
    draw_mem(f, top[1], app, th);
    draw_gpu(f, chunks[2], app, th);
    draw_procs(f, chunks[3], app, th);
    draw_hints(f, chunks[4], app, th);

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
    let snap = app.gpu_snapshot();
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
    let title = Span::styled(
        format!(" CPU  {} ", truncate(&app.cpu_col.model, 28)),
        Style::default(),
    );
    let block = block_title(title, th);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(17), Constraint::Min(20)])
        .split(inner);

    // left: total + sparkline + loadavg
    let hist: Vec<u64> = app.cpu_hist.iter().map(|v| *v as u64).collect();
    let total_color = util_color(app.cpu.total, th);
    let left = Paragraph::new(Text::from(vec![
        Line::from(Span::styled("TOTAL", Style::default().fg(th.dim))),
        Line::from(Span::styled(
            format!("{:5.1}%", app.cpu.total),
            Style::default()
                .fg(total_color)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("load {:.2} {:.2}", app.cpu_col.load[0], app.cpu_col.load[1]),
            Style::default().fg(th.dim),
        )),
        Line::from(Span::styled(
            format!("15m  {:.2}", app.cpu_col.load[2]),
            Style::default().fg(th.dim),
        )),
        Line::from(Span::styled(
            format!("{}T", app.cpu_col.nproc),
            Style::default().fg(th.dim),
        )),
    ]));
    let left_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(3)])
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

    // right: per-core bars, adaptive grouping for high core counts
    let cores = &app.cpu.per_core;
    let n = cores.len();
    let avail_h = cols[1].height.max(1) as usize;
    let ncols = if n <= 32 { 2 } else if n <= 72 { 3 } else { 4 };
    let cells = ncols * avail_h;
    // group adjacent cores when they don't fit individually
    let group = if n <= cells { 1 } else { (n + cells - 1) / cells };
    let shown = (n + group - 1) / group;
    let per_col = (shown + ncols - 1) / ncols;

    let mut col_constraints = Vec::new();
    for _ in 0..ncols {
        col_constraints.push(Constraint::Ratio(1, ncols as u32));
    }
    let core_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(col_constraints)
        .split(cols[1]);

    let label_w = if group == 1 { 4 } else { 8 };
    let bar_w = (core_cols[0].width as usize)
        .saturating_sub(label_w + 5)
        .clamp(2, 30);

    for ci in 0..ncols {
        let mut lines = Vec::new();
        for r in 0..per_col {
            let cell_idx = ci * per_col + r;
            let start = cell_idx * group;
            if start >= n {
                break;
            }
            let end = (start + group).min(n);
            let slice = &cores[start..end];
            let avg = slice.iter().sum::<f64>() / slice.len() as f64;
            let label = if group == 1 {
                format!("C{start:0>2} ")
            } else {
                format!("C{}-{} ", start, end - 1)
            };
            let mut spans = vec![Span::styled(label, Style::default().fg(th.dim))];
            spans.extend(bar_spans(avg, bar_w, util_color(avg, th), th));
            spans.push(Span::styled(
                format!(" {:3.0}%", avg),
                Style::default().fg(th.text),
            ));
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines), core_cols[ci]);
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

    let snap = app.gpu_snapshot();
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
        Line::from(""),
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
        Line::from(""),
        if snap.gpus.is_empty() {
            Line::from(Span::styled(
                "HBM   N/A (no DCU)",
                Style::default().fg(th.dim),
            ))
        } else {
            Line::from(vec![
                Span::styled(
                    format!("HBM   {:.1}G", hbm_used),
                    Style::default().fg(th.dim),
                ),
                Span::styled(
                    format!(" / {:.0}G ({} cards)", hbm_total, snap.gpus.len()),
                    Style::default().fg(th.dim),
                ),
            ])
        },
        seg_bar(
            &[(
                if hbm_total > 0.0 {
                    hbm_used / hbm_total * 100.0
                } else {
                    0.0
                },
                th.cyan,
            )],
            w,
            th,
        ),
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
    let snap = app.gpu_snapshot();
    let mut title_spans = vec![
        Span::styled(" DCU ", Style::default().fg(th.title).add_modifier(Modifier::BOLD)),
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
        .border_style(Style::default().fg(th.border))
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
        View::Summary => draw_gpu_summary(f, inner, app, th, &snap),
        View::Detail => draw_gpu_detail(f, inner, app, th, &snap),
    }
}

fn draw_gpu_detail(
    f: &mut Frame,
    area: Rect,
    app: &App,
    th: &Theme,
    snap: &GpuSnapshot,
) {
    let n = snap.gpus.len();
    if n == 0 {
        return;
    }
    // adaptive grid: show ALL cards. pick column count from card count + width.
    let cols = if n <= 2 {
        n
    } else if n <= 4 {
        2
    } else if area.width >= 150 {
        4
    } else if area.width >= 90 {
        3
    } else {
        2
    };
    let rows = (n + cols - 1) / cols;
    let mut constraints = Vec::new();
    for _ in 0..rows {
        constraints.push(Constraint::Ratio(1, rows as u32));
    }
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (ri, row_area) in row_areas.iter().enumerate() {
        let start = ri * cols;
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(th.border))
        .title(Span::styled(
            format!(" DCU{} {} ", g.index, truncate(&g.name, 18)),
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

    // compact mode: not enough height for charts, show stats only
    if inner.height < 6 {
        let mut s = stat_span("UTIL", util_s, util_c, th);
        s.extend(stat_span(
            "TEMP",
            g.temp_c.map(|t| format!("{t:.0}C")).unwrap_or_else(|| "N/A".into()),
            temp_color(temp, th),
            th,
        ));
        s.extend(stat_span(
            "PWR",
            g.power_w.map(|p| format!("{p:.0}W")).unwrap_or_else(|| "N/A".into()),
            th.orange,
            th,
        ));
        if inner.height >= 3 {
            s.extend(stat_span("HBM", vram_s, th.cyan, th));
            s.extend(stat_span(
                "SCLK",
                g.sclk_mhz.map(|c| format!("{c}M")).unwrap_or_else(|| "N/A".into()),
                th.text,
                th,
            ));
        }
        f.render_widget(Paragraph::new(Line::from(s)), inner);
        return;
    }

    let stat_lines = vec![
        Line::from({
            let mut s = stat_span("UTIL", util_s, util_c, th);
            s.extend(stat_span(
                "TEMP",
                g.temp_c.map(|t| format!("{t:.0}C")).unwrap_or_else(|| "N/A".into()),
                temp_color(temp, th),
                th,
            ));
            s.extend(stat_span(
                "PWR",
                g.power_w.map(|p| format!("{p:.0}W")).unwrap_or_else(|| "N/A".into()),
                th.orange,
                th,
            ));
            s
        }),
        Line::from({
            let mut s = stat_span("HBM", vram_s, th.cyan, th);
            s.extend(stat_span(
                "SCLK",
                g.sclk_mhz.map(|c| format!("{c}M")).unwrap_or_else(|| "N/A".into()),
                th.text,
                th,
            ));
            s.extend(stat_span(
                "MCLK",
                g.mclk_mhz.map(|c| format!("{c}M")).unwrap_or_else(|| "N/A".into()),
                th.dim,
                th,
            ));
            s
        }),
    ];

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(3)])
        .split(inner);
    f.render_widget(Paragraph::new(stat_lines), split[0]);

    let charts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .split(split[1]);

    let util_data: Vec<f64> = app
        .gpu_util_hist
        .get(idx)
        .map(|h| h.iter().copied().collect())
        .unwrap_or_default();
    let vram_data: Vec<f64> = app
        .gpu_vram_hist
        .get(idx)
        .map(|h| h.iter().copied().collect())
        .unwrap_or_default();

    render_curve(f, charts[0], "UTIL", &util_data, th.cyan, th);
    render_curve(f, charts[1], "HBM%", &vram_data, th.orange, th);
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
    _app: &App,
    th: &Theme,
    snap: &GpuSnapshot,
) {
    let bar_w = 14usize;
    let rows: Vec<Row> = snap
        .gpus
        .iter()
        .map(|g| {
            let util = g.util.unwrap_or(0.0);
            let temp = g.temp_c.unwrap_or(0.0);
            let bar = Line::from(bar_spans(util, bar_w, util_color(util, th), th));
            let vram = match (g.vram_used_mb, g.vram_total_mb) {
                (Some(u), Some(t)) => format!("{:.1}/{:.0}G", u / 1024.0, t / 1024.0),
                _ => "N/A".into(),
            };
            Row::new(vec![
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
            ])
        })
        .collect();

    let header = Row::new(
        ["CARD", "MODEL", "GPU-UTIL", "UTIL", "HBM", "TEMP", "POWER", "SCLK"]
            .iter()
            .map(|h| Span::styled(*h, Style::default().fg(th.dim)))
            .collect::<Vec<_>>(),
    );
    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(18),
            Constraint::Length(bar_w as u16 + 1),
            Constraint::Length(5),
            Constraint::Length(10),
            Constraint::Length(5),
            Constraint::Length(6),
            Constraint::Length(6),
        ],
    )
    .header(header);
    f.render_widget(table, area);
}

fn cell(s: impl Into<String>, color: Color) -> TCell<'static> {
    TCell::from(Span::styled(s.into(), Style::default().fg(color)))
}

fn draw_procs(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let snap = app.gpu_snapshot();
    let block = block_title(
        Span::styled(
            format!(
                " PROCESSES  {} total · {} on DCU · sort:{} ",
                app.procs.len(),
                snap.procs.len(),
                if app.sort_by_mem { "MEM" } else { "CPU" }
            ),
            Style::default(),
        ),
        th,
    );
    let inner = block.inner(area);
    f.render_widget(block, area);

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
            Row::new(vec![
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
                Span::styled(p.cmd.clone(), Style::default().fg(th.text)),
            ])
        })
        .collect();

    let header = Row::new(
        vec!["PID", "USER", "CPU%", "MEM%", "DCU", "HBM", "COMMAND"]
            .into_iter()
            .map(|h| Span::styled(h, Style::default().fg(th.dim)))
            .collect::<Vec<_>>(),
    )
    .style(Style::default());

    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .highlight_symbol("▸ ")
    .row_highlight_style(Style::default().bg(th.sel).add_modifier(Modifier::BOLD));

    // manual selection highlight via state
    let mut state = ratatui::widgets::TableState::default().with_selected(Some(app.selected));
    f.render_stateful_widget(table, inner, &mut state);
}

fn draw_hints(f: &mut Frame, area: Rect, app: &App, th: &Theme) {
    let status = app.status_text().map(|s| format!(" [{s}]")).unwrap_or_default();
    let record = if app.recorder.is_some() { " rec:●" } else { "" };
    let line = Line::from(vec![
        Span::styled(
            " q",
            Style::default().fg(th.title).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" quit  ", Style::default().fg(th.dim)),
        Span::styled("h", Style::default().fg(th.title).add_modifier(Modifier::BOLD)),
        Span::styled(" help  ", Style::default().fg(th.dim)),
        Span::styled("Tab", Style::default().fg(th.title).add_modifier(Modifier::BOLD)),
        Span::styled(" view  ", Style::default().fg(th.dim)),
        Span::styled("t", Style::default().fg(th.title).add_modifier(Modifier::BOLD)),
        Span::styled(" theme  ", Style::default().fg(th.dim)),
        Span::styled("↑↓", Style::default().fg(th.title).add_modifier(Modifier::BOLD)),
        Span::styled(" select  ", Style::default().fg(th.dim)),
        Span::styled("k", Style::default().fg(th.title).add_modifier(Modifier::BOLD)),
        Span::styled(" kill  ", Style::default().fg(th.dim)),
        Span::styled("s", Style::default().fg(th.title).add_modifier(Modifier::BOLD)),
        Span::styled(" sort  ", Style::default().fg(th.dim)),
        Span::styled("+/-", Style::default().fg(th.title).add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(" interval{record}{status}"),
            Style::default().fg(th.dim),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
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
        ("↑ ↓ / PgUp PgDn", "select process"),
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
