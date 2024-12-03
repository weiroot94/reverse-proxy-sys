use log::LevelFilter;
use simple_logger::SimpleLogger;

pub fn init_logging(verbosity: &str) {
    let level_filter = match verbosity {
        "trace" => LevelFilter::Trace,
        "debug" => LevelFilter::Debug,
        "info" => LevelFilter::Info,
        "warn" => LevelFilter::Warn,
        "error" => LevelFilter::Error,
        _ => LevelFilter::Info,
    };

    SimpleLogger::new()
        .with_utc_timestamps()
        .with_colors(true)
        .with_level(level_filter)
        .init()
        .unwrap();
}
