mod app;
mod cli;
mod collectors;
mod once;
mod record;
mod theme;
mod ui;

use anyhow::Result;
use app::App;
use clap::Parser;
use cli::Cli;
use crossterm::event::{self, Event, KeyEventKind, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use log::LevelFilter;
use simplelog::{Config as LogConfig, WriteLogger};
use std::io;
use std::time::{Duration, Instant};

fn default_log_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    format!("{home}/.local/state/dtop/dtop.log")
}

fn init_logging(cli: &Cli) -> Result<Option<String>> {
    if cli.no_log {
        return Ok(None);
    }
    let level = match cli.log_level.to_lowercase().as_str() {
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        "off" => LevelFilter::Off,
        _ => LevelFilter::Info,
    };
    let path = cli.log_file.clone().unwrap_or_else(default_log_path);
    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    WriteLogger::init(level, LogConfig::default(), file)?;
    Ok(Some(path))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let log_path = init_logging(&cli)?;
    log::info!(
        "dtop {} starting (pid {}), log: {:?}, record: {:?}",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        log_path,
        cli.record
    );
    log::info!("args: {cli:?}");

    let backend = collectors::gpu::detect(&cli.backend);
    log::info!("gpu backend selected: {}", backend.name());

    if cli.once {
        return once::run(&cli, backend.as_ref());
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, event::EnableMouseCapture)?;
    let mut term = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;

    let result = run_loop(&mut term, &cli);

    disable_raw_mode()?;
    execute!(
        term.backend_mut(),
        LeaveAlternateScreen,
        event::DisableMouseCapture
    )?;
    term.show_cursor()?;
    log::info!("dtop exiting");
    result
}

fn run_loop(term: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>, cli: &Cli) -> Result<()> {
    let mut app = App::new(cli)?;
    let mut last_tick = Instant::now();

    loop {
        let elapsed = last_tick.elapsed().as_secs_f64();
        let timeout = Duration::from_secs_f64((app.interval - elapsed).max(0.02));
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => app.handle_key(k),
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollUp => app.scroll(-1),
                    MouseEventKind::ScrollDown => app.scroll(1),
                    _ => {}
                },
                _ => {}
            }
        }
        if last_tick.elapsed().as_secs_f64() >= app.interval {
            app.tick();
            last_tick = Instant::now();
        }
        term.draw(|f| ui::draw(f, &app))?;
        if !app.running {
            break;
        }
    }
    Ok(())
}
