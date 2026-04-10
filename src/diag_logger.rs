use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

/// A logger that writes to stderr (via env_logger) and also sends each line
/// to an in-memory channel for the diagnostics page.
struct DiagLogger {
    inner: env_logger::Logger,
    tx: SyncSender<String>,
}

impl log::Log for DiagLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        self.inner.log(record);
        if self.enabled(record.metadata()) {
            let prefix = match record.level() {
                log::Level::Error => "🔴",
                log::Level::Warn  => "🟡",
                log::Level::Info  => "  ",
                _                 => "  ",
            };
            let line = format!("{} {}", prefix, record.args());
            self.tx.try_send(line).ok();
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// Initialise the logger.  Returns a `Receiver` that yields new log lines as
/// they are produced.  Must be called exactly once before any logging.
pub fn init() -> Receiver<String> {
    let (tx, rx) = sync_channel::<String>(1000);
    let inner = env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .build();
    let max_level = inner.filter();
    log::set_boxed_logger(Box::new(DiagLogger { inner, tx }))
        .expect("logger already initialised");
    log::set_max_level(max_level);
    rx
}
