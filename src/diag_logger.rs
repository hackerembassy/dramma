use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

/// (level, message): level 0 = info · 1 = warn · 2 = error
pub type LogLine = (u8, String);

/// A logger that writes to stderr (via env_logger) and also sends each line
/// to an in-memory channel for the diagnostics page.
struct DiagLogger {
    inner: env_logger::Logger,
    tx: SyncSender<LogLine>,
}

impl log::Log for DiagLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        self.inner.log(record);
        if self.enabled(record.metadata()) {
            let level: u8 = match record.level() {
                log::Level::Error => 2,
                log::Level::Warn => 1,
                _ => 0,
            };
            self.tx
                .try_send((level, record.args().to_string()))
                .ok();
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// Initialise the logger.  Returns a `Receiver` that yields `(level, text)`
/// pairs as they are produced.  Must be called exactly once before any logging.
pub fn init() -> Receiver<LogLine> {
    let (tx, rx) = sync_channel::<LogLine>(1000);
    let inner = env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .build();
    let max_level = inner.filter();
    log::set_boxed_logger(Box::new(DiagLogger { inner, tx }))
        .expect("logger already initialised");
    log::set_max_level(max_level);
    rx
}
