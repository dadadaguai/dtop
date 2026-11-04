use clap::Parser;

/// dtop - an nvitop-style terminal monitor for Hygon DCU (via librocm_smi64)
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

    /// GPU backend: rsmi | mock  (auto = rsmi)
    #[arg(short = 'b', long, default_value = "auto", value_name = "BACKEND")]
    pub backend: String,

    /// only monitor these devices (comma separated), e.g. 0,1,3
    #[arg(short = 'd', long, value_delimiter = ',', value_name = "0,1,3")]
    pub devices: Option<Vec<usize>>,

    /// HCU%/CU% sampling window in milliseconds (driver blocks for this long)
    #[arg(long, default_value_t = 150, value_name = "MS")]
    pub window_ms: u32,

    /// print one text snapshot and exit
    #[arg(short = '1', long = "once")]
    pub once: bool,

    /// print one JSON snapshot and exit (stable schema, null = unsupported)
    #[arg(long)]
    pub json: bool,

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
