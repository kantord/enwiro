use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

/// Initialize the tracing subscriber with dual output:
/// - Rolling daily log file at `~/.local/state/enwiro/<log_name>` (DEBUG level)
/// - stderr filtered by `RUST_LOG` (defaults to WARN when unset)
///
/// Returns a guard that must be held for the lifetime of the program
/// to ensure the non-blocking file writer flushes on drop.
pub fn init_logging(log_name: &str) -> tracing_appender::non_blocking::WorkerGuard {
    let log_dir = std::env::var("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            home::home_dir()
                .expect("Could not determine home directory")
                .join(".local")
                .join("state")
        })
        .join("enwiro");
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "Warning: could not create log directory {:?}: {}",
            log_dir, e
        );
    }

    let file_appender = tracing_appender::rolling::daily(&log_dir, log_name);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
                ),
        )
        .init();

    guard
}
