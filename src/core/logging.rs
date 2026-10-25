//! stdout carries the MCP protocol, so every record goes to stderr and to
//! `<logs_dir>/server.log`. MCP clients often hide the child's stderr, so the file is
//! the only place where the reason for a failure survives.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use tracing::Level;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer;

pub fn init(logs_dir: &Path, level: LevelFilter) {
    let file = log_file(logs_dir).map(|file| Arc::new(Mutex::new(file)));
    let make = move || Tee { file: file.clone() };

    // Only this crate's records pass the filter: at the default level rmcp prints every
    // handshake structure in full, which is debug noise rather than user-facing output.
    let filter = tracing_subscriber::filter::Targets::new()
        .with_target(env!("CARGO_CRATE_NAME"), level)
        .with_default(LevelFilter::WARN);

    let layer = tracing_subscriber::fmt::layer()
        .with_writer(make)
        .with_ansi(false)
        .with_target(false)
        .with_filter(filter);

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    if tracing_subscriber::registry()
        .with(layer)
        .try_init()
        .is_err()
    {
        eprintln!("failed to initialize logging (a global subscriber may already be installed)");
    }
}

/// Falls back to stderr when the file cannot be opened: logging must never block startup.
fn log_file(dir: &Path) -> Option<File> {
    fs::create_dir_all(dir).ok()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("server.log"))
        .ok()
}

/// The level comes from the config file, never the environment: a log switch is configuration.
pub fn parse_level(raw: &str) -> LevelFilter {
    match raw {
        "trace" => Level::TRACE.into(),
        "debug" => Level::DEBUG.into(),
        "warn" => Level::WARN.into(),
        "error" => Level::ERROR.into(),
        _ => Level::INFO.into(),
    }
}

/// One record, two sinks; `O_APPEND` keeps concurrent servers (one per agent) from
/// truncating each other's lines.
struct Tee {
    file: Option<Arc<Mutex<File>>>,
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let stderr = io::stderr().write_all(buf);
        if let Some(file) = &self.file {
            let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = file.write_all(buf);
        }
        stderr.map(|()| buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let stderr = io::stderr().flush();
        if let Some(file) = &self.file {
            let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = file.flush();
        }
        stderr
    }
}
