use clap::Parser;

/// dtop - a top-like system monitor for Hygon DCU + CPU
#[derive(Parser, Debug, Clone)]
#[command(name = "dtop", version, about)]
pub struct Cli {
    /// UI refresh interval in seconds
    #[arg(short = 'i', long, default_value_t = 1.0, value_name = "SECS")]
    pub interval: f64,

    /// GPU sampling interval in seconds
    #[arg(short = 'g', long, default_value_t = 1.0, value_name = "SECS")]
    pub gpu_interval: f64,

    /// color theme: dark | nord | light
    #[arg(short = 't', long, default_value = "dark", value_name = "THEME")]
    pub theme: String,

    /// GPU backend: auto | hy-smi | rocm-smi | mock | none
    /// auto: hy-smi -> rocm-smi -> none (no fake data on machines without DCU)
    #[arg(short = 'b', long, default_value = "auto", value_name = "BACKEND")]
    pub backend: String,

    /// print one snapshot and exit (script friendly, like gpustat)
    #[arg(short = '1', long = "once")]
    pub once: bool,

    /// initial view: detail | summary
    #[arg(short = 'v', long, default_value = "detail", value_name = "VIEW")]
    pub view: String,

    /// process sort key: cpu | mem
    #[arg(short = 's', long, default_value = "cpu", value_name = "KEY")]
    pub sort: String,

    /// max number of processes to display
    #[arg(short = 'n', long, default_value_t = 10, value_name = "N")]
    pub procs: usize,

    /// log file path (default: ~/.local/state/dtop/dtop.log)
    #[arg(short = 'L', long, value_name = "FILE")]
    pub log_file: Option<String>,

    /// log level: error | warn | info | debug | trace
    #[arg(long, default_value = "info", value_name = "LEVEL")]
    pub log_level: String,

    /// disable file logging
    #[arg(long)]
    pub no_log: bool,

    /// record metrics to a JSONL file (append)
    #[arg(short = 'r', long, value_name = "FILE")]
    pub record: Option<String>,

    /// record interval in seconds
    #[arg(long, default_value_t = 10.0, value_name = "SECS")]
    pub record_interval: f64,

    /// number of history points kept for curves
    #[arg(long, default_value_t = 120, value_name = "N")]
    pub history: usize,

    /// temperature warning threshold in Celsius (yellow)
    #[arg(long, default_value_t = 75.0, value_name = "C")]
    pub temp_warn: f64,

    /// temperature critical threshold in Celsius (red)
    #[arg(long, default_value_t = 85.0, value_name = "C")]
    pub temp_crit: f64,

    /// seconds a DCU must stay idle while holding VRAM before warning
    #[arg(long, default_value_t = 10, value_name = "SECS")]
    pub idle_warn_secs: u64,
}
